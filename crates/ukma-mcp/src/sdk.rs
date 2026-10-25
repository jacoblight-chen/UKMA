//! Type-checked client SDK over the public API (§31.2, P8-WP4).
//!
//! The SDK is *schema driven*: a call names an operation and supplies the request
//! type the registry declares for it, and the response is decoded into the type the
//! registry declares back. Nothing here re-declares an operation, a scope or an
//! error code — [`crate::manifest`] stays the single projection of the registry.
//!
//! The transport is a port: [`InProcess`] talks to a [`crate::Gateway`] directly,
//! a future HTTP or stdio transport implements the same trait without changing a
//! single call site.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use ukma_contracts::{RequestContext, RequestEnvelope, ResponseEnvelope, ResponseStatus};

use crate::gateway::Gateway;

/// An error envelope as the client sees it.
///
/// The code is kept as the wire token: the client never needs the closed-set
/// constant type, and the gateway stays the only place that can emit a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// Code from the §31.5 error registry.
    pub code: String,
    /// Message that is safe to show a user.
    pub safe_message: String,
    /// Whether the call may be retried.
    pub retryable: bool,
    /// Suggested retry delay.
    pub retry_after_ms: Option<u64>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.safe_message)
    }
}

/// Everything that can go wrong for a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The transport failed before an envelope came back.
    Transport(String),
    /// The request could not be serialised.
    Encode(String),
    /// The response could not be decoded into the declared type.
    Decode(String),
    /// The gateway answered with an error envelope.
    Api(ApiError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Transport(detail) => write!(f, "transport: {detail}"),
            ClientError::Encode(detail) => write!(f, "encode: {detail}"),
            ClientError::Decode(detail) => write!(f, "decode: {detail}"),
            ClientError::Api(error) => write!(f, "api: {error}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Where envelopes are sent.
pub trait Transport: Send + Sync {
    /// Send one request envelope and return the raw response envelope.
    fn execute(&self, request: Value) -> Result<Value, ClientError>;
}

/// Transport that calls a gateway in the same process.
#[derive(Clone)]
pub struct InProcess {
    gateway: Arc<Gateway>,
}

impl InProcess {
    /// Wrap a gateway.
    pub fn new(gateway: Gateway) -> Self {
        Self {
            gateway: Arc::new(gateway),
        }
    }

    /// The wrapped gateway.
    pub fn gateway(&self) -> &Gateway {
        &self.gateway
    }
}

impl Transport for InProcess {
    fn execute(&self, request: Value) -> Result<Value, ClientError> {
        let envelope: RequestEnvelope<Value> = serde_json::from_value(request)
            .map_err(|error| ClientError::Encode(error.to_string()))?;
        let response = self.gateway.dispatch(envelope);
        serde_json::to_value(response).map_err(|error| ClientError::Encode(error.to_string()))
    }
}

/// A typed client over one transport.
pub struct Client<T: Transport> {
    transport: T,
    api_version: String,
}

impl<T: Transport> Client<T> {
    /// Build a client.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            api_version: "1.2.3".into(),
        }
    }

    /// The transport in use.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Call an operation with a raw JSON body.
    pub fn invoke(
        &self,
        operation: &str,
        context: &RequestContext,
        input: Value,
    ) -> Result<ResponseEnvelope<Value>, ClientError> {
        let envelope = RequestEnvelope {
            api_version: self.api_version.clone(),
            operation: operation.to_string(),
            context: context.clone(),
            input,
        };
        let request = serde_json::to_value(&envelope)
            .map_err(|error| ClientError::Encode(error.to_string()))?;
        let response = self.transport.execute(request)?;
        let decoded: ResponseEnvelope<Value> = serde_json::from_value(response)
            .map_err(|error| ClientError::Decode(error.to_string()))?;
        match decoded.status {
            ResponseStatus::Error => Err(ClientError::Api(ApiError {
                code: decoded
                    .error
                    .as_ref()
                    .map(|error| error.code.clone())
                    .unwrap_or_else(|| crate::error::INTERNAL.to_string()),
                safe_message: decoded
                    .error
                    .as_ref()
                    .map(|error| error.safe_message.clone())
                    .unwrap_or_default(),
                retryable: decoded.error.as_ref().is_some_and(|error| error.retryable),
                retry_after_ms: decoded
                    .error
                    .as_ref()
                    .and_then(|error| error.retry_after_ms),
            })),
            _ => Ok(decoded),
        }
    }

    /// Call an operation and decode `data` into the declared response type.
    pub fn typed<Q: Serialize, R: DeserializeOwned>(
        &self,
        operation: &str,
        context: &RequestContext,
        request: &Q,
    ) -> Result<R, ClientError> {
        let input = serde_json::to_value(request)
            .map_err(|error| ClientError::Encode(error.to_string()))?;
        let response = self.invoke(operation, context, input)?;
        let data = response.data.ok_or_else(|| {
            ClientError::Decode(format!("{operation} returned no data for a typed call"))
        })?;
        serde_json::from_value(data).map_err(|error| ClientError::Decode(error.to_string()))
    }

    /// `system.capabilities`.
    pub fn capabilities(&self, context: &RequestContext) -> Result<Value, ClientError> {
        Ok(self
            .invoke("system.capabilities", context, Value::Null)?
            .data
            .unwrap_or(Value::Null))
    }

    /// `system.get_operation`.
    pub fn describe(
        &self,
        context: &RequestContext,
        operation_id: &str,
    ) -> Result<Value, ClientError> {
        let response = self.invoke(
            "system.get_operation",
            context,
            serde_json::json!({ "operation_id": operation_id }),
        )?;
        Ok(response.data.unwrap_or(Value::Null))
    }

    /// `system.get_job`.
    pub fn job(&self, context: &RequestContext, job_id: &str) -> Result<Value, ClientError> {
        let response = self.invoke(
            "system.get_job",
            context,
            serde_json::json!({ "job_id": job_id }),
        )?;
        Ok(response.data.unwrap_or(Value::Null))
    }

    /// `system.cancel_job`.
    pub fn cancel_job(&self, context: &RequestContext, job_id: &str) -> Result<Value, ClientError> {
        let response = self.invoke(
            "system.cancel_job",
            context,
            serde_json::json!({ "job_id": job_id }),
        )?;
        Ok(response.data.unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::default_gateway;
    use ukma_contracts::{Consistency, ScopeRef};

    fn context() -> RequestContext {
        RequestContext {
            request_id: "req-sdk".into(),
            trace_id: "trace-sdk".into(),
            scope: ScopeRef {
                tenant_id: "tenant-a".into(),
                project_id: "project-b".into(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: "principal".into(),
            scopes: vec!["system:read".into(), "system:write".into()],
            deadline_at: None,
            consistency: Consistency::Eventual,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: None,
            expected_etag: None,
            delegation_token: None,
        }
    }

    #[test]
    fn the_in_process_transport_reaches_the_gateway() {
        let client = Client::new(InProcess::new(default_gateway()));
        let capabilities = client.capabilities(&context()).unwrap();
        assert_eq!(
            capabilities["domain_operations"],
            crate::manifest::DOMAIN_OPERATION_COUNT
        );
        assert_eq!(capabilities["system_operations"], 4);
    }

    #[test]
    fn system_helpers_decode_the_registry_view() {
        let client = Client::new(InProcess::new(default_gateway()));
        let described = client.describe(&context(), "memory.store_episode").unwrap();
        assert_eq!(described["operation_id"], "memory.store_episode");
        assert_eq!(described["mcp_tool"], "ukma_memory_store_episode_v1");
    }

    #[test]
    fn an_error_envelope_becomes_a_client_error() {
        let client = Client::new(InProcess::new(default_gateway()));
        let error = client.job(&context(), "missing").unwrap_err();
        match error {
            ClientError::Api(api) => assert_eq!(api.code, "JOB_NOT_FOUND"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(client
            .describe(&context(), "nope.nope")
            .unwrap_err()
            .to_string()
            .contains("NOT_FOUND"));
    }

    #[test]
    fn a_typed_call_uses_the_declared_types() {
        let client = Client::new(InProcess::new(default_gateway()));
        #[derive(serde::Deserialize)]
        struct Described {
            operation_id: String,
            api_major: u32,
        }
        let described: Described = client
            .typed(
                "system.get_operation",
                &context(),
                &serde_json::json!({ "operation_id": "system.capabilities" }),
            )
            .unwrap();
        assert_eq!(described.operation_id, "system.capabilities");
        assert_eq!(described.api_major, 1);
    }
}
