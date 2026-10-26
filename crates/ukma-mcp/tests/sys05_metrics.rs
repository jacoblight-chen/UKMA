//! SYS-05 `system.metrics` contract tests — Phase 8 gap closure.
//!
//! Counters live on the gateway and are shared by every dispatch path, so the
//! tests drive real envelopes through the real handler set.

use std::sync::Arc;

use serde_json::{json, Value};
use ukma_contracts::generated::OPERATIONS;
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ScopeRef};
use ukma_mcp::gateway::StaticScopes;
use ukma_mcp::{default_gateway, Gateway};

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

fn full_authority() -> Gateway {
    let scopes = all_scopes();
    let borrowed: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let resolver = StaticScopes::new().grant(PRINCIPAL, &borrowed);
    default_gateway().with_scope_resolver(Arc::new(resolver))
}

fn envelope(operation: &str, scopes: Vec<String>, input: Value) -> RequestEnvelope<Value> {
    RequestEnvelope {
        api_version: "1.2.3".into(),
        operation: operation.into(),
        context: RequestContext {
            request_id: format!("req-{operation}"),
            trace_id: "trace-sys05".into(),
            scope: ScopeRef {
                tenant_id: "tenant-a".into(),
                project_id: "project-b".into(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: PRINCIPAL.into(),
            scopes,
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

fn dispatch(gateway: &Gateway, operation: &str, input: Value) -> Value {
    let response = gateway.dispatch(envelope(operation, all_scopes(), input));
    serde_json::to_value(response).expect("response serialises")
}

fn data_of(gateway: &Gateway, operation: &str, input: Value) -> Value {
    let response = dispatch(gateway, operation, input);
    assert_eq!(
        response["status"], "ok",
        "{operation} should succeed, got {response}"
    );
    response["data"].clone()
}

fn metrics(gateway: &Gateway) -> Value {
    data_of(gateway, "system.metrics", json!({}))
}

#[test]
fn sys05_metrics_positive_counts_dispatches() {
    let gateway = full_authority();
    data_of(&gateway, "system.capabilities", json!({}));
    data_of(&gateway, "system.metrics", json!({}));

    let page = metrics(&gateway);
    // Self-observation: the in-flight metrics call is recorded only after it
    // returns, so this third call sees the two preceding dispatches.
    assert_eq!(page["total_success"].as_u64(), Some(2));
    assert_eq!(page["total_failure"].as_u64(), Some(0));
    let per_operation = page["per_operation"]
        .as_array()
        .expect("per-operation list");
    let capabilities = per_operation
        .iter()
        .find(|entry| entry["operation_id"] == "system.capabilities")
        .expect("capabilities counted");
    assert_eq!(capabilities["success"].as_u64(), Some(1));
    assert_eq!(capabilities["failure"].as_u64(), Some(0));
    // The earlier read of the metrics is counted; this one is not yet.
    let own = per_operation
        .iter()
        .find(|entry| entry["operation_id"] == "system.metrics")
        .expect("system.metrics counted");
    assert_eq!(own["success"].as_u64(), Some(1));
}

#[test]
fn sys05_metrics_counts_failures() {
    let gateway = full_authority();
    // An unknown operation fails validation and is counted as a failure.
    let response = dispatch(&gateway, "code.does_not_exist", json!({}));
    assert_eq!(response["status"], "error");

    let page = metrics(&gateway);
    assert!(page["total_failure"].as_u64().expect("failure count") >= 1);
    let per_operation = page["per_operation"]
        .as_array()
        .expect("per-operation list");
    let unknown = per_operation
        .iter()
        .find(|entry| entry["operation_id"] == "code.does_not_exist")
        .expect("the failed call is counted");
    assert_eq!(unknown["failure"].as_u64(), Some(1));
    assert_eq!(unknown["success"].as_u64(), Some(0));
}

#[test]
fn sys05_metrics_requires_the_system_read_scope() {
    let resolver = StaticScopes::new(); // nothing granted
    let gateway = default_gateway().with_scope_resolver(Arc::new(resolver));
    let response = gateway.dispatch(envelope(
        "system.metrics",
        vec!["system:read".to_string()],
        json!({}),
    ));
    let value = serde_json::to_value(response).expect("response serialises");
    assert_eq!(value["status"], "error");
    assert_eq!(
        value["error"]["code"].as_str(),
        Some("FORBIDDEN"),
        "a principal without system:read must be denied"
    );
}

#[test]
fn sys05_metrics_counts_authorization_failures() {
    let scopes = all_scopes();
    let borrowed: Vec<&str> = scopes
        .iter()
        .filter(|scope| **scope != "memory:read")
        .map(String::as_str)
        .collect();
    let resolver = StaticScopes::new().grant(PRINCIPAL, &borrowed);
    let gateway = default_gateway().with_scope_resolver(Arc::new(resolver));
    let response = gateway.dispatch(envelope("memory.stats", all_scopes(), json!({})));
    let value = serde_json::to_value(response).expect("response serialises");
    assert_eq!(value["status"], "error");

    let page = data_of(&gateway, "system.metrics", json!({}));
    let per_operation = page["per_operation"]
        .as_array()
        .expect("per-operation list");
    let denied = per_operation
        .iter()
        .find(|entry| entry["operation_id"] == "memory.stats")
        .expect("the denied call is counted");
    assert_eq!(denied["failure"].as_u64(), Some(1));
    assert_eq!(denied["success"].as_u64(), Some(0));
}

#[test]
fn sys05_metrics_is_deterministic_and_monotonic() {
    let gateway = full_authority();
    data_of(&gateway, "system.capabilities", json!({}));
    let first = metrics(&gateway);
    let second = metrics(&gateway);
    // Same counters twice in a row (the metric reads themselves are counted,
    // so the second read is one higher for system.metrics only).
    assert_eq!(first["total_failure"], second["total_failure"]);
    assert_eq!(
        second["total_success"].as_u64(),
        first["total_success"].as_u64().map(|count| count + 1)
    );
    // Per-operation entries are sorted by operation id.
    let ids: Vec<&str> = second["per_operation"]
        .as_array()
        .expect("list")
        .iter()
        .map(|entry| entry["operation_id"].as_str().expect("id"))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}
