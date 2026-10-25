//! Registry-driven dispatch (§31.4).
//!
//! The gateway is the only place that sees an untrusted envelope: it validates
//! the version and operation, injects the trusted scope, enforces the declared
//! idempotency/concurrency/deadline/limit rules, and normalises the returned code
//! against the operation's declared error set.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use ukma_contracts::generated::{Concurrency, Idempotency, OperationSpec, OPERATIONS};
use ukma_contracts::{
    Consistency, ErrorDetail, Freshness, PartialReason, RequestEnvelope, ResponseEnvelope,
    ResponseStatus,
};

use crate::context::OpContext;
use crate::error::{self, GatewayError};

/// Largest request body the gateway accepts.
pub const MAX_INPUT_BYTES: usize = 1 << 20;

/// What a handler produced.
#[derive(Debug, Clone)]
pub struct GatewayOutcome {
    /// Response data.
    pub data: Value,
    /// Warnings surfaced in the envelope.
    pub warnings: Vec<String>,
    /// Reasons the answer is partial.
    pub partial_reasons: Vec<PartialReason>,
    /// Freshness of derived data.
    pub freshness: Freshness,
    /// Index generation observed.
    pub index_generation: Option<String>,
    /// Snapshot the answer was computed against.
    pub snapshot_id: Option<String>,
    /// Revision observed.
    pub observed_revision: Option<String>,
}

impl GatewayOutcome {
    /// A complete result.
    pub fn data(data: Value) -> Self {
        Self {
            data,
            warnings: Vec::new(),
            partial_reasons: Vec::new(),
            freshness: Freshness::Current,
            index_generation: None,
            snapshot_id: None,
            observed_revision: None,
        }
    }

    /// A result that is explicitly partial.
    pub fn partial(data: Value, source: &str, code: &str, detail: impl Into<String>) -> Self {
        Self {
            data,
            warnings: Vec::new(),
            partial_reasons: vec![PartialReason {
                source: source.to_string(),
                code: code.to_string(),
                detail: detail.into(),
            }],
            freshness: Freshness::Stale,
            index_generation: None,
            snapshot_id: None,
            observed_revision: None,
        }
    }

    /// Add a warning.
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Set the observed index generation.
    pub fn with_generation(mut self, generation: impl Into<String>) -> Self {
        self.index_generation = Some(generation.into());
        self
    }
}

/// A registered operation handler.
pub type Handler =
    Arc<dyn Fn(&OpContext, &Value) -> Result<GatewayOutcome, GatewayError> + Send + Sync>;

/// Resolves the scopes a principal actually holds.
///
/// Authentication belongs to the host: the gateway never believes the scopes an
/// envelope carries, it asks the resolver. The default resolver trusts the
/// envelope and is documented as a development profile.
pub trait ScopeResolver: Send + Sync {
    /// Scopes granted to `principal_id` for this call.
    fn granted_scopes(&self, principal_id: &str, requested: &[String]) -> Vec<String>;
}

/// Development resolver: trusts the envelope's scopes.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvelopeScopes;

impl ScopeResolver for EnvelopeScopes {
    fn granted_scopes(&self, _principal_id: &str, requested: &[String]) -> Vec<String> {
        requested.to_vec()
    }
}

/// Resolver backed by a fixed principal -> scopes table.
#[derive(Debug, Clone, Default)]
pub struct StaticScopes {
    grants: BTreeMap<String, Vec<String>>,
}

impl StaticScopes {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant scopes to a principal.
    pub fn grant(mut self, principal_id: &str, scopes: &[&str]) -> Self {
        self.grants.insert(
            principal_id.to_string(),
            scopes.iter().map(|scope| (*scope).to_string()).collect(),
        );
        self
    }
}

impl ScopeResolver for StaticScopes {
    fn granted_scopes(&self, principal_id: &str, _requested: &[String]) -> Vec<String> {
        self.grants.get(principal_id).cloned().unwrap_or_default()
    }
}

/// Time source used for deadline checks.
pub trait Clock: Send + Sync {
    /// Current instant in Unix seconds.
    fn now(&self) -> i64;
}

/// Wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Clock pinned at an instant.
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: Arc<std::sync::Mutex<i64>>,
}

impl FixedClock {
    /// Clock pinned at an instant.
    pub fn new(seconds: i64) -> Self {
        Self {
            now: Arc::new(std::sync::Mutex::new(seconds)),
        }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        *self.now.lock().expect("clock poisoned")
    }
}

/// The dispatch core.
pub struct Gateway {
    handlers: BTreeMap<&'static str, Handler>,
    clock: Arc<dyn Clock>,
    scopes: Arc<dyn ScopeResolver>,
    max_input_bytes: usize,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("handlers", &self.handlers.len())
            .field("max_input_bytes", &self.max_input_bytes)
            .finish()
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}

fn spec_of(operation_id: &str) -> Option<&'static OperationSpec> {
    OPERATIONS
        .iter()
        .find(|spec| spec.operation_id == operation_id)
}

fn major_of(api_version: &str) -> Option<u32> {
    api_version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
}

fn parse_instant(text: &str) -> Option<i64> {
    let trimmed = text.trim_end_matches('Z');
    let mut parts = trimmed.split(['T', 't']);
    let date = parts.next()?;
    let time = parts.next()?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<i64>().ok()?;
    let minute = time_parts.next()?.parse::<i64>().ok()?;
    let second = time_parts.next()?.split('.').next()?.parse::<i64>().ok()?;
    Some(
        crate::instant::days_from_civil(year, month, day) * 86_400
            + hour * 3_600
            + minute * 60
            + second,
    )
}

impl Gateway {
    /// Gateway with no handlers registered yet.
    pub fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
            clock: Arc::new(SystemClock),
            scopes: Arc::new(EnvelopeScopes),
            max_input_bytes: MAX_INPUT_BYTES,
        }
    }

    /// Replace the clock.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Replace the scope resolver (the host's authentication decision).
    pub fn with_scope_resolver(mut self, scopes: Arc<dyn ScopeResolver>) -> Self {
        self.scopes = scopes;
        self
    }

    /// Replace the request size limit.
    pub fn with_max_input_bytes(mut self, bytes: usize) -> Self {
        self.max_input_bytes = bytes;
        self
    }

    /// Register a handler for a registered operation.
    pub fn register(&mut self, operation_id: &'static str, handler: Handler) -> Result<(), String> {
        if spec_of(operation_id).is_none() {
            return Err(format!("{operation_id} is not a registered operation"));
        }
        if self.handlers.insert(operation_id, handler).is_some() {
            return Err(format!("{operation_id} already has a handler"));
        }
        Ok(())
    }

    /// Whether an operation has a handler.
    pub fn is_registered(&self, operation_id: &str) -> bool {
        self.handlers.contains_key(operation_id)
    }

    /// Registered operation ids, sorted.
    pub fn registered(&self) -> Vec<&'static str> {
        self.handlers.keys().copied().collect()
    }

    /// Operations that the registry declares but this gateway cannot dispatch.
    pub fn unhandled(&self) -> Vec<&'static str> {
        OPERATIONS
            .iter()
            .map(|spec| spec.operation_id)
            .filter(|id| !self.handlers.contains_key(id))
            .collect()
    }

    /// Call one operation with an already-authenticated context.
    pub fn call(
        &self,
        operation_id: &str,
        ctx: &OpContext,
        input: &Value,
    ) -> Result<GatewayOutcome, GatewayError> {
        let spec = spec_of(operation_id).ok_or_else(|| {
            GatewayError::invalid_schema(format!("unknown operation `{operation_id}`"))
        })?;
        let handler = self.handlers.get(spec.operation_id).ok_or_else(|| {
            GatewayError::internal(format!("no handler registered for {operation_id}"))
        })?;
        self.verify(operation_id, spec, ctx, input)?;
        handler(ctx, input)
    }

    /// Dispatch an untrusted request envelope.
    pub fn dispatch(&self, request: RequestEnvelope<Value>) -> ResponseEnvelope<Value> {
        let request_id = request.context.request_id.clone();
        let trace_id = request.context.trace_id.clone();
        let operation = request.operation.clone();

        let mut warnings = Vec::new();
        let spec = match spec_of(&operation) {
            Some(spec) => spec,
            None => {
                return failure(
                    request_id,
                    trace_id,
                    GatewayError::invalid_schema(format!("unknown operation `{operation}`")),
                    warnings,
                )
            }
        };
        if major_of(&request.api_version) != Some(spec.api_major) {
            return failure(
                request_id,
                trace_id,
                GatewayError::invalid_schema(format!(
                    "api_version `{}` does not match major {}",
                    request.api_version, spec.api_major
                )),
                warnings,
            );
        }

        // The gateway asks the host what the principal holds; the envelope only
        // carries the scopes the client *expects*.
        let granted = self
            .scopes
            .granted_scopes(&request.context.principal_id, &request.context.scopes);
        let ctx = OpContext::from_request(&request.context, granted);
        if let Some(mismatch) = ctx.expectation_mismatch() {
            warnings.push(mismatch);
        }

        // Envelope/precondition checks are generic ?31.4 rules, not domain
        // outcomes, so their codes are returned as-is; only a handler result is
        // normalised against the operation's declared error set.
        if let Err(error) = self.verify(&operation, spec, &ctx, &request.input) {
            return failure(request_id, trace_id, error, warnings);
        }
        let result = match self.handlers.get(spec.operation_id) {
            Some(handler) => handler(&ctx, &request.input),
            None => Err(GatewayError::internal(format!(
                "no handler registered for {operation}"
            ))),
        };
        match result {
            Ok(outcome) => {
                warnings.extend(outcome.warnings);
                ResponseEnvelope {
                    api_version: request.api_version,
                    request_id,
                    trace_id,
                    status: if outcome.partial_reasons.is_empty() {
                        ResponseStatus::Ok
                    } else {
                        ResponseStatus::Partial
                    },
                    data: Some(outcome.data),
                    error: None,
                    warnings,
                    partial_reasons: outcome.partial_reasons,
                    snapshot_id: outcome.snapshot_id,
                    index_generation: outcome.index_generation,
                    freshness: outcome.freshness,
                    observed_revision: outcome.observed_revision,
                }
            }
            Err(error) => {
                let normalised = error::normalise(spec, error.code);
                if let Some(warning) = normalised.warning {
                    warnings.push(warning);
                }
                failure(
                    request_id,
                    trace_id,
                    GatewayError {
                        code: normalised.code,
                        ..error
                    },
                    warnings,
                )
            }
        }
    }

    fn verify(
        &self,
        operation_id: &str,
        spec: &OperationSpec,
        ctx: &OpContext,
        input: &Value,
    ) -> Result<(), GatewayError> {
        // Deny by default: every declared scope must be held.
        for scope in spec.required_scopes {
            ctx.require(&[scope])?;
        }
        if matches!(spec.idempotency, Idempotency::Required) && ctx.idempotency_key.is_none() {
            return Err(GatewayError::invalid_schema(
                "this operation requires an idempotency_key",
            ));
        }
        if matches!(spec.concurrency, Concurrency::ExpectedEtag) {
            let inline = input
                .get("expected_etag")
                .and_then(Value::as_str)
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false);
            if ctx.expected_etag.is_none() && !inline {
                return Err(GatewayError::invalid_schema(
                    "this operation requires an expected_etag",
                ));
            }
        }
        let serialised = input.to_string();
        if serialised.len() > self.max_input_bytes {
            return Err(GatewayError::invalid_schema(format!(
                "input is {} bytes, limit is {}",
                serialised.len(),
                self.max_input_bytes
            ))
            .with_detail(operation_id));
        }
        if let Some(deadline) = &ctx.deadline_at {
            let instant = parse_instant(deadline).ok_or_else(|| {
                GatewayError::invalid_schema("deadline_at is not an RFC 3339 instant")
            })?;
            if self.clock.now() > instant {
                return Err(
                    GatewayError::timeout().with_detail(format!("deadline {deadline} has passed"))
                );
            }
        }
        if matches!(ctx.consistency, Consistency::Snapshot) && ctx.required_generation.is_none() {
            // Snapshot reads must name a generation; never silently downgrade.
            return Err(GatewayError::invalid_schema(
                "consistency=snapshot requires required_generation",
            ));
        }
        Ok(())
    }
}

fn failure(
    request_id: String,
    trace_id: String,
    error: GatewayError,
    warnings: Vec<String>,
) -> ResponseEnvelope<Value> {
    ResponseEnvelope {
        api_version: "1.2.3".to_string(),
        request_id,
        trace_id,
        status: ResponseStatus::Error,
        data: None,
        error: Some(ErrorDetail {
            code: error.code.to_string(),
            safe_message: error.safe_message,
            retryable: error.retryable,
            retry_after_ms: error.retry_after_ms,
        }),
        warnings,
        partial_reasons: Vec::new(),
        snapshot_id: None,
        index_generation: None,
        freshness: Freshness::Unknown,
        observed_revision: None,
    }
}

/// Decode a handler input into a typed request.
pub fn decode<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, GatewayError> {
    serde_json::from_value(input.clone()).map_err(|error| {
        GatewayError::invalid_schema("input does not match the request schema")
            .with_detail(error.to_string())
    })
}

/// Encode a handler response.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<Value, GatewayError> {
    serde_json::to_value(value)
        .map_err(|error| GatewayError::internal(format!("response encoding failed: {error}")))
}

/// Run a typed call: decode, invoke, encode.
pub fn typed<I, O>(
    ctx: &OpContext,
    input: &Value,
    call: impl FnOnce(&ukma_contracts::RequestContext, I) -> Result<O, GatewayError>,
) -> Result<GatewayOutcome, GatewayError>
where
    I: serde::de::DeserializeOwned,
    O: serde::Serialize,
{
    let request: I = decode(input)?;
    let request_context = ctx.to_request_context();
    let response = call(&request_context, request)?;
    Ok(GatewayOutcome::data(encode(&response)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::OpContext;
    use ukma_contracts::{RequestContext, ScopeRef};

    fn request_context(operation: &str, scopes: Vec<String>) -> RequestEnvelope<Value> {
        RequestEnvelope {
            api_version: "1.2.3".into(),
            operation: operation.into(),
            context: RequestContext {
                request_id: "req-1".into(),
                trace_id: "trace-1".into(),
                scope: ScopeRef {
                    tenant_id: "tenant-a".into(),
                    project_id: "project-b".into(),
                    repo_id: None,
                    workspace_id: None,
                    agent_id: None,
                    session_id: None,
                },
                principal_id: "principal".into(),
                scopes,
                deadline_at: None,
                consistency: Consistency::Eventual,
                required_generation: None,
                snapshot_id: None,
                idempotency_key: None,
                expected_etag: None,
                delegation_token: None,
            },
            input: serde_json::json!({}),
        }
    }

    fn gateway_with(operation: &'static str, scopes: &[&str]) -> Gateway {
        let mut gateway = Gateway::new();
        gateway
            .register(
                operation,
                Arc::new(|_ctx: &OpContext, _input: &Value| {
                    Ok(GatewayOutcome::data(serde_json::json!({"ok": true})))
                }),
            )
            .unwrap();
        let grants = StaticScopes::new().grant("principal", scopes);
        gateway.with_scope_resolver(Arc::new(grants))
    }

    #[test]
    fn registration_rejects_unknown_operations_and_duplicates() {
        let mut gateway = Gateway::new();
        let handler: Handler =
            Arc::new(|_ctx, _input| Ok(GatewayOutcome::data(serde_json::json!({}))));
        assert!(gateway.register("nope.operation", handler.clone()).is_err());
        gateway
            .register("code.get_symbol", handler.clone())
            .unwrap();
        assert!(gateway.register("code.get_symbol", handler).is_err());
        assert!(gateway.is_registered("code.get_symbol"));
        assert_eq!(gateway.registered(), vec!["code.get_symbol"]);
        assert!(gateway.unhandled().contains(&"memory.search"));
    }

    #[test]
    fn dispatch_validates_the_api_version_and_operation() {
        let gateway = gateway_with("code.get_symbol", &["code:read"]);
        let mut request = request_context("code.get_symbol", vec!["code:read".into()]);
        request.api_version = "0.9.0".into();
        let response = gateway.dispatch(request);
        assert_eq!(response.status, ResponseStatus::Error);
        assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");

        let response = gateway.dispatch(request_context("code.nope", vec![]));
        assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
    }

    #[test]
    fn dispatch_denies_a_missing_scope_and_warns_about_expectations() {
        let denied = gateway_with("code.get_symbol", &[]);
        let response = denied.dispatch(request_context("code.get_symbol", vec![]));
        assert_eq!(response.error.unwrap().code, "FORBIDDEN");

        let gateway = gateway_with("code.get_symbol", &["code:read"]);
        let mut request = request_context(
            "code.get_symbol",
            vec!["code:read".into(), "code:write".into()],
        );
        request.context.request_id = "req-2".into();
        let response = gateway.dispatch(request);
        assert_eq!(response.status, ResponseStatus::Ok);
        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("code:write")));
    }

    #[test]
    fn dispatch_requires_idempotency_and_etag_where_declared() {
        let mut gateway = Gateway::new();
        gateway
            .register(
                "memory.store_episode",
                Arc::new(|_ctx, _input| Ok(GatewayOutcome::data(serde_json::json!({})))),
            )
            .unwrap();
        let response = gateway.dispatch(request_context(
            "memory.store_episode",
            vec!["memory:write".into()],
        ));
        assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");

        let mut request = request_context("memory.store_episode", vec!["memory:write".into()]);
        request.context.idempotency_key = Some("idem-1".into());
        assert_eq!(gateway.dispatch(request).status, ResponseStatus::Ok);

        let mut gateway = Gateway::new();
        gateway
            .register(
                "memory.update_revision",
                Arc::new(|_ctx, _input| Ok(GatewayOutcome::data(serde_json::json!({})))),
            )
            .unwrap();
        let response = gateway.dispatch(request_context(
            "memory.update_revision",
            vec!["memory:write".into()],
        ));
        assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");

        // An inline expected_etag satisfies the concurrency requirement.
        let mut request = request_context("memory.update_revision", vec!["memory:write".into()]);
        request.context.idempotency_key = Some("idem-2".into());
        request.input = serde_json::json!({"expected_etag": "\"abc\""});
        assert_eq!(gateway.dispatch(request).status, ResponseStatus::Ok);
    }

    #[test]
    fn dispatch_enforces_deadlines_and_input_limits() {
        let gateway = gateway_with("code.get_symbol", &["code:read"]).with_clock(Arc::new(
            FixedClock::new(parse_instant("2026-10-25T00:00:00Z").unwrap()),
        ));
        let mut request = request_context("code.get_symbol", vec!["code:read".into()]);
        request.context.deadline_at = Some("2026-10-24T00:00:00Z".into());
        assert_eq!(request.context.scopes.len(), 1);
        let response = gateway.dispatch(request);
        assert_eq!(response.error.unwrap().code, "TIMEOUT");

        let mut request = request_context("code.get_symbol", vec!["code:read".into()]);
        request.context.deadline_at = Some("not-a-time".into());
        assert_eq!(
            gateway.dispatch(request).error.unwrap().code,
            "INVALID_SCHEMA"
        );

        let small = gateway_with("code.get_symbol", &["code:read"]).with_max_input_bytes(1);
        let response = small.dispatch(request_context("code.get_symbol", vec!["code:read".into()]));
        assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
    }

    #[test]
    fn snapshot_consistency_without_a_generation_is_refused() {
        let gateway = gateway_with("code.get_symbol", &["code:read"]);
        let mut request = request_context("code.get_symbol", vec!["code:read".into()]);
        request.context.consistency = Consistency::Snapshot;
        assert_eq!(
            gateway.dispatch(request).error.unwrap().code,
            "INVALID_SCHEMA"
        );
    }

    #[test]
    fn an_undeclared_error_code_is_normalised_with_a_warning() {
        let mut gateway = Gateway::new();
        gateway
            .register(
                "code.get_symbol",
                Arc::new(|_ctx, _input| Err(GatewayError::new("MADE_UP_CODE", "boom", false))),
            )
            .unwrap();
        let response =
            gateway.dispatch(request_context("code.get_symbol", vec!["code:read".into()]));
        assert_eq!(response.error.as_ref().unwrap().code, "INTERNAL");
        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("MADE_UP_CODE")));
    }

    #[test]
    fn a_partial_outcome_sets_the_partial_status() {
        let mut gateway = Gateway::new();
        gateway
            .register(
                "code.get_symbol",
                Arc::new(|_ctx, _input| {
                    Ok(GatewayOutcome::partial(
                        serde_json::json!({"ok": true}),
                        "lexical",
                        "MODEL_UNAVAILABLE",
                        "no embedding model",
                    ))
                }),
            )
            .unwrap();
        let response =
            gateway.dispatch(request_context("code.get_symbol", vec!["code:read".into()]));
        assert_eq!(response.status, ResponseStatus::Partial);
        assert_eq!(response.partial_reasons.len(), 1);
        assert_eq!(response.freshness, Freshness::Stale);
    }

    #[test]
    fn typed_helpers_decode_and_encode() {
        let ctx = OpContext::from_request(
            &request_context("code.get_symbol", vec!["code:read".into()]).context,
            vec!["code:read".into()],
        );
        let outcome = typed(&ctx, &serde_json::json!(3), |_rc, input: u32| Ok(input * 2)).unwrap();
        assert_eq!(outcome.data, serde_json::json!(6));
        assert!(typed(&ctx, &serde_json::json!("x"), |_rc, input: u32| Ok(input)).is_err());
    }
}
