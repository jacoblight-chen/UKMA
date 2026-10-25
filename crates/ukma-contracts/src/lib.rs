//! UKMA v1.2.3 contract phase 1: shared preliminary wire types.
//! These structs are not yet a fully closed domain API specification.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Consistency {
    Eventual,
    ReadYourWrites,
    Snapshot,
    StrictGeneration,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Code,
    Document,
    Memory,
    Traceability,
    Shared,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRef {
    pub tenant_id: String,
    pub project_id: String,
    pub repo_id: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    pub domain: Domain,
    pub kind: String,
    pub id: String,
    pub revision: String,
    pub scope: ScopeRef,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestContext {
    pub request_id: String,
    pub trace_id: String,
    pub scope: ScopeRef,
    pub principal_id: String,
    pub scopes: Vec<String>,
    pub deadline_at: Option<String>,
    pub consistency: Consistency,
    pub required_generation: Option<String>,
    pub snapshot_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub expected_etag: Option<String>,
    pub delegation_token: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope<T> {
    pub api_version: String,
    pub operation: String,
    pub context: RequestContext,
    pub input: T,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Ok,
    Partial,
    Error,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Current,
    Stale,
    Rebuilding,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetail {
    pub code: String,
    pub safe_message: String,
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialReason {
    pub source: String,
    pub code: String,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope<T> {
    pub api_version: String,
    pub request_id: String,
    pub trace_id: String,
    pub status: ResponseStatus,
    pub data: Option<T>,
    pub error: Option<ErrorDetail>,
    pub warnings: Vec<String>,
    pub partial_reasons: Vec<PartialReason>,
    pub snapshot_id: Option<String>,
    pub index_generation: Option<String>,
    pub freshness: Freshness,
    pub observed_revision: Option<String>,
}

pub mod phase2;

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> ScopeRef {
        ScopeRef {
            tenant_id: "t".into(),
            project_id: "p".into(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        }
    }

    #[test]
    fn enum_wire_names_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&Consistency::ReadYourWrites).unwrap(),
            "\"read_your_writes\""
        );
        assert_eq!(serde_json::to_string(&Domain::Code).unwrap(), "\"code\"");
        assert_eq!(
            serde_json::to_string(&phase2::CommitStatus::NotStarted).unwrap(),
            "\"not_started\""
        );
        assert_eq!(
            serde_json::to_string(&phase2::ProjectionStatus::Ready).unwrap(),
            "\"ready\""
        );
        assert_eq!(
            serde_json::to_string(&phase2::HistoricalValidity::Unverified).unwrap(),
            "\"unverified\""
        );
        assert_eq!(
            serde_json::to_string(&phase2::CurrentApplicability::Stale).unwrap(),
            "\"stale\""
        );
    }

    #[test]
    fn request_envelope_round_trip_preserves_wire_form() {
        let envelope = RequestEnvelope {
            api_version: "1.2.3".into(),
            operation: "code.get_symbol".into(),
            context: RequestContext {
                request_id: "req-1".into(),
                trace_id: "trace-1".into(),
                scope: scope(),
                principal_id: "principal".into(),
                scopes: vec!["code.read".into()],
                deadline_at: None,
                consistency: Consistency::ReadYourWrites,
                required_generation: None,
                snapshot_id: None,
                idempotency_key: Some("idem-1".into()),
                expected_etag: None,
                delegation_token: None,
            },
            input: serde_json::json!({"symbol_id": "symbol:one"}),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"read_your_writes\""));
        assert!(json.contains("\"idempotency_key\""));
        let back: RequestEnvelope<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.operation, "code.get_symbol");
        assert_eq!(back.context.consistency, Consistency::ReadYourWrites);
        assert_eq!(back.context.idempotency_key.as_deref(), Some("idem-1"));
    }

    #[test]
    fn response_envelope_round_trip() {
        let envelope = ResponseEnvelope {
            api_version: "1.2.3".into(),
            request_id: "req-1".into(),
            trace_id: "trace-1".into(),
            status: ResponseStatus::Ok,
            data: Some(1u64),
            error: None,
            warnings: vec![],
            partial_reasons: vec![],
            snapshot_id: None,
            index_generation: Some("g1".into()),
            freshness: Freshness::Current,
            observed_revision: Some("r1".into()),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let back: ResponseEnvelope<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, ResponseStatus::Ok);
        assert_eq!(back.data, Some(1));
        assert_eq!(back.freshness, Freshness::Current);
    }

    #[test]
    fn wire_types_reject_unknown_fields() {
        let bad_scope = r#"{"tenant_id":"t","project_id":"p","unknown_field":1}"#;
        assert!(serde_json::from_str::<ScopeRef>(bad_scope).is_err());
        let bad_context = r#"{"request_id":"r","trace_id":"t","scope":{"tenant_id":"t","project_id":"p"},"principal_id":"p","scopes":[],"deadline_at":null,"consistency":"eventual","required_generation":null,"snapshot_id":null,"idempotency_key":null,"expected_etag":null,"delegation_token":null,"unexpected":true}"#;
        assert!(serde_json::from_str::<RequestContext>(bad_context).is_err());
    }
}
