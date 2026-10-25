//! Canonical trace types (§35.1) and the §40.3 three-dimensional validity model.
//!
//! `TraceState` is only a workflow state. Whether a link was ever true
//! (`historical_validity`) and whether it still applies (`current_applicability`)
//! are separate dimensions and are never collapsed into one `stale` flag.

use serde::{Deserialize, Serialize};
use std::fmt;

/// API/schema version carried by every trace object.
pub const SCHEMA_VERSION: &str = "1.2.3";

/// Workflow state of a link (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceState {
    /// Proposed, not yet verified.
    Proposed,
    /// Verified, awaiting approval.
    Validated,
    /// Approved.
    Accepted,
    /// Refused; terminal.
    Rejected,
    /// Queued for revalidation after a source revision change.
    Stale,
    /// Withdrawn by an approved operator; terminal.
    Revoked,
}

impl TraceState {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            TraceState::Proposed => "proposed",
            TraceState::Validated => "validated",
            TraceState::Accepted => "accepted",
            TraceState::Rejected => "rejected",
            TraceState::Stale => "stale",
            TraceState::Revoked => "revoked",
        }
    }

    /// Whether the state can no longer change (a new revision is required).
    pub fn is_terminal(self) -> bool {
        matches!(self, TraceState::Rejected | TraceState::Revoked)
    }
}

/// Relation kind of a link (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TraceKind {
    /// Requirement specifies an artefact.
    Specifies,
    /// Code implements a requirement.
    Implements,
    /// Test verifies an implementation.
    Verifies,
    /// Plain reference.
    References,
    /// Explicit contradiction.
    Contradicts,
    /// Derived from another artefact.
    DerivedFrom,
}

impl TraceKind {
    /// Every kind, in specification order.
    pub const ALL: [TraceKind; 6] = [
        TraceKind::Specifies,
        TraceKind::Implements,
        TraceKind::Verifies,
        TraceKind::References,
        TraceKind::Contradicts,
        TraceKind::DerivedFrom,
    ];

    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            TraceKind::Specifies => "SPECIFIES",
            TraceKind::Implements => "IMPLEMENTS",
            TraceKind::Verifies => "VERIFIES",
            TraceKind::References => "REFERENCES",
            TraceKind::Contradicts => "CONTRADICTS",
            TraceKind::DerivedFrom => "DERIVED_FROM",
        }
    }
}

/// How a trace endpoint is classified for coverage and gap analysis.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    /// A requirement.
    Requirement,
    /// Production code.
    Code,
    /// A test or check.
    Test,
    /// A document.
    Document,
    /// Anything else.
    Other,
}

/// Historical truth of a link at its asserted revision (§40.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalValidity {
    /// The link was true for the revision it asserted.
    Valid,
    /// The link was never true for that revision.
    Invalid,
    /// Never assessed.
    Unverified,
}

/// Whether the link still applies to the current heads (§40.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentApplicability {
    /// Applies to the observed heads.
    Applicable,
    /// A source moved on; the link must be revalidated.
    RequiresRevalidation,
    /// The relation no longer holds.
    NotApplicable,
    /// Heads unknown, so applicability cannot be decided.
    Unknown,
}

/// Freshness of the index backing the link (§40.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexFreshness {
    /// Index is at the observed generation.
    Current,
    /// Index lags the observed generation.
    Lagging,
    /// Index is being rebuilt.
    Rebuilding,
    /// Index is unavailable.
    Unavailable,
    /// Unknown.
    Unknown,
}

/// The three independent validity dimensions (§40.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidityVector {
    /// Historical truth.
    pub historical_validity: HistoricalValidity,
    /// Current applicability.
    pub current_applicability: CurrentApplicability,
    /// Index freshness.
    pub index_freshness: IndexFreshness,
    /// Revision the link asserted.
    pub asserted_source_revision: String,
    /// Observed head revision, when known.
    pub observed_head_revision: Option<String>,
    /// When this assessment was made.
    pub assessed_at: String,
    /// Evidence the assessment rests on.
    pub evidence_refs: Vec<String>,
}

/// Address of a trace object.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "TraceabilityRef"))]
pub struct Ref {
    /// Object id.
    pub id: String,
    /// Object kind.
    pub kind: String,
    /// Observed revision.
    pub revision: String,
}

impl Ref {
    /// Build a reference.
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        revision: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            revision: revision.into(),
        }
    }
}

/// Evidence supporting a link.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "TraceabilityEvidence"))]
pub struct Evidence {
    /// Evidence id.
    pub evidence_id: String,
    /// Where it came from.
    pub source_ref: String,
    /// Confidence in `[0,1]`.
    pub confidence: f64,
}

/// One trace link (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceLink {
    /// Address of the link itself.
    pub reference: Ref,
    /// Source endpoint.
    pub source: Ref,
    /// Target endpoint.
    pub target: Ref,
    /// Relation kind.
    pub kind: TraceKind,
    /// Workflow state.
    pub state: TraceState,
    /// Evidence. Immutable once accepted.
    pub evidence: Vec<Evidence>,
    /// Why the link exists.
    pub rationale: Option<String>,
    /// Creation instant.
    pub created_at: String,
    /// Instant of the last verification.
    pub verified_at: Option<String>,
    /// Source revision asserted by the link.
    pub source_revision: String,
    /// Target revision asserted by the link.
    pub target_revision: String,
    /// Content tag.
    pub etag: String,
    /// Three-dimensional validity.
    pub validity: ValidityVector,
}

/// Input of `traceability.propose_link` (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceLinkInput {
    /// Source endpoint.
    pub source: Ref,
    /// Target endpoint.
    pub target: Ref,
    /// Relation kind.
    pub kind: TraceKind,
    /// Evidence.
    pub evidence: Vec<Evidence>,
    /// Optional rationale.
    pub rationale: Option<String>,
}

/// Cursor page request (§31.4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "TraceabilityPageRequest"))]
pub struct PageRequest {
    /// Opaque cursor.
    pub cursor: Option<String>,
    /// Maximum items.
    pub limit: u32,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: 50,
        }
    }
}

impl PageRequest {
    /// Largest page a response may return.
    pub const MAX_LIMIT: u32 = 200;

    /// Page request with a limit.
    pub fn new(limit: u32) -> Self {
        Self {
            cursor: None,
            limit,
        }
    }

    /// Validate the limit.
    pub fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > Self::MAX_LIMIT {
            return Err(TraceError::InvalidArgument {
                field: "page.limit".to_string(),
                reason: format!("must be in 1..={}", Self::MAX_LIMIT),
            });
        }
        Ok(())
    }

    /// Cursor offset.
    pub fn offset(&self) -> Result<usize> {
        match &self.cursor {
            None => Ok(0),
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| TraceError::CursorExpired {
                    cursor: cursor.clone(),
                }),
        }
    }
}

/// One page of results.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items.
    pub items: Vec<T>,
    /// Cursor for the next page.
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Build a page from the full ordered result set.
    pub fn of(items: Vec<T>, request: &PageRequest) -> Result<Self> {
        request.validate()?;
        let offset = request.offset()?;
        if offset > items.len() {
            return Err(TraceError::CursorExpired {
                cursor: request.cursor.clone().unwrap_or_default(),
            });
        }
        let limit = request.limit as usize;
        let mut page: Vec<T> = items.into_iter().skip(offset).take(limit + 1).collect();
        let next_cursor = if page.len() > limit {
            page.truncate(limit);
            Some((offset + limit).to_string())
        } else {
            None
        };
        Ok(Self {
            items: page,
            next_cursor,
        })
    }
}

/// Handle of an accepted asynchronous job.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "TraceabilityJobHandle"))]
pub struct JobHandle {
    /// Job id.
    pub job_id: String,
    /// Job kind.
    pub kind: String,
    /// Stage when the handle was produced.
    pub stage: String,
}

/// Live view of an asynchronous job.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "TraceabilityJobView"))]
pub struct JobView {
    /// Job id.
    pub job_id: String,
    /// Job kind.
    pub kind: String,
    /// Current stage.
    pub stage: String,
    /// Whether canonical data was committed.
    pub canonical_committed: bool,
    /// Diagnostics.
    pub diagnostics: Vec<String>,
}

// ---------------------------------------------------------------------------
// Operation requests / responses
// ---------------------------------------------------------------------------

/// `traceability.propose_link` (TAPI-01).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposeLink {
    /// Link to propose.
    pub link: TraceLinkInput,
}

/// `traceability.validate_link` (TAPI-02).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidateLink {
    /// Link under validation.
    pub link: Ref,
    /// Evidence ids used for the decision.
    pub evidence_ids: Vec<String>,
    /// Caller's etag.
    pub expected_etag: String,
}

/// Decision of `traceability.accept_link` (§35.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptDecision {
    /// Accept the link.
    Accept,
    /// Reject the link.
    Reject,
}

/// `traceability.accept_link` (TAPI-03).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptLink {
    /// Link under review.
    pub link: Ref,
    /// Decision.
    pub decision: AcceptDecision,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `traceability.revoke_link` (TAPI-04).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeLink {
    /// Link to revoke.
    pub link: Ref,
    /// Why.
    pub reason: String,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `traceability.get_link` (TAPI-05).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetLink {
    /// Link to read.
    pub link: Ref,
}

/// `traceability.list_links` (TAPI-06).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListLinks {
    /// Endpoint to list around.
    pub endpoint: Ref,
    /// Kinds to include; empty means all.
    pub kinds: Vec<TraceKind>,
    /// States to include; empty means all.
    pub states: Vec<TraceState>,
    /// Page request.
    pub page: PageRequest,
}

/// `traceability.query_coverage` (TAPI-07, §35.1 `CoverageQuery`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageQuery {
    /// Tenant/project the requirements live in.
    pub requirement_scope: ScopeRef,
    /// Explicit requirement subset; empty means every in-scope requirement.
    pub requirement_refs: Vec<Ref>,
    /// Kinds that count as implementation.
    pub implementation_kinds: Vec<TraceKind>,
    /// Only count accepted links.
    pub accepted_only: bool,
    /// Count links whose index is lagging.
    pub include_unindexed: bool,
    /// Page request.
    pub page: PageRequest,
}

/// Tenant/project scope, kept local so the crate has no wire dependency.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRef {
    /// Tenant.
    pub tenant_id: String,
    /// Project.
    pub project_id: String,
}

/// Coverage report (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Requirements with a qualifying implementation link.
    pub numerator: u32,
    /// Authorised in-scope requirements.
    pub denominator: u32,
    /// `numerator/denominator`, `None` when the denominator is zero.
    pub coverage_ratio: Option<f64>,
    /// Covered requirements.
    pub covered: Vec<Ref>,
    /// Requirements without a qualifying link.
    pub uncovered: Vec<Ref>,
    /// Requirements whose coverage cannot be decided.
    pub unknown: Vec<Ref>,
    /// Requirements excluded because they are out of scope or unauthorised.
    pub excluded_count: u32,
    /// Whether the analysis was incomplete.
    pub coverage_incomplete: bool,
    /// Snapshot the report was computed against.
    pub snapshot_id: String,
}

/// `traceability.analyze_change_impact` (TAPI-08, §35.1 `TraceImpactQuery`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceImpactQuery {
    /// Changed endpoints.
    pub changed: Vec<Ref>,
    /// Maximum traversal depth.
    pub max_depth: u32,
    /// Kinds to follow; empty means all.
    pub kinds: Vec<TraceKind>,
    /// Page request for the returned links.
    pub page: PageRequest,
}

/// Impact analysis result (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceImpact {
    /// Everything reachable from the change.
    pub impacted: Vec<Ref>,
    /// Links traversed.
    pub links: Vec<TraceLink>,
    /// One path per impacted endpoint.
    pub paths: Vec<Vec<Ref>>,
    /// Endpoints that could not be expanded because a link is stale.
    pub unknown_due_to_stale: Vec<Ref>,
    /// `complete` or `partial`.
    pub completeness: Completeness,
}

/// Completeness marker shared by the analysis responses.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    /// Every conclusion could be reached.
    Complete,
    /// Something could not be decided.
    Partial,
}

/// `traceability.find_design_gaps` (TAPI-09).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapQuery {
    /// Tenant/project scope.
    pub scope: ScopeRef,
    /// Requirement subset; empty means every in-scope requirement.
    pub requirement_refs: Vec<Ref>,
}

/// Gap report (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapReport {
    /// Requirements with no implementation link.
    pub missing_implementation: Vec<Ref>,
    /// Implementations with no verifying test.
    pub missing_tests: Vec<Ref>,
    /// Links that need revalidation.
    pub stale_links: Vec<Ref>,
    /// Links that could not be verified.
    pub unverifiable: Vec<Ref>,
    /// Whether the analysis was incomplete.
    pub coverage_incomplete: bool,
}

/// `traceability.compare_revisions` (TAPI-10).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRevisionDiff {
    /// Baseline snapshot.
    pub from_revision: String,
    /// Comparison snapshot.
    pub to_revision: String,
    /// Tenant/project scope.
    pub scope: ScopeRef,
}

/// Result of `traceability.compare_revisions` (§35.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceRevisionDiffResult {
    /// Links only in `to`.
    pub added: Vec<TraceLink>,
    /// Links whose state or etag changed.
    pub changed: Vec<TraceLink>,
    /// Links only in `from`.
    pub removed: Vec<TraceLink>,
}

/// `traceability.explain_with_evidence` (TAPI-11).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainEvidence {
    /// Claim to explain.
    pub claim: String,
    /// Endpoints the claim is about.
    pub refs: Vec<Ref>,
    /// Token ceiling of the answer.
    pub max_tokens: u32,
}

/// Conclusion of an evidence report (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    /// The links support the claim.
    Supported,
    /// A contradicting link exists.
    Contradicted,
    /// Not enough evidence.
    Insufficient,
}

/// Evidence report (§35.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReport {
    /// Claim that was explained.
    pub claim: String,
    /// Links used.
    pub links: Vec<TraceLink>,
    /// Evidence gathered.
    pub evidence: Vec<Evidence>,
    /// Conclusion.
    pub conclusion: Conclusion,
    /// References withheld because the caller cannot read them.
    pub omitted_due_to_access: u32,
    /// Whether the report is partial.
    pub completeness: Completeness,
}

/// `traceability.revalidate_stale` (TAPI-12).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevalidateStale {
    /// Links to revalidate.
    pub link_refs: Vec<Ref>,
    /// Policy that authorises the revalidation.
    pub policy_id: String,
    /// Must be `true`: revalidation is asynchronous (§35.2).
    pub asynchronous: bool,
}

/// `traceability.get_revalidation_job` (TAPI-13).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTraceJob {
    /// Job id.
    pub job_id: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure mode of the traceability domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceError {
    /// Malformed argument.
    InvalidArgument {
        /// Field.
        field: String,
        /// Reason.
        reason: String,
    },
    /// Unknown object.
    NotFound {
        /// What was not found.
        resource: String,
    },
    /// The caller lacks a required scope.
    Forbidden {
        /// Missing scope.
        required_scope: String,
    },
    /// The object belongs to another tenant/project.
    CrossScopeDenied {
        /// Requested scope.
        requested: String,
        /// Owning scope.
        actual: String,
    },
    /// Optimistic concurrency mismatch.
    EtagMismatch {
        /// Expected.
        expected: String,
        /// Actual.
        actual: String,
    },
    /// The link is not in a state that allows the transition.
    LinkStateConflict {
        /// Link.
        link: String,
        /// Current state.
        state: String,
        /// Requested transition.
        transition: String,
    },
    /// A link needs evidence it did not get.
    EvidenceRequired {
        /// Operation.
        operation: String,
    },
    /// The asserted revisions no longer match the observed heads.
    SourceRevisionMismatch {
        /// Endpoint.
        endpoint: String,
        /// Asserted revision.
        asserted: String,
        /// Observed head.
        observed: String,
    },
    /// A snapshot or endpoint is unavailable.
    SourceUnavailable {
        /// Reference.
        reference: String,
    },
    /// An analysis exceeded its configured limit.
    ResourceLimit {
        /// Detail.
        detail: String,
    },
    /// The index is stale for the requested operation.
    IndexStale {
        /// Detail.
        detail: String,
    },
    /// The index is not ready yet.
    IndexNotReady {
        /// Detail.
        detail: String,
    },
    /// Unknown job.
    JobNotFound {
        /// Job id.
        job_id: String,
    },
    /// Page cursor is invalid.
    CursorExpired {
        /// Cursor.
        cursor: String,
    },
    /// The answer would exceed the token budget.
    ContextBudgetExceeded {
        /// Requested tokens.
        requested: u32,
        /// Budget.
        remaining: u32,
    },
}

impl TraceError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            TraceError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            TraceError::NotFound { .. } => "NOT_FOUND",
            TraceError::Forbidden { .. } => "FORBIDDEN",
            TraceError::CrossScopeDenied { .. } => "CROSS_SCOPE_DENIED",
            TraceError::EtagMismatch { .. } => "ETAG_MISMATCH",
            TraceError::LinkStateConflict { .. } => "LINK_STATE_CONFLICT",
            TraceError::EvidenceRequired { .. } => "EVIDENCE_REQUIRED",
            TraceError::SourceRevisionMismatch { .. } => "SOURCE_REVISION_MISMATCH",
            TraceError::SourceUnavailable { .. } => "SOURCE_UNAVAILABLE",
            TraceError::ResourceLimit { .. } => "RESOURCE_LIMIT",
            TraceError::IndexStale { .. } => "INDEX_STALE",
            TraceError::IndexNotReady { .. } => "INDEX_NOT_READY",
            TraceError::JobNotFound { .. } => "JOB_NOT_FOUND",
            TraceError::CursorExpired { .. } => "CURSOR_EXPIRED",
            TraceError::ContextBudgetExceeded { .. } => "CONTEXT_BUDGET_EXCEEDED",
        }
    }
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceError::InvalidArgument { field, reason } => write!(f, "{field}: {reason}"),
            TraceError::NotFound { resource } => write!(f, "{resource} not found"),
            TraceError::Forbidden { required_scope } => write!(f, "missing scope {required_scope}"),
            TraceError::CrossScopeDenied { requested, actual } => {
                write!(f, "scope {requested} cannot access {actual}")
            }
            TraceError::EtagMismatch { expected, actual } => {
                write!(f, "etag mismatch: expected {expected}, found {actual}")
            }
            TraceError::LinkStateConflict {
                link,
                state,
                transition,
            } => write!(f, "{link} is {state} and cannot {transition}"),
            TraceError::EvidenceRequired { operation } => {
                write!(f, "{operation} requires evidence")
            }
            TraceError::SourceRevisionMismatch {
                endpoint,
                asserted,
                observed,
            } => write!(
                f,
                "{endpoint} asserted {asserted} but the head is {observed}"
            ),
            TraceError::SourceUnavailable { reference } => {
                write!(f, "{reference} is unavailable")
            }
            TraceError::ResourceLimit { detail } => write!(f, "resource limit: {detail}"),
            TraceError::IndexStale { detail } => write!(f, "index stale: {detail}"),
            TraceError::IndexNotReady { detail } => write!(f, "index not ready: {detail}"),
            TraceError::JobNotFound { job_id } => write!(f, "job {job_id} not found"),
            TraceError::CursorExpired { cursor } => write!(f, "cursor {cursor} expired"),
            TraceError::ContextBudgetExceeded {
                requested,
                remaining,
            } => write!(f, "need {requested} tokens, {remaining} remain"),
        }
    }
}

impl std::error::Error for TraceError {}

/// Result alias.
pub type Result<T> = std::result::Result<T, TraceError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_tokens_match_the_catalog() {
        assert_eq!(
            serde_json::to_string(&TraceState::Stale).unwrap(),
            "\"stale\""
        );
        assert_eq!(
            serde_json::to_string(&TraceKind::DerivedFrom).unwrap(),
            "\"DERIVED_FROM\""
        );
        assert_eq!(
            serde_json::to_string(&HistoricalValidity::Valid).unwrap(),
            "\"valid\""
        );
        assert_eq!(
            serde_json::to_string(&CurrentApplicability::RequiresRevalidation).unwrap(),
            "\"requires_revalidation\""
        );
        assert_eq!(
            serde_json::to_string(&Conclusion::Insufficient).unwrap(),
            "\"insufficient\""
        );
    }

    #[test]
    fn terminal_states_are_rejected_and_revoked() {
        assert!(TraceState::Rejected.is_terminal());
        assert!(TraceState::Revoked.is_terminal());
        for state in [
            TraceState::Proposed,
            TraceState::Validated,
            TraceState::Accepted,
            TraceState::Stale,
        ] {
            assert!(!state.is_terminal(), "{}", state.as_str());
        }
        assert_eq!(TraceKind::ALL.len(), 6);
    }

    #[test]
    fn page_pagination_and_validation() {
        let page = Page::of(vec![1, 2, 3], &PageRequest::new(2)).unwrap();
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.next_cursor.as_deref(), Some("2"));
        assert_eq!(
            Page::of(vec![1], &PageRequest::new(0)).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            Page::of(
                vec![1],
                &PageRequest {
                    cursor: Some("9".into()),
                    limit: 5
                }
            )
            .unwrap_err()
            .code(),
            "CURSOR_EXPIRED"
        );
    }

    #[test]
    fn error_codes_match_the_registry() {
        assert_eq!(
            TraceError::LinkStateConflict {
                link: "l".into(),
                state: "accepted".into(),
                transition: "validate".into()
            }
            .code(),
            "LINK_STATE_CONFLICT"
        );
        assert_eq!(
            TraceError::SourceRevisionMismatch {
                endpoint: "e".into(),
                asserted: "1".into(),
                observed: "2".into()
            }
            .code(),
            "SOURCE_REVISION_MISMATCH"
        );
        assert_eq!(
            TraceError::EvidenceRequired {
                operation: "op".into()
            }
            .code(),
            "EVIDENCE_REQUIRED"
        );
    }
}
