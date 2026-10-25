//! Canonical memory types (§21, §34.1) and the request/response shapes of the
//! memory operations (§34.2).
//!
//! Wire forms follow §34.1 (`'working'|'episodic'|...`) rather than the §21.1
//! envelope example (`"WORKING"`): §34 is the normative catalog for the domain
//! API and §21.1 is an illustrative envelope. The mapping is recorded in
//! ADR-051 (D-01).

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::time::TimeError;

/// API/schema version carried by every memory object.
pub const SCHEMA_VERSION: &str = "1.2.3";
/// Highest number of automatic working-memory candidate blocks per turn (§21.2).
pub const MAX_BLOCKS_PER_TURN: u32 = 5;

/// Which memory store an item lives in (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// Working memory: session/agent scoped and budgeted.
    Working,
    /// Episodic memory: TTL and FSRS driven.
    Episodic,
    /// Semantic memory: versioned, evidence-backed facts.
    Semantic,
    /// Procedural memory: abstract procedures only, never tool grants.
    Procedural,
}

impl MemoryKind {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Working => "working",
            MemoryKind::Episodic => "episodic",
            MemoryKind::Semantic => "semantic",
            MemoryKind::Procedural => "procedural",
        }
    }
}

/// Working memory block state machine (§21.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkingState {
    /// Written but not yet visible to the agent.
    Staged,
    /// Visible and counted against the budget.
    Active,
    /// Paged out; only `page_in` restores it.
    Paged,
    /// Replaced by a `FoldedResult`.
    Folded,
    /// Dropped from working memory; the source may still be searchable.
    Evicted,
}

impl WorkingState {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkingState::Staged => "staged",
            WorkingState::Active => "active",
            WorkingState::Paged => "paged",
            WorkingState::Folded => "folded",
            WorkingState::Evicted => "evicted",
        }
    }
}

/// Episodic memory lifecycle (§21.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeState {
    /// Live episode.
    Active,
    /// Summarised in place, source retained.
    Compressed,
    /// No longer in the active window but retained.
    Archived,
    /// Past its retention horizon; general queries stop, tombstone is pushed.
    Expired,
    /// Logically deleted.
    Tombstoned,
    /// Physically purged after every guard passed.
    Purged,
    /// Frozen by legal hold or explicit protection.
    Held,
}

impl EpisodeState {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            EpisodeState::Active => "active",
            EpisodeState::Compressed => "compressed",
            EpisodeState::Archived => "archived",
            EpisodeState::Expired => "expired",
            EpisodeState::Tombstoned => "tombstoned",
            EpisodeState::Purged => "purged",
            EpisodeState::Held => "held",
        }
    }

    /// Whether the state is reachable by a normal TTL transition.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            EpisodeState::Active | EpisodeState::Compressed | EpisodeState::Archived
        )
    }
}

/// Verification state of a memory record (§21.1 `verification_state`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    /// No independent verification yet.
    Unverified,
    /// Independently verified.
    Verified,
    /// Contradicted or rejected.
    Rejected,
    /// Verified once, but the source moved on.
    Stale,
}

impl VerificationState {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationState::Unverified => "unverified",
            VerificationState::Verified => "verified",
            VerificationState::Rejected => "rejected",
            VerificationState::Stale => "stale",
        }
    }
}

/// Trust columns of a memory object (§21.1, kept separate from authorization).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Trust {
    /// Reliability of the asserting source.
    pub source: f64,
    /// Confidence attributed to the indirect agent.
    pub agent: f64,
    /// Confidence of the extractor that produced the record.
    pub extraction: f64,
}

impl Default for Trust {
    fn default() -> Self {
        Self {
            source: 0.5,
            agent: 0.5,
            extraction: 0.5,
        }
    }
}

impl Trust {
    /// Validate that every column is a bounded probability.
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("trust.source", self.source),
            ("trust.agent", self.agent),
            ("trust.extraction", self.extraction),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(MemoryError::InvalidArgument {
                    field: name.to_string(),
                    reason: format!("must be in [0,1], got {value}"),
                });
            }
        }
        Ok(())
    }

    /// Combined, bounded ranking signal. Never an authorization decision.
    pub fn rank(&self) -> f64 {
        ((self.source + self.agent + self.extraction) / 3.0).clamp(0.0, 1.0)
    }
}

/// Kind of working memory block (§21.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", schemars(rename = "MemoryBlockKind"))]
pub enum BlockKind {
    /// What the task currently is.
    TaskState,
    /// Output of a tool call.
    ToolOutput,
    /// Recent conversation content.
    RecentMessage,
    /// Context the agent asked to keep.
    ActiveContext,
    /// Result of folding a sub-agent scope.
    FoldedResult,
}

impl BlockKind {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::TaskState => "task_state",
            BlockKind::ToolOutput => "tool_output",
            BlockKind::RecentMessage => "recent_message",
            BlockKind::ActiveContext => "active_context",
            BlockKind::FoldedResult => "folded_result",
        }
    }
}

/// Episode type (§21.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeType {
    /// One conversation turn.
    ConversationTurn,
    /// One tool execution.
    ToolExecution,
    /// A reflection episode.
    Reflection,
    /// An episode synthesised from a sub-agent.
    FoldedSubagent,
}

/// Outcome of the work an episode records (§21.3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeOutcome {
    /// Finished successfully.
    Success,
    /// Partially finished.
    Partial,
    /// Failed.
    Failure,
}

/// Requested response detail (§34.1 `MemoryQuery.detail`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailLevel {
    /// Identification only.
    Title,
    /// Key facts only.
    Facts,
    /// Summary plus evidence references.
    Summary,
    /// Full authorised content.
    Full,
}

impl DetailLevel {
    /// Whether the level may return the record body.
    pub fn includes_content(self) -> bool {
        matches!(self, DetailLevel::Full)
    }
}

/// State of a reconsolidation window (§23.4, §34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowState {
    /// Accepting a patch until `labile_until`.
    Open,
    /// Patch committed inside the window.
    Committed,
    /// Closed by the owner without a patch.
    Cancelled,
    /// Closed by time.
    Expired,
}

impl WindowState {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            WindowState::Open => "open",
            WindowState::Committed => "committed",
            WindowState::Cancelled => "cancelled",
            WindowState::Expired => "expired",
        }
    }
}

/// State of one index projection after a deletion (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionState {
    /// Revocation not yet attempted.
    Pending,
    /// Revoked.
    Cleared,
    /// Revocation failed and must be retried.
    Failed,
}

/// State of the cache invalidation step of a deletion (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheInvalidation {
    /// Not yet attempted.
    Pending,
    /// Every cache entry dropped.
    Complete,
    /// At least one cache entry could not be dropped.
    Failed,
}

/// One projection entry of a [`DeletionReceipt`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionStatus {
    /// Projection name, for example `lexical_index`.
    pub name: String,
    /// Whether that projection has been revoked.
    pub state: ProjectionState,
}

/// Address of one memory record.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "MemoryRef"))]
pub struct Ref {
    /// Memory id.
    pub id: String,
    /// Store the id belongs to.
    pub kind: MemoryKind,
    /// Revision or content tag the caller observed.
    pub revision: String,
}

impl Ref {
    /// Build a reference.
    pub fn new(id: impl Into<String>, kind: MemoryKind, revision: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            revision: revision.into(),
        }
    }
}

/// Evidence supporting a claim (§21.1 `provenance_refs`, §34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "MemoryEvidence"))]
pub struct Evidence {
    /// Evidence id.
    pub evidence_id: String,
    /// Where the evidence came from, for example `artifact:sha256:...`.
    pub source_ref: String,
    /// Confidence the extractor assigned to this piece of evidence.
    pub confidence: f64,
    /// Independent source group used for promotion counting (§27.1).
    pub source_group: String,
}

impl Evidence {
    /// Validate the bounded confidence.
    pub fn validate(&self) -> Result<()> {
        if self.evidence_id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "evidence_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(MemoryError::InvalidArgument {
                field: "evidence.confidence".to_string(),
                reason: format!("must be in [0,1], got {}", self.confidence),
            });
        }
        Ok(())
    }
}

/// Summary view of a memory record (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryView {
    /// Address.
    pub reference: Ref,
    /// Store.
    pub kind: MemoryKind,
    /// State token of the owning store.
    pub state: String,
    /// Summary, when the requested detail allows it.
    pub summary: Option<String>,
    /// Body, only for [`DetailLevel::Full`].
    pub content: Option<String>,
    /// Evidence references.
    pub evidence: Vec<Evidence>,
    /// Bounded ranking trust.
    pub trust: f64,
    /// Revision of the returned record.
    pub version: String,
    /// Token count of the body.
    pub token_count: u32,
    /// Whether the record is a folded derivation.
    pub folded: bool,
    /// Language of the content (BCP-47).
    pub language: String,
    /// Whether the body was withheld by policy.
    pub redacted: bool,
}

/// Search request (§34.1 `MemoryQuery`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryQuery {
    /// Free-text query.
    pub query: String,
    /// Stores to search.
    pub kinds: Vec<MemoryKind>,
    /// Optional language filter (BCP-47).
    pub language: Option<String>,
    /// Only records created at or after this instant.
    pub after: Option<String>,
    /// Only records created at or before this instant.
    pub before: Option<String>,
    /// Minimum ranking trust, in `[0,1]`.
    pub min_trust: Option<f64>,
    /// Maximum number of results.
    pub max_results: u32,
    /// Requested detail.
    pub detail: DetailLevel,
    /// Token budget for the response.
    pub max_tokens: u32,
}

/// Input of `memory.put_working_block` (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingBlockInput {
    /// Session the block belongs to.
    pub session_id: String,
    /// Agent the block belongs to.
    pub agent_id: String,
    /// Block kind.
    pub block_type: BlockKind,
    /// Block content.
    pub content: String,
    /// BCP-47 language.
    pub language: String,
    /// Priority in `[0,1]`.
    pub priority: f64,
    /// Whether the block is pinned by the user (§22.2 R6).
    pub protected: bool,
    /// Optional turn-based expiry.
    pub expires_after_turns: Option<u32>,
}

/// Input of `memory.store_episode` (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeInput {
    /// Session id.
    pub session_id: String,
    /// Agent id.
    pub agent_id: String,
    /// Delegation depth of the writing agent.
    pub agent_depth: u32,
    /// Episode type.
    pub episode_type: EpisodeType,
    /// Summary text.
    pub summary: String,
    /// Reference to the full text artifact, if any.
    pub full_text_ref: Option<String>,
    /// Key facts.
    pub key_facts: Vec<String>,
    /// Outcome.
    pub outcome: EpisodeOutcome,
    /// Sources the episode derives from.
    pub source_refs: Vec<String>,
    /// Importance in `[0,1]`.
    pub importance_score: f64,
    /// BCP-47 language.
    pub language: String,
    /// Evidence supporting the episode.
    pub evidence: Vec<Evidence>,
}

/// Input of `memory.fold` (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldInput {
    /// Episodes (or blocks) to fold.
    pub episode_refs: Vec<Ref>,
    /// Policy that authorises the fold.
    pub policy_id: String,
    /// Token ceiling of the result.
    pub max_tokens: u32,
    /// Agent the folded result belongs to.
    pub target_agent_id: String,
}

/// Result of folding (§34.1, §21.5).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldedResult {
    /// Summary text.
    pub summary: String,
    /// Facts that survived the fold.
    pub key_facts: Vec<String>,
    /// Sources the summary derives from.
    pub source_refs: Vec<String>,
    /// Fold depth; each fold increments it.
    pub fold_level: u32,
    /// `T_orig * exp(-0.15 * fold_level)` bounded to `[0,1]`.
    pub trust_score: f64,
    /// Tokens in the summary.
    pub token_count: u32,
    /// Evidence that must never be dropped.
    pub protected_evidence_refs: Vec<String>,
    /// Whether safety rules forced a lossless result.
    pub forced_preserve: bool,
}

/// Receipt of a paging operation (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagingReceipt {
    /// Block that was paged.
    pub block: Ref,
    /// Page that now holds it.
    pub page_ref: Option<Ref>,
    /// Outcome token.
    pub status: PagingStatus,
    /// `sha256:` hash of the paged payload.
    pub integrity_hash: String,
}

/// Outcome of a paging operation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PagingStatus {
    /// The block left working memory.
    PagedOut,
    /// The block came back.
    PagedIn,
    /// The page could not be opened.
    Unavailable,
}

/// Reconsolidation window (§23.4, §34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconsolidationWindow {
    /// Fact the window applies to.
    pub fact: Ref,
    /// Window id.
    pub window_id: String,
    /// Instant the window closes for writes.
    pub expires_at: String,
    /// Window state.
    pub state: WindowState,
}

/// Delegation grant handed to a sub-agent (§26.4, §34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentScopeGrant {
    /// Parent agent.
    pub parent_agent_id: String,
    /// Child agent instance.
    pub child_agent_id: String,
    /// Session the grant is bound to.
    pub session_id: String,
    /// Task the child works on.
    pub task_id: String,
    /// Depth the child will run at.
    pub depth: u32,
    /// Scopes the child may use; must be a subset of the parent's.
    pub allowed_scopes: Vec<String>,
    /// Tools the child may use; must be a subset of the parent's.
    pub allowed_tools: Vec<String>,
    /// Expiry of the grant.
    pub expires_at: String,
    /// Context tokens the child may use.
    pub max_context_tokens: u32,
}

/// View of an open agent scope.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentScopeView {
    /// Scope id.
    pub scope_id: String,
    /// Instant the scope expires.
    pub expires_at: String,
}

/// Result of closing an agent scope.
///
/// Only the folded result, its evidence references and a status cross the
/// agent boundary (§26.4): full episodes and working memory never do.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloseScopeResult {
    /// Whether the scope was open and is now closed.
    pub closed: bool,
    /// Folded hand-over to the parent, when a fold was requested.
    pub folded_result: Option<FoldedResult>,
    /// Terminal status of the child agent.
    pub status: AgentStatus,
    /// Evidence the parent may look up.
    pub evidence_refs: Vec<String>,
}

/// Sub-agent lifecycle (§26.4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Starting up.
    Starting,
    /// Executing.
    Running,
    /// Waiting on the parent or a tool.
    Waiting,
    /// Finished successfully.
    Completed,
    /// Finished partially.
    Partial,
    /// Failed.
    Failed,
    /// Cancelled by the parent.
    Cancelled,
    /// Exceeded `subagent_timeout`.
    TimedOut,
    /// Producing the folded result.
    Folding,
    /// Folded and immutable.
    Sealed,
    /// Eligible for cleanup.
    CleanupEligible,
}

impl AgentStatus {
    /// Stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Starting => "starting",
            AgentStatus::Running => "running",
            AgentStatus::Waiting => "waiting",
            AgentStatus::Completed => "completed",
            AgentStatus::Partial => "partial",
            AgentStatus::Failed => "failed",
            AgentStatus::Cancelled => "cancelled",
            AgentStatus::TimedOut => "timed_out",
            AgentStatus::Folding => "folding",
            AgentStatus::Sealed => "sealed",
            AgentStatus::CleanupEligible => "cleanup_eligible",
        }
    }

    /// Terminal statuses that may enter `folding`.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentStatus::Completed
                | AgentStatus::Partial
                | AgentStatus::Failed
                | AgentStatus::Cancelled
                | AgentStatus::TimedOut
        )
    }
}

/// Receipt of a deletion (forget or working-block removal) (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionReceipt {
    /// Memory that was deleted.
    pub memory: Ref,
    /// Revision that carries the tombstone.
    pub tombstone_revision: String,
    /// Per-projection revocation.
    pub projections: Vec<ProjectionStatus>,
    /// Cache invalidation state.
    pub cache_invalidation: CacheInvalidation,
    /// When the backup copy expires, if a backup exists.
    pub backup_expiry_at: Option<String>,
    /// Whether a legal hold kept any authority from being touched.
    pub legal_hold: bool,
}

/// Cursor page request (§31.4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "MemoryPageRequest"))]
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
            return Err(MemoryError::InvalidArgument {
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
                .map_err(|_| MemoryError::CursorExpired {
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
            return Err(MemoryError::CursorExpired {
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

/// Bookkeeping receipt of a mutation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "MemoryChangeReceipt"))]
pub struct ChangeReceipt {
    /// Resource affected.
    pub resource_id: String,
    /// New etag.
    pub etag: String,
    /// Whether anything changed.
    pub changed: bool,
    /// Events a gateway must publish.
    pub events: Vec<String>,
}

/// Patch applied by `memory.update_revision` / `commit_reconsolidation`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryPatch {
    /// Replacement summary.
    pub summary: Option<String>,
    /// Replacement body.
    pub content: Option<String>,
    /// Replacement key facts.
    pub key_facts: Option<Vec<String>>,
    /// Replacement trust columns.
    pub trust: Option<Trust>,
    /// Replacement language.
    pub language: Option<String>,
    /// Replacement importance.
    pub importance_score: Option<f64>,
}

impl MemoryPatch {
    /// Whether the patch would change anything.
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.content.is_none()
            && self.key_facts.is_none()
            && self.trust.is_none()
            && self.language.is_none()
            && self.importance_score.is_none()
    }

    /// Validate the bounded fields.
    pub fn validate(&self) -> Result<()> {
        if let Some(trust) = &self.trust {
            trust.validate()?;
        }
        if let Some(importance) = self.importance_score {
            if !importance.is_finite() || !(0.0..=1.0).contains(&importance) {
                return Err(MemoryError::InvalidArgument {
                    field: "importance_score".to_string(),
                    reason: format!("must be in [0,1], got {importance}"),
                });
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Operation requests / responses
// ---------------------------------------------------------------------------

/// `memory.open_agent_scope` (MAPI-01).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAgentScope {
    /// Delegation grant.
    pub grant: AgentScopeGrant,
}

/// `memory.close_agent_scope` (MAPI-02).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseAgentScope {
    /// Scope to close.
    pub scope_id: String,
    /// Why it is being closed.
    pub reason: String,
    /// Whether the child should hand a folded result to the parent.
    pub fold_on_close: bool,
}

/// `memory.put_working_block` (MAPI-03).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PutWorkingBlock {
    /// Block to write.
    pub input: WorkingBlockInput,
    /// Whether the block was auto-generated by the runtime this turn.
    pub automatic: bool,
}

/// `memory.list_working_blocks` (MAPI-04).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListWorkingBlocks {
    /// Session to list.
    pub session_id: String,
    /// Agent to list.
    pub agent_id: String,
    /// Page request.
    pub page: PageRequest,
}

/// `memory.remove_working_block` (MAPI-05).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveWorkingBlock {
    /// Block to remove.
    pub block: Ref,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `memory.store_episode` (MAPI-06).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreEpisode {
    /// Episode to store.
    pub input: EpisodeInput,
}

/// `memory.search` (MAPI-07).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchMemory {
    /// Query.
    pub query: MemoryQuery,
}

/// `memory.retrieve` (MAPI-08).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveMemory {
    /// Record to retrieve.
    pub memory: Ref,
    /// Requested detail.
    pub detail: DetailLevel,
    /// Token ceiling of the response.
    pub max_tokens: u32,
}

/// `memory.update_revision` (MAPI-09).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateMemory {
    /// Record to update.
    pub memory: Ref,
    /// Patch to apply.
    pub patch: MemoryPatch,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `memory.fold` (MAPI-13).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldMemory {
    /// Fold input.
    pub input: FoldInput,
    /// Optional model score for the fold level; absent means deterministic.
    pub model_score: Option<f64>,
}

/// `memory.page_out` (MAPI-14).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageOut {
    /// Block to page out.
    pub block: Ref,
    /// Policy authorising the paging.
    pub policy_id: String,
}

/// `memory.page_in` (MAPI-15).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageIn {
    /// Page reference.
    pub page_ref: Ref,
    /// Hash the caller expects.
    pub expected_hash: String,
}

/// Result of `memory.page_in`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageInResult {
    /// Restored block.
    pub block: MemoryView,
    /// Paging receipt.
    pub receipt: PagingReceipt,
}

/// `memory.begin_reconsolidation` (MAPI-16).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeginReconsolidation {
    /// Fact whose window opens.
    pub fact: Ref,
    /// Caller's etag.
    pub expected_etag: String,
    /// Window length in hours; defaults to 4 and is clamped to 1..=12.
    pub window_hours: Option<f64>,
}

/// `memory.commit_reconsolidation` (MAPI-17).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitReconsolidation {
    /// Window to commit.
    pub window_id: String,
    /// Patch to apply.
    pub patch: MemoryPatch,
    /// Evidence supporting the change.
    pub evidence: Vec<Evidence>,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `memory.forget` (MAPI-23).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetMemory {
    /// Record to delete.
    pub memory: Ref,
    /// Why it is deleted.
    pub reason: String,
    /// Caller's etag.
    pub expected_etag: String,
}

/// Input of `memory.propose_fact` (§34.1, MAPI-10 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticFactInput {
    /// Subject of the claim.
    pub subject: Ref,
    /// Predicate.
    pub predicate: String,
    /// Object of the claim.
    pub object: String,
    /// BCP-47 language of the claim.
    pub language: String,
    /// Evidence that supports the claim.
    pub evidence: Vec<Evidence>,
    /// Confidence in `[0,1]`.
    pub confidence: f64,
}

/// `memory.propose_fact` (MAPI-10 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposeFact {
    /// Claim to propose.
    pub input: SemanticFactInput,
}

/// Review decision of `memory.verify_fact` (§34.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactDecision {
    /// Accept the claim.
    Verify,
    /// Reject the claim.
    Reject,
}

/// `memory.verify_fact` (MAPI-11 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyFact {
    /// Claim under review.
    pub fact: Ref,
    /// Independent evidence used for the decision.
    pub evidence_ids: Vec<String>,
    /// Decision.
    pub decision: FactDecision,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `memory.find_conflicts` (MAPI-12 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindConflicts {
    /// Claim to compare against.
    pub fact: Ref,
    /// Maximum number of conflicts to return.
    pub max_results: u32,
}

/// `memory.consolidate` (MAPI-18 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consolidate {
    /// Episodes to consolidate.
    pub episode_refs: Vec<Ref>,
    /// Pipeline policy.
    pub policy_id: String,
    /// Must be `true`: consolidation is asynchronous (§34.2).
    pub asynchronous: bool,
}

/// Handle of an accepted asynchronous job.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "MemoryJobHandle"))]
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
#[cfg_attr(feature = "schema", schemars(rename = "MemoryJobView"))]
pub struct JobView {
    /// Job id.
    pub job_id: String,
    /// Job kind.
    pub kind: String,
    /// Current stage.
    pub stage: String,
    /// Whether the canonical data was committed.
    pub canonical_committed: bool,
    /// Diagnostics.
    pub diagnostics: Vec<String>,
}

/// `memory.get_consolidation_job` (MAPI-19 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetMemoryJob {
    /// Job id.
    pub job_id: String,
}

/// Draft of a procedure (§34.1, MAPI-20 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcedureDraft {
    /// Procedure name.
    pub name: String,
    /// Preconditions.
    pub preconditions: Vec<String>,
    /// Steps.
    pub steps: Vec<String>,
    /// Evidence.
    pub evidence: Vec<Evidence>,
    /// Permissions the procedure needs from the host.
    pub permissions: Vec<String>,
    /// Episodes the procedure was learned from.
    pub source_episodes: Vec<Ref>,
    /// BCP-47 language.
    pub language: String,
}

/// `memory.register_procedure_candidate` (MAPI-20 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterProcedure {
    /// Draft to register.
    pub draft: ProcedureDraft,
}

/// Procedure lifecycle state (§27.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureState {
    /// Learned but not yet extracted into a recipe.
    Learn,
    /// Candidate recipe, not usable.
    Candidate,
    /// Approved by consolidation, not yet activated.
    Consolidated,
    /// Approved for application.
    Active,
    /// Under refinement.
    Refining,
    /// Disabled by policy.
    Disabled,
}

/// View of a procedure (§34.1).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcedureView {
    /// Address.
    pub reference: Ref,
    /// Lifecycle state.
    pub state: ProcedureState,
    /// Observed success rate in `[0,1]`.
    pub success_rate: f64,
    /// Number of observed runs.
    pub run_count: u32,
    /// Permissions required from the host (never granted by memory).
    pub permissions: Vec<String>,
    /// Evidence.
    pub evidence: Vec<Evidence>,
}

/// Review decision of `memory.review_procedure` (§34.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureDecision {
    /// Activate the procedure.
    Activate,
    /// Send it back for refinement.
    Refine,
    /// Disable it.
    Disable,
}

/// `memory.review_procedure` (MAPI-21 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewProcedure {
    /// Procedure under review.
    pub procedure: Ref,
    /// Decision.
    pub decision: ProcedureDecision,
    /// Evidence used for the decision.
    pub evidence_ids: Vec<String>,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `memory.record_procedure_outcome` (MAPI-22 — implemented in P6).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordProcedureOutcome {
    /// Procedure that ran.
    pub procedure: Ref,
    /// Whether the run succeeded.
    pub success: bool,
    /// Evidence of the run.
    pub evidence: Vec<Evidence>,
    /// Caller's etag.
    pub expected_etag: String,
}

/// `memory.evaluate_eviction` (MAPI-24).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluateEviction {
    /// Agent whose working memory is evaluated.
    pub agent_id: String,
    /// Session that holds the blocks.
    pub session_id: String,
    /// Policy id: `deterministic-v1` or a shadow policy id.
    pub policy_id: String,
    /// Whether to only plan.
    pub dry_run: bool,
    /// Tokens that must be freed.
    pub required_tokens: u32,
}

/// Plan produced by `memory.evaluate_eviction`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvictionPlan {
    /// Every candidate considered.
    pub candidates: Vec<Ref>,
    /// Blocks the policy selected.
    pub selected: Vec<Ref>,
    /// Candidates that may never be evicted.
    pub protected: Vec<Ref>,
    /// Model profile behind the proposal, when one ran.
    pub model_profile: Option<String>,
    /// Whether the deterministic policy was authoritative.
    pub deterministic_fallback: bool,
    /// Whether the policy could not free enough without touching protected work.
    pub overflow_safe_failure: bool,
    /// Stable policy identifier.
    pub policy_version: String,
    /// Why the plan looks the way it does.
    pub decision_reason: String,
}

/// `memory.get_budget` (MAPI-25).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetMemoryBudget {
    /// Agent whose budget is reported.
    pub agent_id: String,
    /// Session whose budget is reported.
    pub session_id: String,
}

/// Budget view (§10.2, §22.1, MAPI-25).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetView {
    /// `W_model`.
    pub model_context: u32,
    /// `C_usable`.
    pub available: u32,
    /// `B_eff(depth)`.
    pub agent_budget: u32,
    /// Working memory share.
    pub wm: u32,
    /// Core knowledge share.
    pub core: u32,
    /// Message ring share.
    pub ring: u32,
    /// Tokens currently used by active working blocks.
    pub used: u32,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure mode of the memory domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// Malformed argument.
    InvalidArgument {
        /// Field.
        field: String,
        /// Reason.
        reason: String,
    },
    /// The record is unknown in the bound revision.
    NotFound {
        /// What was not found.
        resource: String,
    },
    /// The caller lacks a required scope.
    Forbidden {
        /// Missing scope.
        required_scope: String,
    },
    /// The record belongs to another tenant, project, agent or session.
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
    /// The caller reused an idempotency key with different arguments.
    IdempotencyConflict {
        /// The reused key.
        key: String,
    },
    /// A mutating operation needs evidence it did not get.
    EvidenceRequired {
        /// Operation that needs it.
        operation: String,
    },
    /// The requested write does not fit the agent budget.
    ContextBudgetExceeded {
        /// Requested tokens.
        requested: u32,
        /// Remaining budget.
        remaining: u32,
    },
    /// `C_usable` is below the 4096 token floor.
    InsufficientContextBudget {
        /// `C_usable`.
        available: u32,
        /// Floor.
        floor: u32,
    },
    /// The deterministic policy could not free enough tokens safely.
    ContextOverflowSafeFailure {
        /// Tokens required.
        required: u32,
        /// Tokens the policy could free.
        freeable: u32,
    },
    /// A policy forbids the operation.
    PolicyDenied {
        /// Why.
        reason: String,
    },
    /// Retention or legal hold forbids the deletion.
    DeletionBlocked {
        /// Why.
        reason: String,
    },
    /// The referenced revision or page is gone.
    StaleReference {
        /// Reference.
        reference: String,
    },
    /// The page payload is unavailable or fails its integrity check.
    SourceUnavailable {
        /// Reference.
        reference: String,
    },
    /// The record is not in a state that allows the operation.
    StateConflict {
        /// Record.
        resource: String,
        /// Current state.
        state: String,
        /// Operation.
        operation: String,
    },
    /// Page cursor is invalid.
    CursorExpired {
        /// Cursor.
        cursor: String,
    },
    /// A result set or candidate scan exceeded its configured limit.
    ResourceLimit {
        /// Detail.
        detail: String,
    },
    /// Too many concurrent sub-agents.
    ResourceExhausted {
        /// Detail.
        detail: String,
    },
    /// A model or profile is required but not available.
    ModelUnavailable {
        /// Profile.
        profile: String,
    },
    /// The capability is registered but not implemented by this build.
    CapabilityUnavailable {
        /// Capability name.
        capability: String,
    },
    /// A timestamp could not be interpreted or is out of range.
    InvalidInstant {
        /// Detail.
        detail: String,
    },
}

impl MemoryError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            MemoryError::InvalidArgument { .. } | MemoryError::InvalidInstant { .. } => {
                "INVALID_ARGUMENT"
            }
            MemoryError::NotFound { .. } => "NOT_FOUND",
            MemoryError::Forbidden { .. } => "FORBIDDEN",
            MemoryError::CrossScopeDenied { .. } => "CROSS_SCOPE_DENIED",
            MemoryError::EtagMismatch { .. } => "ETAG_MISMATCH",
            MemoryError::IdempotencyConflict { .. } => "IDEMPOTENCY_CONFLICT",
            MemoryError::EvidenceRequired { .. } => "EVIDENCE_REQUIRED",
            MemoryError::ContextBudgetExceeded { .. } => "CONTEXT_BUDGET_EXCEEDED",
            MemoryError::InsufficientContextBudget { .. } => "INSUFFICIENT_CONTEXT_BUDGET",
            MemoryError::ContextOverflowSafeFailure { .. } => "CONTEXT_OVERFLOW_SAFE_FAILURE",
            MemoryError::PolicyDenied { .. } => "POLICY_DENIED",
            MemoryError::DeletionBlocked { .. } => "DELETION_BLOCKED",
            MemoryError::StaleReference { .. } => "STALE_REFERENCE",
            MemoryError::SourceUnavailable { .. } => "SOURCE_UNAVAILABLE",
            MemoryError::StateConflict { .. } => "LINK_STATE_CONFLICT",
            MemoryError::CursorExpired { .. } => "CURSOR_EXPIRED",
            MemoryError::ResourceLimit { .. } => "RESOURCE_LIMIT",
            MemoryError::ResourceExhausted { .. } => "RESOURCE_EXHAUSTED",
            MemoryError::ModelUnavailable { .. } => "MODEL_UNAVAILABLE",
            MemoryError::CapabilityUnavailable { .. } => "CAPABILITY_UNAVAILABLE",
        }
    }

    /// Whether retrying the same request may succeed.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            MemoryError::SourceUnavailable { .. }
                | MemoryError::ResourceExhausted { .. }
                | MemoryError::ResourceLimit { .. }
        )
    }
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::InvalidArgument { field, reason } => write!(f, "{field}: {reason}"),
            MemoryError::NotFound { resource } => write!(f, "{resource} not found"),
            MemoryError::Forbidden { required_scope } => {
                write!(f, "missing scope {required_scope}")
            }
            MemoryError::CrossScopeDenied { requested, actual } => {
                write!(f, "scope {requested} cannot access {actual}")
            }
            MemoryError::EtagMismatch { expected, actual } => {
                write!(f, "etag mismatch: expected {expected}, found {actual}")
            }
            MemoryError::IdempotencyConflict { key } => {
                write!(f, "idempotency key {key} was reused with different arguments")
            }
            MemoryError::EvidenceRequired { operation } => {
                write!(f, "{operation} requires evidence")
            }
            MemoryError::ContextBudgetExceeded {
                requested,
                remaining,
            } => write!(f, "need {requested} tokens, {remaining} remain"),
            MemoryError::InsufficientContextBudget { available, floor } => {
                write!(f, "C_usable {available} is below the {floor} floor")
            }
            MemoryError::ContextOverflowSafeFailure { required, freeable } => write!(
                f,
                "cannot free {required} tokens without touching protected content ({freeable} freeable)"
            ),
            MemoryError::PolicyDenied { reason } => write!(f, "policy denied: {reason}"),
            MemoryError::DeletionBlocked { reason } => write!(f, "deletion blocked: {reason}"),
            MemoryError::StaleReference { reference } => write!(f, "{reference} is stale"),
            MemoryError::SourceUnavailable { reference } => {
                write!(f, "{reference} is unavailable")
            }
            MemoryError::StateConflict {
                resource,
                state,
                operation,
            } => write!(f, "{resource} is {state} and cannot {operation}"),
            MemoryError::CursorExpired { cursor } => write!(f, "cursor {cursor} expired"),
            MemoryError::ResourceLimit { detail } => write!(f, "resource limit: {detail}"),
            MemoryError::ResourceExhausted { detail } => write!(f, "resource exhausted: {detail}"),
            MemoryError::ModelUnavailable { profile } => {
                write!(f, "no model available for `{profile}`")
            }
            MemoryError::CapabilityUnavailable { capability } => {
                write!(f, "{capability} is not implemented by this build")
            }
            MemoryError::InvalidInstant { detail } => write!(f, "invalid instant: {detail}"),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<crate::fsrs::FsrsError> for MemoryError {
    fn from(error: crate::fsrs::FsrsError) -> Self {
        MemoryError::InvalidArgument {
            field: "fsrs".to_string(),
            reason: error.to_string(),
        }
    }
}

impl From<TimeError> for MemoryError {
    fn from(error: TimeError) -> Self {
        MemoryError::InvalidInstant {
            detail: error.to_string(),
        }
    }
}

impl From<context_engine::BudgetError> for MemoryError {
    fn from(error: context_engine::BudgetError) -> Self {
        match error {
            context_engine::BudgetError::InsufficientContextBudget { c_usable } => {
                MemoryError::InsufficientContextBudget {
                    available: u32::try_from(c_usable).unwrap_or(0),
                    floor: context_engine::B_FLOOR,
                }
            }
            context_engine::BudgetError::ContextBudgetExceeded { limit, requested } => {
                MemoryError::ContextBudgetExceeded {
                    requested,
                    remaining: limit,
                }
            }
            context_engine::BudgetError::InvalidArgument { field, reason } => {
                MemoryError::InvalidArgument {
                    field: field.to_string(),
                    reason,
                }
            }
        }
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, MemoryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_kind_wire_form_is_lowercase_per_section_34() {
        assert_eq!(
            serde_json::to_string(&MemoryKind::Episodic).unwrap(),
            "\"episodic\""
        );
        assert_eq!(
            serde_json::to_string(&WorkingState::Paged).unwrap(),
            "\"paged\""
        );
        assert_eq!(
            serde_json::to_string(&EpisodeState::Tombstoned).unwrap(),
            "\"tombstoned\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::TimedOut).unwrap(),
            "\"timed_out\""
        );
        assert_eq!(
            serde_json::to_string(&PagingStatus::PagedOut).unwrap(),
            "\"paged_out\""
        );
    }

    #[test]
    fn agent_status_reports_terminal_states() {
        for status in [
            AgentStatus::Completed,
            AgentStatus::Partial,
            AgentStatus::Failed,
            AgentStatus::Cancelled,
            AgentStatus::TimedOut,
        ] {
            assert!(status.is_terminal(), "{}", status.as_str());
        }
        for status in [
            AgentStatus::Starting,
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Folding,
            AgentStatus::Sealed,
            AgentStatus::CleanupEligible,
        ] {
            assert!(!status.is_terminal(), "{}", status.as_str());
        }
    }

    #[test]
    fn trust_validation_rejects_out_of_range_columns() {
        assert!(Trust::default().validate().is_ok());
        let bad = Trust {
            source: 1.5,
            ..Trust::default()
        };
        assert_eq!(bad.validate().unwrap_err().code(), "INVALID_ARGUMENT");
        let nan = Trust {
            agent: f64::NAN,
            ..Trust::default()
        };
        assert!(nan.validate().is_err());
        assert!((Trust::default().rank() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn evidence_requires_id_and_bounded_confidence() {
        let good = Evidence {
            evidence_id: "e1".into(),
            source_ref: "artifact:sha256:aa".into(),
            confidence: 0.8,
            source_group: "doc-a".into(),
        };
        assert!(good.validate().is_ok());
        let mut bad = good.clone();
        bad.evidence_id = "  ".into();
        assert!(bad.validate().is_err());
        let mut bad = good;
        bad.confidence = 1.1;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn page_of_paginates_and_reports_the_next_cursor() {
        let request = PageRequest::new(2);
        let page = Page::of(vec![1, 2, 3], &request).unwrap();
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.next_cursor.as_deref(), Some("2"));
        let second = Page::of(
            vec![1, 2, 3],
            &PageRequest {
                cursor: Some("2".into()),
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(second.items, vec![3]);
        assert_eq!(second.next_cursor, None);
        assert_eq!(
            Page::of(
                vec![1],
                &PageRequest {
                    cursor: Some("9".into()),
                    limit: 2
                }
            )
            .unwrap_err()
            .code(),
            "CURSOR_EXPIRED"
        );
        assert_eq!(
            Page::of(vec![1], &PageRequest::new(0)).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn error_codes_match_the_registry() {
        assert_eq!(
            MemoryError::DeletionBlocked {
                reason: "hold".into()
            }
            .code(),
            "DELETION_BLOCKED"
        );
        assert_eq!(
            MemoryError::CapabilityUnavailable {
                capability: "memory.consolidate".into()
            }
            .code(),
            "CAPABILITY_UNAVAILABLE"
        );
        assert_eq!(
            MemoryError::StateConflict {
                resource: "fact".into(),
                state: "verified".into(),
                operation: "verify".into()
            }
            .code(),
            "LINK_STATE_CONFLICT"
        );
        assert!(MemoryError::SourceUnavailable {
            reference: "page:1".into()
        }
        .retryable());
        assert!(!MemoryError::EtagMismatch {
            expected: "a".into(),
            actual: "b".into()
        }
        .retryable());
    }

    #[test]
    fn memory_patch_detects_emptiness_and_validates_bounds() {
        assert!(MemoryPatch::default().is_empty());
        let patch = MemoryPatch {
            summary: Some("s".into()),
            ..MemoryPatch::default()
        };
        assert!(!patch.is_empty());
        assert!(patch.validate().is_ok());
        let bad = MemoryPatch {
            importance_score: Some(-0.1),
            ..MemoryPatch::default()
        };
        assert_eq!(bad.validate().unwrap_err().code(), "INVALID_ARGUMENT");
    }
}
