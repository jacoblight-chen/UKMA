//! UKMA server: the HTTP surface over the gateway (§31.2, P8-WP4).
//!
//! The server owns no semantics of its own: it resolves a path to a route with
//! [`ukma_mcp::rest::resolve`], dispatches through the gateway and renders the
//! response envelope. Tenant scope, authorization, idempotency and error codes all
//! come from the registry, so this file cannot drift from the contract.

use std::collections::BTreeMap;

use serde_json::Value;
use ukma_contracts::generated::OPERATIONS;
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ResponseStatus, ScopeRef};
use ukma_mcp::rest::{self, Route};
use ukma_mcp::{default_gateway, manifest_json, Gateway};

/// Largest body the server accepts, matching the gateway limit.
pub const MAX_BODY_BYTES: usize = ukma_mcp::gateway::MAX_INPUT_BYTES;

/// Envelope version the server speaks.
pub const API_VERSION: &str = "1.2.3";

/// Header carrying the tenant for context-free routes (job status).
pub const TENANT_HEADER: &str = "x-ukma-tenant-id";
/// Header carrying the project.
pub const PROJECT_HEADER: &str = "x-ukma-project-id";
/// Header carrying the authenticated principal.
pub const PRINCIPAL_HEADER: &str = "x-ukma-principal-id";
/// Header carrying the scopes the client expects, comma separated.
pub const SCOPES_HEADER: &str = "x-ukma-scopes";
/// Header carrying the trace id.
pub const TRACE_HEADER: &str = "x-ukma-trace-id";

/// One request as the HTTP layer hands it over.
#[derive(Debug, Clone, Default)]
pub struct HttpRequest {
    /// Method, case insensitive.
    pub method: String,
    /// Path, optionally with a query string.
    pub path: String,
    /// Headers, names lower-cased.
    pub headers: BTreeMap<String, String>,
    /// Raw body.
    pub body: String,
}

impl HttpRequest {
    /// Build a request.
    pub fn new(method: &str, path: &str, body: &str) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            headers: BTreeMap::new(),
            body: body.into(),
        }
    }

    /// Add a header (name is lower-cased).
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .insert(name.to_ascii_lowercase(), value.to_string());
        self
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// One response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Content type.
    pub content_type: &'static str,
    /// Body.
    pub body: String,
}

impl HttpResponse {
    fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json",
            body,
        }
    }

    fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            serde_json::json!({ "error": { "code": code, "safe_message": message } }).to_string(),
        )
    }
}

/// HTTP status for a registry error code.
///
/// The spec closes the error-code set but does not fix an HTTP status per code, so
/// this mapping is a server choice: it only groups codes into the RFC 9110 classes
/// that match their meaning.
pub fn status_for(code: &str) -> u16 {
    match code {
        "INVALID_SCHEMA"
        | "INVALID_ARGUMENT"
        | "UNSUPPORTED_MEDIA_TYPE"
        | "EVIDENCE_REQUIRED"
        | "MODEL_PROFILE_MISMATCH"
        | "CONSISTENCY_UNSUPPORTED" => 400,
        "UNAUTHENTICATED" | "DELEGATION_EXPIRED" => 401,
        "FORBIDDEN" | "CROSS_SCOPE_DENIED" | "POLICY_DENIED" => 403,
        "NOT_FOUND" | "JOB_NOT_FOUND" | "ALREADY_DELETED" => 404,
        "ETAG_MISMATCH"
        | "SOURCE_REVISION_MISMATCH"
        | "IDEMPOTENCY_CONFLICT"
        | "LINK_STATE_CONFLICT"
        | "CONFLICTING_EVIDENCE"
        | "DELETION_BLOCKED"
        | "JOB_NOT_CANCELLABLE" => 409,
        "CURSOR_EXPIRED" => 410,
        "SOURCE_TOO_LARGE" => 413,
        "INSUFFICIENT_CONTEXT_BUDGET" | "CONTEXT_BUDGET_EXCEEDED" | "RESOURCE_LIMIT" => 422,
        "CAPABILITY_UNAVAILABLE"
        | "INDEX_NOT_READY"
        | "INDEX_STALE"
        | "MODEL_UNAVAILABLE"
        | "STORAGE_UNAVAILABLE"
        | "SOURCE_UNAVAILABLE" => 503,
        "TIMEOUT" => 504,
        _ => 500,
    }
}

/// The gateway plus its HTTP projection.
pub struct Server {
    gateway: Gateway,
}

impl Server {
    /// Serve a gateway.
    pub fn new(gateway: Gateway) -> Self {
        Self { gateway }
    }

    /// Serve the in-memory development gateway.
    pub fn in_memory() -> Self {
        Self::new(default_gateway())
    }

    /// The gateway behind the server.
    pub fn gateway(&self) -> &Gateway {
        &self.gateway
    }

    /// Dispatch one HTTP request.
    pub fn handle(&self, request: HttpRequest) -> HttpResponse {
        if request.body.len() > MAX_BODY_BYTES {
            return HttpResponse::error(400, "INVALID_SCHEMA", "request body is too large");
        }
        match rest::resolve(&request.method, &request.path) {
            Route::Health => HttpResponse::json(200, rest::health_json()),
            Route::JobStatus { job_id } => self.job_status(&request, &job_id),
            Route::Operation { operation_id, .. } => {
                if !request.method.eq_ignore_ascii_case("POST") {
                    return HttpResponse::error(405, "INVALID_SCHEMA", "reads are POSTed");
                }
                self.dispatch(operation_id, &request.body)
            }
            Route::MethodNotAllowed { allowed } => HttpResponse::json(
                405,
                serde_json::json!({ "error": { "code": "INVALID_SCHEMA", "safe_message": "method not allowed", "allowed": allowed } })
                    .to_string(),
            ),
            Route::NotFound => {
                HttpResponse::error(404, "NOT_FOUND", "no route is registered at this path")
            }
        }
    }

    /// Render the tool manifest (`tool-manifest.json`).
    pub fn manifest(&self) -> String {
        manifest_json()
    }

    fn dispatch(&self, operation_id: &'static str, body: &str) -> HttpResponse {
        let envelope: RequestEnvelope<Value> = match serde_json::from_str(body) {
            Ok(envelope) => envelope,
            Err(error) => {
                return HttpResponse::error(
                    400,
                    "INVALID_SCHEMA",
                    &format!("request body is not an envelope: {error}"),
                )
            }
        };
        // The path and the body must name the same operation; a mismatch is a
        // client fault, never a silent redirect.
        if envelope.operation != operation_id {
            return HttpResponse::error(
                400,
                "INVALID_SCHEMA",
                &format!(
                    "body operation `{}` does not match route operation `{operation_id}`",
                    envelope.operation
                ),
            );
        }
        let response = self.gateway.dispatch(envelope);
        Self::render(response)
    }

    fn job_status(&self, request: &HttpRequest, job_id: &str) -> HttpResponse {
        let Some(tenant_id) = request.header(TENANT_HEADER) else {
            return HttpResponse::error(
                400,
                "INVALID_SCHEMA",
                "job status needs the x-ukma-tenant-id header",
            );
        };
        let Some(project_id) = request.header(PROJECT_HEADER) else {
            return HttpResponse::error(
                400,
                "INVALID_SCHEMA",
                "job status needs the x-ukma-project-id header",
            );
        };
        let principal_id = request.header(PRINCIPAL_HEADER).unwrap_or("anonymous");
        let scopes: Vec<String> = request
            .header(SCOPES_HEADER)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|scope| !scope.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let context = RequestContext {
            request_id: format!("http-job-{job_id}"),
            trace_id: request
                .header(TRACE_HEADER)
                .unwrap_or("trace-http")
                .to_string(),
            scope: ScopeRef {
                tenant_id: tenant_id.to_string(),
                project_id: project_id.to_string(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: principal_id.to_string(),
            scopes,
            deadline_at: None,
            consistency: Consistency::Eventual,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: None,
            expected_etag: None,
            delegation_token: None,
        };
        let response = self.gateway.dispatch(RequestEnvelope {
            api_version: API_VERSION.to_string(),
            operation: "system.get_job".to_string(),
            context,
            input: serde_json::json!({ "job_id": job_id }),
        });
        Self::render(response)
    }

    fn render(response: ukma_contracts::ResponseEnvelope<Value>) -> HttpResponse {
        let status = match response.status {
            ResponseStatus::Ok | ResponseStatus::Partial => 200,
            ResponseStatus::Error => status_for(
                response
                    .error
                    .as_ref()
                    .map(|error| error.code.as_str())
                    .unwrap_or("INTERNAL"),
            ),
        };
        let body = serde_json::to_string(&response)
            .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"));
        HttpResponse::json(status, body)
    }
}

/// Operations the server exposes, in registry order.
pub fn exposed_operations() -> Vec<&'static str> {
    OPERATIONS.iter().map(|spec| spec.operation_id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registry_operation_has_a_route_with_a_status_mapping() {
        let server = Server::in_memory();
        for spec in OPERATIONS {
            let (_, path) = rest::route_of(spec.server_method).expect("route parses");
            let response = server.handle(HttpRequest::new("POST", path, "{}"));
            assert_eq!(
                response.status, 400,
                "{} accepted an empty body",
                spec.operation_id
            );
            for code in spec.error_codes {
                let status = status_for(code);
                assert!((400..=599).contains(&status), "{code} mapped to {status}");
            }
        }
    }

    #[test]
    fn health_and_unknown_routes_are_distinguished() {
        let server = Server::in_memory();
        let health = server.handle(HttpRequest::new("GET", "/api/v1/health", ""));
        assert_eq!(health.status, 200);
        assert!(health.body.contains("\"status\":\"ok\""));

        let missing = server.handle(HttpRequest::new("GET", "/api/v1/nope", ""));
        assert_eq!(missing.status, 404);

        let wrong_method = server.handle(HttpRequest::new("GET", "/api/v1/memory/search", ""));
        assert_eq!(wrong_method.status, 405);
    }
}
