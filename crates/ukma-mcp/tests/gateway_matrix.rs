//! Gateway contract matrix (§31, §36.4, §40.3, §40.7).
//!
//! Every test dispatches through the real gateway with the real handler set, so a
//! regression in the operation registry, the envelope rules or a domain service is
//! visible here.

use std::sync::Arc;

use serde_json::{json, Value};
use ukma_contracts::generated::OPERATIONS;
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ResponseStatus, ScopeRef};
use ukma_mcp::alias::{AliasTable, PINNED_UPSTREAM_COMMIT};
use ukma_mcp::gateway::StaticScopes;
use ukma_mcp::rest::route_of;
use ukma_mcp::{
    build, default_gateway, drift, manifest_json, resolve, rest, tool_by_name, Gateway, JobIndex,
    JobRecord, Route, Services,
};

const PRINCIPAL: &str = "principal";

fn all_scopes() -> Vec<String> {
    let mut scopes: Vec<String> = Vec::new();
    for spec in OPERATIONS {
        for scope in spec.required_scopes {
            if !scopes.iter().any(|existing| existing == scope) {
                scopes.push((*scope).to_string());
            }
        }
    }
    scopes.sort();
    scopes
}

fn grant(gateway: Gateway, scopes: &[String]) -> Gateway {
    let borrowed: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let resolver = StaticScopes::new().grant(PRINCIPAL, &borrowed);
    gateway.with_scope_resolver(Arc::new(resolver))
}

fn full_authority() -> Gateway {
    grant(default_gateway(), &all_scopes())
}

fn envelope(operation: &str, tenant: &str, project: &str, input: Value) -> RequestEnvelope<Value> {
    RequestEnvelope {
        api_version: "1.2.3".into(),
        operation: operation.into(),
        context: RequestContext {
            request_id: format!("req-{operation}"),
            trace_id: "trace-matrix".into(),
            scope: ScopeRef {
                tenant_id: tenant.into(),
                project_id: project.into(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: PRINCIPAL.into(),
            scopes: all_scopes(),
            deadline_at: None,
            consistency: Consistency::Eventual,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: Some(format!("idem-{operation}")),
            expected_etag: Some("\"etag-1\"".into()),
            delegation_token: None,
        },
        input,
    }
}

fn call(gateway: &Gateway, operation: &str, input: Value) -> Value {
    let response = gateway.dispatch(envelope(operation, "tenant-a", "project-b", input));
    serde_json::to_value(response).expect("response serialises")
}

fn data_of(gateway: &Gateway, operation: &str, input: Value) -> Value {
    let response = call(gateway, operation, input);
    assert_eq!(
        response["status"], "ok",
        "{operation} should succeed, got {response}"
    );
    response["data"].clone()
}

fn code_of(gateway: &Gateway, operation: &str, input: Value) -> String {
    let response = call(gateway, operation, input);
    assert_eq!(
        response["status"], "error",
        "{operation} should fail, got {response}"
    );
    response["error"]["code"]
        .as_str()
        .expect("an error carries a code")
        .to_string()
}

fn episode_input(session: &str, summary: &str) -> Value {
    json!({
        "input": {
            "session_id": session,
            "agent_id": "agent-1",
            "agent_depth": 0,
            "episode_type": "conversation_turn",
            "summary": summary,
            "full_text_ref": null,
            "key_facts": ["fact"],
            "outcome": "success",
            "source_refs": ["artifact:sha256:abc"],
            "importance_score": 0.5,
            "language": "en",
            "evidence": [],
        }
    })
}

fn retrieve_input(memory_id: &str, revision: &str) -> Value {
    json!({
        "memory": { "id": memory_id, "kind": "episodic", "revision": revision },
        "detail": "summary",
        "max_tokens": 512,
    })
}

fn route_path(server_method: &str) -> String {
    server_method
        .split_once(' ')
        .map(|(_, path)| path.to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Registry projection (§36.4 / MCP-001 / PROP-10)
// ---------------------------------------------------------------------------

#[test]
fn the_manifest_is_projected_from_the_registry_without_drift() {
    assert_eq!(drift(), Vec::<String>::new());

    let manifest: Value = serde_json::from_str(&manifest_json()).expect("manifest is JSON");
    let tools = manifest["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), OPERATIONS.len());
    assert_eq!(
        manifest["domain_operations"],
        ukma_mcp::manifest::DOMAIN_OPERATION_COUNT as u64
    );
    assert_eq!(manifest["system_operations"], 4);

    for spec in OPERATIONS {
        let tool = tool_by_name(spec.mcp_tool).expect("every operation has a tool");
        assert_eq!(tool.operation_id, spec.operation_id);
        assert!(tool.name.starts_with("ukma_"));
        assert!(route_of(tool.rest).is_some(), "{} has a route", tool.name);
    }
}

#[test]
fn every_operation_is_dispatched_and_returns_only_declared_codes() {
    let gateway = full_authority();
    assert_eq!(gateway.unhandled(), Vec::<&str>::new());
    assert_eq!(gateway.registered().len(), OPERATIONS.len());

    for spec in OPERATIONS {
        let response = call(&gateway, spec.operation_id, json!({}));
        let status = response["status"].as_str().unwrap_or_default();
        if status == "ok" || status == "partial" {
            continue;
        }
        assert_eq!(status, "error", "{} returned {response}", spec.operation_id);
        let code = response["error"]["code"].as_str().unwrap_or_default();
        // Envelope-level rejections are generic §31.4 rules; a domain failure
        // must be a code the operation declares.
        if code == "INVALID_SCHEMA" {
            continue;
        }
        assert!(
            spec.error_codes.contains(&code),
            "{} returned undeclared code {code}",
            spec.operation_id
        );
        assert_ne!(code, "CAPABILITY_UNAVAILABLE", "{}", spec.operation_id);
    }
}

#[test]
fn the_api_version_and_operation_name_are_validated() {
    let gateway = full_authority();

    let mut request = envelope("memory.search", "tenant-a", "project-b", json!({}));
    request.api_version = "2.0.0".into();
    let response = gateway.dispatch(request);
    assert_eq!(response.status, ResponseStatus::Error);
    assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");

    let response = gateway.dispatch(envelope("memory.nope", "tenant-a", "project-b", json!({})));
    assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
}

// ---------------------------------------------------------------------------
// Authorization (§31.4)
// ---------------------------------------------------------------------------

#[test]
fn authorization_is_deny_by_default_and_client_scopes_are_expectations() {
    let denied = grant(default_gateway(), &[]);
    let response = denied.dispatch(envelope(
        "memory.search",
        "tenant-a",
        "project-b",
        json!({}),
    ));
    assert_eq!(response.error.unwrap().code, "FORBIDDEN");

    // Holding the declared scope is enough; an expectation the principal does not
    // hold is a warning, not a failure.
    let partial = grant(default_gateway(), &["memory:read".to_string()]);
    let mut request = envelope("memory.search", "tenant-a", "project-b", json!({}));
    request.context.scopes = vec!["memory:read".into(), "memory:write".into()];
    let response = partial.dispatch(request);
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("memory:write")),
        "warnings: {:?}",
        response.warnings
    );
}

#[test]
fn a_forged_envelope_scope_does_not_grant_authority() {
    let gateway = grant(default_gateway(), &["memory:read".to_string()]);
    let mut request = envelope("memory.store_episode", "tenant-a", "project-b", json!({}));
    // The client claims the write scope; the resolver granted only read.
    request.context.scopes = vec!["memory:write".into()];
    let response = gateway.dispatch(request);
    assert_eq!(response.error.unwrap().code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// Domain round trip and tenant isolation (§40.7)
// ---------------------------------------------------------------------------

#[test]
fn a_stored_episode_is_readable_only_inside_its_scope() {
    let gateway = full_authority();
    let stored = data_of(
        &gateway,
        "memory.store_episode",
        episode_input("session-1", "the build failed on the parser"),
    );
    let memory_id = stored["resource_id"]
        .as_str()
        .expect("resource id")
        .to_string();
    let etag = stored["etag"].as_str().expect("etag").to_string();
    assert!(memory_id.starts_with("mem:tenant-a:project-b:"));

    // Episodic memory is sandbox scoped: the reader must be the same agent in the
    // same session (§26.4).
    let mut request = envelope(
        "memory.retrieve",
        "tenant-a",
        "project-b",
        retrieve_input(&memory_id, &etag),
    );
    request.context.scope.session_id = Some("session-1".into());
    request.context.scope.agent_id = Some("agent-1".into());
    let response = gateway.dispatch(request);
    assert_eq!(response.status, ResponseStatus::Ok);
    let view = serde_json::to_value(response).expect("serialises");
    assert_eq!(view["data"]["reference"]["id"], memory_id.as_str());

    // Another tenant sees the same id as absent, never as forbidden (§40.7).
    let mut request = envelope(
        "memory.retrieve",
        "tenant-z",
        "project-b",
        retrieve_input(&memory_id, &etag),
    );
    request.context.scope.session_id = Some("session-1".into());
    request.context.scope.agent_id = Some("agent-1".into());
    let error = gateway
        .dispatch(request)
        .error
        .expect("cross-tenant read fails");
    assert_eq!(error.code, "NOT_FOUND");
}

#[test]
fn a_foreign_scope_write_is_denied_without_leaking_existence() {
    let gateway = full_authority();
    let mut request = envelope(
        "memory.store_episode",
        "tenant-a",
        "project-b",
        episode_input("session-9", "wrong session"),
    );
    request.context.scope.session_id = Some("session-other".into());
    let response = gateway.dispatch(request);
    let error = response.error.expect("cross-scope write fails");
    // `memory.store_episode` does not declare NOT_FOUND, so the gateway publishes
    // FORBIDDEN rather than an undeclared code.
    assert_eq!(error.code, "FORBIDDEN");
    assert!(response
        .warnings
        .iter()
        .any(|warning| warning.contains("NOT_FOUND")));
}

#[test]
fn an_idempotent_replay_of_store_episode_returns_the_same_resource() {
    let gateway = full_authority();
    let input = episode_input("session-2", "idempotent write");
    let first = data_of(&gateway, "memory.store_episode", input.clone());
    let second = data_of(&gateway, "memory.store_episode", input);
    assert_eq!(first["resource_id"], second["resource_id"]);
    assert_eq!(second["changed"], false);
}

// ---------------------------------------------------------------------------
// Envelope preconditions (§31.4)
// ---------------------------------------------------------------------------

#[test]
fn idempotency_keys_are_required_where_the_registry_declares_them() {
    let gateway = full_authority();
    let mut request = envelope(
        "memory.store_episode",
        "tenant-a",
        "project-b",
        episode_input("session-3", "no key"),
    );
    request.context.idempotency_key = None;
    let response = gateway.dispatch(request);
    assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
}

#[test]
fn expected_etag_is_required_where_the_registry_declares_it() {
    let gateway = full_authority();
    let mut request = envelope(
        "memory.update_revision",
        "tenant-a",
        "project-b",
        json!({
            "memory": { "id": "mem:tenant-a:project-b:1", "kind": "episodic", "revision": "r1" },
            "patch": { "summary": "x" },
        }),
    );
    request.context.expected_etag = None;
    let response = gateway.dispatch(request);
    assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
}

#[test]
fn deadlines_and_input_limits_are_enforced_before_the_handler() {
    let gateway = full_authority();
    let mut request = envelope("memory.get_budget", "tenant-a", "project-b", json!({}));
    request.context.deadline_at = Some("1999-01-01T00:00:00Z".into());
    let response = gateway.dispatch(request);
    assert_eq!(response.error.unwrap().code, "TIMEOUT");

    let mut request = envelope("memory.get_budget", "tenant-a", "project-b", json!({}));
    request.context.deadline_at = Some("not-a-time".into());
    assert_eq!(
        gateway.dispatch(request).error.unwrap().code,
        "INVALID_SCHEMA"
    );

    let tiny = build(&Services::in_memory())
        .expect("handlers register")
        .with_max_input_bytes(1);
    let response = grant(tiny, &all_scopes()).dispatch(envelope(
        "memory.get_budget",
        "tenant-a",
        "project-b",
        json!({}),
    ));
    assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
}

#[test]
fn snapshot_reads_must_name_a_generation() {
    let gateway = full_authority();
    let mut request = envelope("memory.search", "tenant-a", "project-b", json!({}));
    request.context.consistency = Consistency::Snapshot;
    let response = gateway.dispatch(request);
    assert_eq!(response.error.unwrap().code, "INVALID_SCHEMA");
}

// ---------------------------------------------------------------------------
// System operations (SYS-01..04)
// ---------------------------------------------------------------------------

#[test]
fn the_system_operations_describe_the_registry() {
    let gateway = default_gateway();
    let capabilities = data_of(&gateway, "system.capabilities", json!({}));
    assert_eq!(capabilities["spec_version"], "1.2.3");
    assert_eq!(capabilities["domain_operations"], 108);
    assert_eq!(capabilities["system_operations"], 4);
    assert_eq!(capabilities["domains"]["code"], 27);
    assert_eq!(capabilities["domains"]["document"], 16);
    assert_eq!(capabilities["domains"]["memory"], 25);
    assert_eq!(capabilities["domains"]["traceability"], 13);
    assert_eq!(capabilities["domains"]["pro"], 27);

    let described = data_of(
        &gateway,
        "system.get_operation",
        json!({ "operation_id": "traceability.propose_link" }),
    );
    assert_eq!(described["mcp_tool"], "ukma_traceability_propose_link_v1");
    assert_eq!(described["rest"], "POST /api/v1/traceability/propose_link");

    assert_eq!(
        code_of(
            &gateway,
            "system.get_operation",
            json!({ "operation_id": "nope.nope" })
        ),
        "NOT_FOUND"
    );
    assert_eq!(
        code_of(&gateway, "system.get_job", json!({ "job_id": "job-x" })),
        "JOB_NOT_FOUND"
    );
}

#[test]
fn a_job_is_visible_only_inside_its_tenant_and_project() {
    let services = Services::in_memory();
    services.jobs.record(JobRecord {
        job_id: "job-1".into(),
        tenant_id: "tenant-a".into(),
        project_id: "project-b".into(),
        kind: "document.ingest".into(),
        stage: "ready".into(),
        canonical_committed: true,
        cancellable: true,
        cancelled: false,
        diagnostics: vec![],
    });
    let gateway = build(&services).expect("handlers register");

    let visible = data_of(&gateway, "system.get_job", json!({ "job_id": "job-1" }));
    assert_eq!(visible["stage"], "ready");

    let foreign = gateway.dispatch(envelope(
        "system.get_job",
        "tenant-z",
        "project-b",
        json!({ "job_id": "job-1" }),
    ));
    assert_eq!(foreign.error.unwrap().code, "JOB_NOT_FOUND");

    let cancelled = data_of(&gateway, "system.cancel_job", json!({ "job_id": "job-1" }));
    assert_eq!(cancelled["stage"], "cancelled");
    assert_eq!(cancelled["canonical_committed"], true);
    assert_eq!(cancelled["cancellable"], false);
    // Cancelling twice is idempotent, not an error.
    let again = data_of(&gateway, "system.cancel_job", json!({ "job_id": "job-1" }));
    assert_eq!(again["stage"], "cancelled");

    services.jobs.record(JobRecord {
        job_id: "job-done".into(),
        tenant_id: "tenant-a".into(),
        project_id: "project-b".into(),
        kind: "code.analysis".into(),
        stage: "committed".into(),
        canonical_committed: true,
        cancellable: false,
        cancelled: false,
        diagnostics: vec![],
    });
    assert_eq!(
        code_of(
            &gateway,
            "system.cancel_job",
            json!({ "job_id": "job-done" })
        ),
        "JOB_NOT_CANCELLABLE"
    );
}

#[test]
fn the_job_index_isolates_tenants() {
    let index = JobIndex::new();
    index.record(JobRecord {
        job_id: "job-2".into(),
        tenant_id: "tenant-a".into(),
        project_id: "project-b".into(),
        kind: "code.analysis".into(),
        stage: "running".into(),
        canonical_committed: false,
        cancellable: true,
        cancelled: false,
        diagnostics: vec![],
    });
    assert!(index.get("tenant-z", "project-b", "job-2").is_none());
    assert!(index.get("tenant-a", "project-z", "job-2").is_none());
    assert!(index.get("tenant-a", "project-b", "job-2").is_some());
}

// ---------------------------------------------------------------------------
// Error normalisation (§31.4)
// ---------------------------------------------------------------------------

#[test]
fn an_undeclared_domain_code_is_never_published() {
    let gateway = default_gateway();
    let spec = OPERATIONS
        .iter()
        .find(|spec| spec.operation_id == "memory.retrieve")
        .expect("memory.retrieve is registered");
    let code = code_of(
        &gateway,
        "memory.retrieve",
        retrieve_input("mem:missing", "r1"),
    );
    assert!(
        spec.error_codes.contains(&code.as_str()) || code == "INVALID_SCHEMA",
        "memory.retrieve published {code}"
    );
}

// ---------------------------------------------------------------------------
// REST projection (§31.2)
// ---------------------------------------------------------------------------

#[test]
fn rest_routes_are_projected_and_reads_are_posted() {
    for spec in OPERATIONS {
        let tool = tool_by_name(spec.mcp_tool).expect("tool");
        let (method, path) = route_of(tool.rest).expect("route parses");
        assert_eq!(method, "POST");
        match resolve("POST", path) {
            Route::Operation { operation_id, .. } => {
                assert_eq!(operation_id, spec.operation_id)
            }
            other => panic!("{} resolved to {other:?}", spec.operation_id),
        }
        assert_eq!(
            resolve("GET", path),
            Route::MethodNotAllowed { allowed: "POST" }
        );
        assert_eq!(route_path(spec.server_method), path);
    }

    assert_eq!(resolve("GET", "/api/v1/health"), Route::Health);
    assert_eq!(
        resolve("POST", "/api/v1/health"),
        Route::MethodNotAllowed { allowed: "GET" }
    );
    assert_eq!(
        resolve("GET", "/api/v1/jobs/job-1"),
        Route::JobStatus {
            job_id: "job-1".into()
        }
    );
    assert_eq!(resolve("GET", "/api/v1/nope"), Route::NotFound);
}

#[test]
fn the_health_projection_reports_the_manifest() {
    let health: Value = serde_json::from_str(&rest::health_json()).expect("health is JSON");
    assert_eq!(health["spec_version"], "1.2.3");
    assert_eq!(health["status"], "ok");
    assert_eq!(
        health["operations"],
        ukma_contracts::generated::OPERATIONS.len() as u64
    );
}

// ---------------------------------------------------------------------------
// Legacy alias layer (§36.4) — inert by construction
// ---------------------------------------------------------------------------

#[test]
fn the_codegraph_alias_layer_is_inert_and_unclaimed() {
    let table = AliasTable::disabled();
    assert!(table.is_empty());
    assert!(!table.is_enabled());
    assert_eq!(table.pinned_commit(), PINNED_UPSTREAM_COMMIT);
    assert_eq!(PINNED_UPSTREAM_COMMIT.len(), 40);
    assert_eq!(table.resolve("codegraph_get_symbol"), None);

    let matrix: Value =
        serde_json::from_str(&table.difference_matrix_json()).expect("matrix is JSON");
    assert_eq!(matrix["enabled"], false);
    assert_eq!(matrix["alias_count"], 0);

    // Enabling without the pinned commit, or without a parity test, is refused.
    assert!(AliasTable::enable("0000000000000000000000000000000000000000", vec![]).is_err());
    assert!(AliasTable::enable(
        PINNED_UPSTREAM_COMMIT,
        vec![ukma_mcp::alias::AliasEntry {
            legacy_tool: "codegraph_get_symbol",
            operation_id: "code.get_symbol",
            parity_test: "",
        }],
    )
    .is_err());
    let enabled = AliasTable::enable(
        PINNED_UPSTREAM_COMMIT,
        vec![ukma_mcp::alias::AliasEntry {
            legacy_tool: "codegraph_get_symbol",
            operation_id: "code.get_symbol",
            parity_test: "tests/gateway_matrix.rs",
        }],
    )
    .expect("a parity-tested alias on the pinned commit is accepted");
    assert_eq!(
        enabled.resolve("codegraph_get_symbol"),
        Some("code.get_symbol")
    );
}
