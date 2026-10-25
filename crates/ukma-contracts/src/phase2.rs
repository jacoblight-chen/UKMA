//! Phase 2 additive shared types; does not mean Domain APIs are closed.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitStatus {
    NotStarted,
    Pending,
    Committed,
    Unknown,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStatus {
    NotStarted,
    Pending,
    Ready,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalValidity {
    Valid,
    Invalid,
    Unverified,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CurrentApplicability {
    Applicable,
    Stale,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidityVector {
    pub historical_validity: HistoricalValidity,
    pub current_applicability: CurrentApplicability,
    pub index_freshness: crate::Freshness,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationOutcome {
    pub execution_status: String, // validated against operation status on JSON boundary
    pub commit_status: CommitStatus,
    pub projection_status: ProjectionStatus,
    pub operation_id: String,
}
