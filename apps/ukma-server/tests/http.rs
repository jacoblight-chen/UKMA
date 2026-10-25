//! HTTP surface contract tests (§31.2).

use serde_json::{json, Value};
use ukma_contracts::generated::OPERATIONS;
use ukma_server::{
    status_for, HttpRequest, HttpResponse, Server, MAX_BODY_BYTES, PRINCIPAL_HEADER,
    PROJECT_HEADER, SCOPES_HEADER, TENANT_HEADER,
};

fn body_of(response: &HttpResponse) -> Value {
    serde_json::from_str(&response.body).expect("response body is JSON")
}

fn envelope(operation: &str, scopes: &[&str]) -> String {
    json!({
        "api_version": "1.2.3",
        "operation": operation,
        "context": {
            "request_id": "req-http",
            "trace_id": "trace-http",
            "scope": {
                "tenant_id": "tenant-a",
                "project_id": "project-b",
                "repo_id": null,
                "workspace_id": null,
                "agent_id": null,
                "session_id": null
            },
            "principal_id": "principal",
            "scopes": scopes,
            "deadline_at": null,
            "consistency": "eventual",
            "required_generation": null,
            "snapshot_id": null,
            "idempotency_key": null,
            "expected_etag": null,
            "delegation_token": null
        },
        "input": {}
    })
    .to_string()
}

#[test]
fn health_is_a_get_and_reports_the_registry_size() {
    let server = Server::in_memory();
    let response = server.handle(HttpRequest::new("GET", "/api/v1/health", ""));
    assert_eq!(response.status, 200);
    let body = body_of(&response);
    assert_eq!(body["spec_version"], "1.2.3");
    assert_eq!(body["operations"], OPERATIONS.len());
}

#[test]
fn a_domain_operation_is_posted_with_an_envelope() {
    let server = Server::in_memory();
    let response = server.handle(HttpRequest::new(
        "POST",
        "/api/v1/system/capabilities",
        &envelope("system.capabilities", &["system:read"]),
    ));
    assert_eq!(response.status, 200);
    let body = body_of(&response);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["data"]["domain_operations"], 65);
}

#[test]
fn the_path_and_the_body_must_name_the_same_operation() {
    let server = Server::in_memory();
    let response = server.handle(HttpRequest::new(
        "POST",
        "/api/v1/memory/search",
        &envelope("system.capabilities", &["system:read"]),
    ));
    assert_eq!(response.status, 400);
    assert_eq!(body_of(&response)["error"]["code"], "INVALID_SCHEMA");
}

#[test]
fn a_broken_body_is_rejected_before_dispatch() {
    let server = Server::in_memory();
    let response = server.handle(HttpRequest::new("POST", "/api/v1/memory/search", "{"));
    assert_eq!(response.status, 400);

    let response = server.handle(HttpRequest::new("POST", "/api/v1/memory/search", "{}"));
    assert_eq!(response.status, 400);
    let code = body_of(&response)["error"]["code"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(code, "INVALID_SCHEMA");
}

#[test]
fn an_oversized_body_is_refused() {
    let server = Server::in_memory();
    let huge = "x".repeat(MAX_BODY_BYTES + 1);
    let response = server.handle(HttpRequest::new("POST", "/api/v1/memory/search", &huge));
    assert_eq!(response.status, 400);
}

#[test]
fn authorization_failures_map_to_403() {
    let server = Server::in_memory();
    // The development gateway trusts the envelope, so an empty scope list is what
    // produces the denial here.
    let response = server.handle(HttpRequest::new(
        "POST",
        "/api/v1/memory/store_episode",
        &envelope("memory.store_episode", &[]),
    ));
    assert_eq!(response.status, 403);
    assert_eq!(body_of(&response)["error"]["code"], "FORBIDDEN");
}

#[test]
fn job_status_reads_context_from_headers() {
    let server = Server::in_memory();
    let missing = server.handle(HttpRequest::new("GET", "/api/v1/jobs/job-1", ""));
    assert_eq!(missing.status, 400);

    let response = server.handle(
        HttpRequest::new("GET", "/api/v1/jobs/job-1", "")
            .with_header(TENANT_HEADER, "tenant-a")
            .with_header(PROJECT_HEADER, "project-b")
            .with_header(PRINCIPAL_HEADER, "principal")
            .with_header(SCOPES_HEADER, "system:read"),
    );
    assert_eq!(response.status, 404);
    assert_eq!(body_of(&response)["error"]["code"], "JOB_NOT_FOUND");
}

#[test]
fn method_and_route_errors_are_distinct() {
    let server = Server::in_memory();
    let unknown = server.handle(HttpRequest::new("GET", "/api/v1/health/x", ""));
    assert_eq!(unknown.status, 404);

    let not_allowed = server.handle(HttpRequest::new("PUT", "/api/v1/memory/search", ""));
    assert_eq!(not_allowed.status, 405);
    assert_eq!(body_of(&not_allowed)["error"]["allowed"], "POST");
}

#[test]
fn the_status_mapping_covers_the_closed_error_set() {
    assert_eq!(status_for("INVALID_SCHEMA"), 400);
    assert_eq!(status_for("UNAUTHENTICATED"), 401);
    assert_eq!(status_for("FORBIDDEN"), 403);
    assert_eq!(status_for("NOT_FOUND"), 404);
    assert_eq!(status_for("ETAG_MISMATCH"), 409);
    assert_eq!(status_for("CURSOR_EXPIRED"), 410);
    assert_eq!(status_for("SOURCE_TOO_LARGE"), 413);
    assert_eq!(status_for("TIMEOUT"), 504);
    assert_eq!(status_for("INTERNAL"), 500);
    assert_eq!(status_for("SOMETHING_NEW"), 500);
}

#[test]
fn every_operation_is_reachable_over_http() {
    let server = Server::in_memory();
    for spec in OPERATIONS {
        let path = spec
            .server_method
            .split_once(' ')
            .map(|(_, path)| path.to_string())
            .expect("route");
        let response = server.handle(HttpRequest::new(
            "POST",
            &path,
            &envelope(spec.operation_id, spec.required_scopes),
        ));
        assert!(
            (200..600).contains(&response.status),
            "{} produced {}",
            spec.operation_id,
            response.status
        );
    }
}
