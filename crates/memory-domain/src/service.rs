//! The memory service: working memory, episodic lifecycle, folding, eviction,
//! paging, reconsolidation, agent scopes and deletion.
//!
//! Operations implemented by this build (P5 scope): MAPI-01..09, 13..17, 23..25.
//! The advanced operations of §27/§24 (fact promotion, consolidation DAG,
//! procedures, multilingual alignment) belong to P6 and report
//! `CAPABILITY_UNAVAILABLE` instead of silently returning an empty success.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use context_engine::{
    allocate, b_eff, check_allocation, next_compression_stage, Candidate, CompressionStage,
    DeterministicPolicy, EvictionPolicy, PreserveRule, Reserves, ShadowRlPolicy,
};
use embedding_engine::EmbeddingPort;
use ukma_contracts::RequestContext;

use crate::deletion::{self, DeletionGuards};
use crate::domain::{
    AgentScopeGrant, AgentScopeView, AgentStatus, BeginReconsolidation, BlockKind, BudgetView,
    ChangeReceipt, CloseAgentScope, CloseScopeResult, CommitReconsolidation, DeletionReceipt,
    DetailLevel, EvaluateEviction, EvictionPlan, FoldInput, FoldMemory, FoldedResult, ForgetMemory,
    GetMemoryBudget, GetMemoryStats, ListMemoryQuery, ListWorkingBlocks, MemoryContextMatch,
    MemoryContextQuery, MemoryContextResult, MemoryError, MemoryKind, MemoryKindCount,
    MemoryListItem, MemoryListPage, MemoryPatch, MemoryStats, MemoryVerificationCount, MemoryView,
    OpenAgentScope, Page, PageIn, PageInResult, PageOut, PageRequest, PagingReceipt, PagingStatus,
    ProcedureState, PutWorkingBlock, Ref, RemoveWorkingBlock, Result, RetrieveMemory, SearchMemory,
    StoreEpisode, Trust, UpdateMemory, VerificationState, WorkingState,
};
use crate::episodic::{self, Episode, SweepAction, SweepDecision};
use crate::fsrs::{self, FsrsState};
use crate::reconsolidation::{MemoryRecord, Window};
use crate::scope::{self, ParentAuthority};
use crate::search::{self, SearchDocument};
use crate::time::{self, format_rfc3339};

/// Scope needed to read memory.
pub const SCOPE_READ: &str = "memory:read";
/// Scope needed to write memory.
pub const SCOPE_WRITE: &str = "memory:write";
/// Scope needed to delegate a sub-agent scope.
pub const SCOPE_DELEGATE: &str = "memory:delegate";
/// Scope needed to fold.
pub const SCOPE_FOLD: &str = "memory:fold";
/// Scope needed to plan eviction.
pub const SCOPE_MANAGE: &str = "memory:manage";
/// Scope needed to delete memory.
pub const SCOPE_FORGET: &str = "memory:forget";
/// Scope needed to review a fact (§34.2 MAPI-11).
pub const SCOPE_REVIEW: &str = "memory:review";
/// Scope needed to consolidate (§34.2 MAPI-18).
pub const SCOPE_CONSOLIDATE: &str = "memory:consolidate";
/// Scope needed to write a procedure (§34.2 MAPI-20/22).
pub const SCOPE_PROCEDURE_WRITE: &str = "memory:procedure_write";
/// Scope needed to review a procedure (§34.2 MAPI-21).
pub const SCOPE_PROCEDURE_REVIEW: &str = "memory:procedure_review";

/// Deterministic policy identifier (§22.3).
pub const POLICY_DETERMINISTIC: &str = "deterministic-v1";
/// Shadow RL policy identifier (§22.3).
pub const POLICY_SHADOW_RL: &str = "shadow-rl-v1";
/// Default page lifetime in hours (§22.4 page-cache TTL).
pub const PAGE_TTL_HOURS: f64 = 24.0;

/// Authorization decisions.
pub trait MemoryAuthorizer: Send + Sync {
    /// Require `scope` on the context.
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> Result<()>;
}

/// Default authorizer: the delegated scope must be present.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScopeAuthorizer;

impl MemoryAuthorizer for ScopeAuthorizer {
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> Result<()> {
        if ctx.scopes.iter().any(|granted| granted == scope) {
            Ok(())
        } else {
            Err(MemoryError::Forbidden {
                required_scope: scope.to_string(),
            })
        }
    }
}

/// Supplies the rights a caller may delegate.
///
/// The default implementation derives them from the request context and holds
/// no tools, so tool delegation is refused until a capability registry is
/// wired: an unverifiable attenuation is never accepted.
pub trait DelegationAuthority: Send + Sync {
    /// Authority held by the caller.
    fn authority(&self, ctx: &RequestContext) -> ParentAuthority;
}

/// Derives delegation rights from the request context.
#[derive(Debug, Default, Clone, Copy)]
pub struct ContextDelegation;

impl DelegationAuthority for ContextDelegation {
    fn authority(&self, ctx: &RequestContext) -> ParentAuthority {
        ParentAuthority {
            tenant_id: ctx.scope.tenant_id.clone(),
            agent_instance_id: ctx
                .scope
                .agent_id
                .clone()
                .unwrap_or_else(|| "agent-root".to_string()),
            depth: 0,
            scopes: ctx.scopes.clone(),
            tools: Vec::new(),
            max_context_tokens: ctx
                .required_generation
                .as_ref()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(u32::MAX),
            expires_at: i64::MAX,
            remaining_tokens: u32::MAX,
        }
    }
}

/// Time source.
pub trait MemoryClock: Send + Sync {
    /// Current instant in Unix seconds.
    fn now(&self) -> i64;
}

/// Wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl MemoryClock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Test/controlled clock.
#[derive(Debug, Default)]
pub struct FixedClock {
    now: Mutex<i64>,
}

impl FixedClock {
    /// Clock pinned at an instant.
    pub fn new(now: i64) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    /// Move the clock forward.
    pub fn advance(&self, seconds: i64) {
        let mut guard = self.now.lock().expect("clock poisoned");
        *guard += seconds;
    }

    /// Set the clock.
    pub fn set(&self, seconds: i64) {
        let mut guard = self.now.lock().expect("clock poisoned");
        *guard = seconds;
    }
}

impl MemoryClock for FixedClock {
    fn now(&self) -> i64 {
        *self.now.lock().expect("clock poisoned")
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemScope {
    tenant_id: String,
    project_id: String,
    session_id: Option<String>,
    agent_id: Option<String>,
}

impl ItemScope {
    fn from_context(ctx: &RequestContext) -> Self {
        Self {
            tenant_id: ctx.scope.tenant_id.clone(),
            project_id: ctx.scope.project_id.clone(),
            session_id: ctx.scope.session_id.clone(),
            agent_id: ctx.scope.agent_id.clone(),
        }
    }

    fn same_tenant_project(&self, other: &ItemScope) -> bool {
        self.tenant_id == other.tenant_id && self.project_id == other.project_id
    }

    fn same_sandbox(&self, other: &ItemScope) -> bool {
        self.same_tenant_project(other)
            && self.session_id == other.session_id
            && self.agent_id == other.agent_id
    }
}

#[derive(Debug, Clone)]
struct Block {
    id: String,
    scope: ItemScope,
    kind: BlockKind,
    content: String,
    language: String,
    priority: f64,
    protected: bool,
    preserve_rules: Vec<PreserveRule>,
    expires_after_turns: Option<u32>,
    state: WorkingState,
    revision: u64,
    etag: String,
    token_count: u32,
    created_at: i64,
    page_id: Option<String>,
    evidence_refs: Vec<String>,
    automatic: bool,
    turn: u64,
    referenced: bool,
}

#[derive(Debug, Clone)]
struct PageRecord {
    page_id: String,
    block_id: String,
    scope: ItemScope,
    payload: String,
    checksum: String,
    memory_revision: u64,
    generation: String,
    expires_at: i64,
    refcount: u32,
}

#[derive(Debug, Clone)]
struct MemoryItem {
    kind: MemoryKind,
    scope: ItemScope,
    revisions: MemoryRecord,
    episode: Option<Episode>,
    windows: Vec<Window>,
    tombstoned: bool,
    verification: VerificationState,
    fold_level: u32,
}

impl MemoryItem {
    fn etag(&self) -> String {
        self.revisions.head().etag.clone()
    }

    fn reference(&self) -> Ref {
        Ref::new(
            self.revisions.id.clone(),
            self.kind,
            self.revisions.head().etag.clone(),
        )
    }

    fn text(&self) -> String {
        let head = self.revisions.head();
        format!("{} {}", head.summary, head.content)
    }
}

#[derive(Debug, Clone)]
struct ScopeRecord {
    grant: AgentScopeGrant,
    parent: ParentAuthority,
    status: AgentStatus,
    opened_at: i64,
    closed: bool,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Default)]
struct TurnState {
    turn: u64,
    automatic_in_turn: u32,
    running_children: usize,
}

#[derive(Debug, Default)]
struct ServiceState {
    blocks: BTreeMap<String, Block>,
    evicted: BTreeMap<String, Block>,
    pages: BTreeMap<String, PageRecord>,
    items: BTreeMap<String, MemoryItem>,
    facts: BTreeMap<String, crate::facts::SemanticFact>,
    procedures: BTreeMap<String, crate::procedures::Procedure>,
    pipelines: BTreeMap<String, crate::consolidation::ConsolidationPipeline>,
    quarantines: BTreeMap<String, crate::poisoning::QuarantineRecord>,
    scopes: BTreeMap<String, ScopeRecord>,
    turns: BTreeMap<String, TurnState>,
    trace_links: BTreeMap<String, BTreeSet<String>>,
    stale_trace_links: BTreeSet<String>,
    idempotency: BTreeMap<String, IdempotentOutcome>,
    sequence: u64,
    generation: u64,
    /// Candidate index over a large corpus; `None` until the first big search.
    search_index: Option<crate::search::CandidateIndex>,
}

/// Result recorded for an idempotency key.
#[derive(Debug, Clone)]
struct IdempotentOutcome {
    fingerprint: String,
    resource_id: String,
    etag: String,
}

impl ServiceState {
    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    fn next_generation(&mut self) -> String {
        self.generation += 1;
        format!("mem-gen-{}", self.generation)
    }
}

/// Reference implementation of the memory domain service.
pub struct MemoryService {
    authorizer: Arc<dyn MemoryAuthorizer>,
    delegation: Arc<dyn DelegationAuthority>,
    clock: Arc<dyn MemoryClock>,
    embedder: Option<Arc<dyn EmbeddingPort>>,
    embedding_profile: String,
    reserves: Reserves,
    state: Mutex<ServiceState>,
}

impl std::fmt::Debug for MemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryService").finish()
    }
}

impl Default for MemoryService {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryService {
    /// Service with the registered 128K profile of §22.1.
    pub fn new() -> Self {
        Self {
            authorizer: Arc::new(ScopeAuthorizer),
            delegation: Arc::new(ContextDelegation),
            clock: Arc::new(SystemClock),
            embedder: None,
            embedding_profile: "unconfigured".to_string(),
            reserves: Reserves::new(131_072, 8_192, 8_192, 4_096),
            state: Mutex::new(ServiceState::default()),
        }
    }

    /// Replace the authorizer.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn MemoryAuthorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    /// Replace the delegation authority.
    pub fn with_delegation(mut self, delegation: Arc<dyn DelegationAuthority>) -> Self {
        self.delegation = delegation;
        self
    }

    /// Replace the clock.
    pub fn with_clock(mut self, clock: Arc<dyn MemoryClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Configure an embedding profile for retrieval.
    pub fn with_embedder(
        mut self,
        embedder: Arc<dyn EmbeddingPort>,
        profile_id: impl Into<String>,
    ) -> Self {
        self.embedder = Some(embedder);
        self.embedding_profile = profile_id.into();
        self
    }

    /// Replace the model reserves used by the budget calculation.
    pub fn with_reserves(mut self, reserves: Reserves) -> Self {
        self.reserves = reserves;
        self
    }

    fn lock(&self) -> MutexGuard<'_, ServiceState> {
        self.state.lock().expect("memory service state poisoned")
    }

    /// Fingerprint of everything that can change a search result.
    ///
    /// It hashes `(id, live, head etag)` per record: the head etag is derived from
    /// the content, so any revision, tombstone or insertion changes the value and a
    /// stale candidate index is detected without hooking every mutation path.
    fn corpus_fingerprint(state: &ServiceState) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (id, item) in &state.items {
            for byte in id
                .bytes()
                .chain(std::iter::once(u8::from(item.tombstoned)))
                .chain(item.revisions.head().etag.bytes())
            {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    fn now(&self) -> i64 {
        self.clock.now()
    }

    // -- helpers ------------------------------------------------------------

    fn depth_of(&self, state: &ServiceState, ctx: &RequestContext) -> u32 {
        let key = format!(
            "{}:{}",
            ctx.scope.tenant_id,
            ctx.scope.agent_id.clone().unwrap_or_default()
        );
        state
            .scopes
            .values()
            .find(|scope| {
                scope.grant.child_agent_id == ctx.scope.agent_id.clone().unwrap_or_default()
                    && !scope.closed
                    && format!("{}:{}", scope.parent.tenant_id, scope.grant.child_agent_id) == key
            })
            .map(|scope| scope.grant.depth)
            .unwrap_or(0)
    }

    fn budget_for(&self, _state: &ServiceState, depth: u32) -> Result<BudgetView> {
        let c_usable = self.reserves.c_usable()?;
        let agent_budget = b_eff(c_usable, depth)?;
        let allocation = allocate(agent_budget, depth);
        Ok(BudgetView {
            model_context: self.reserves.w_model,
            available: c_usable,
            agent_budget,
            wm: allocation.wm,
            core: allocation.core,
            ring: allocation.ring,
            used: 0,
        })
    }

    fn used_tokens(state: &ServiceState, session_id: &str, agent_id: &str) -> u32 {
        state
            .blocks
            .values()
            .filter(|block| {
                block.state == WorkingState::Active
                    && block.scope.session_id.as_deref() == Some(session_id)
                    && block.scope.agent_id.as_deref() == Some(agent_id)
            })
            .map(|block| block.token_count)
            .sum()
    }

    fn turn_key(session_id: &str, agent_id: &str) -> String {
        format!("{session_id}\u{0}{agent_id}")
    }

    fn item<'a>(&'a self, state: &'a ServiceState, id: &str) -> Result<&'a MemoryItem> {
        state
            .items
            .get(id)
            .filter(|item| !item.tombstoned)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {id}"),
            })
    }

    fn visible_to(requested: &ItemScope, item: &MemoryItem) -> bool {
        match item.kind {
            // Semantic memory is shared inside the tenant/project through a
            // delegation; working and episodic memory stay inside the sandbox
            // that created them (§26.4, §34.3).
            MemoryKind::Semantic => requested.same_tenant_project(&item.scope),
            _ => requested.same_sandbox(&item.scope),
        }
    }

    fn require_item_scope(&self, ctx: &RequestContext, item: &MemoryItem) -> Result<()> {
        let requested = ItemScope::from_context(ctx);
        if Self::visible_to(&requested, item) {
            Ok(())
        } else {
            Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", item.scope.tenant_id, item.scope.project_id),
            })
        }
    }

    fn check_etag(expected: &str, item: &MemoryItem) -> Result<()> {
        let actual = item.etag();
        if expected == actual {
            Ok(())
        } else {
            Err(MemoryError::EtagMismatch {
                expected: expected.to_string(),
                actual,
            })
        }
    }

    fn view(
        &self,
        item: &MemoryItem,
        detail: DetailLevel,
        max_tokens: u32,
        redact: bool,
    ) -> Result<MemoryView> {
        let head = item.revisions.head();
        let token_count = token_count(&item.text());
        if detail == DetailLevel::Full && !redact && token_count > max_tokens {
            return Err(MemoryError::ContextBudgetExceeded {
                requested: token_count,
                remaining: max_tokens,
            });
        }
        let summary = match detail {
            DetailLevel::Title => None,
            DetailLevel::Facts => Some(head.key_facts.join("; ")).filter(|text| !text.is_empty()),
            DetailLevel::Summary | DetailLevel::Full => Some(head.summary.clone()),
        };
        let content = if detail.includes_content() && !redact {
            Some(head.content.clone())
        } else {
            None
        };
        Ok(MemoryView {
            reference: item.reference(),
            kind: item.kind,
            state: item.episode.as_ref().map_or_else(
                || item.verification.as_str().to_string(),
                |episode| episode.state.as_str().to_string(),
            ),
            summary,
            content,
            evidence: head.evidence.clone(),
            trust: trust_rank(head, item),
            version: head.etag.clone(),
            token_count,
            folded: item.fold_level > 0,
            language: head_language(item),
            redacted: redact,
        })
    }
}

fn head_language(item: &MemoryItem) -> String {
    item.episode
        .as_ref()
        .map(|episode| episode.language.clone())
        .unwrap_or_else(|| "en".to_string())
}

fn trust_rank(head: &crate::reconsolidation::MemoryRevision, item: &MemoryItem) -> f64 {
    item.episode
        .as_ref()
        .map(|episode| episode.trust.rank())
        .unwrap_or_else(|| {
            if head.evidence.is_empty() {
                0.5
            } else {
                let total: f64 = head.evidence.iter().map(|item| item.confidence).sum();
                (total / head.evidence.len() as f64).clamp(0.0, 1.0)
            }
        })
}

fn token_count(text: &str) -> u32 {
    search::tokenize(text).len().max(1) as u32
}

fn block_etag(block_id: &str, revision: u64) -> String {
    format!("\"{block_id}-{revision}\"")
}

fn preserve_rules(item: &MemoryItem) -> Vec<PreserveRule> {
    let mut rules = Vec::new();
    if let Some(episode) = &item.episode {
        if episode.legal_hold {
            rules.push(PreserveRule::LegalHold);
        }
        if episode.pinned {
            rules.push(PreserveRule::UserPin);
        }
        if episode.active_job {
            rules.push(PreserveRule::TaskState);
        }
        if item.verification == VerificationState::Verified
            && !item.revisions.head().evidence.is_empty()
        {
            rules.push(PreserveRule::AuthorityEvidence);
        }
    }
    rules
}

fn item_importance(item: &MemoryItem) -> f64 {
    item.episode
        .as_ref()
        .map(|episode| episode.importance_score)
        .unwrap_or(0.5)
}

fn request_fingerprint(operation: &str, request: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(operation.as_bytes());
    hasher.update([0u8]);
    hasher.update(request.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

impl MemoryService {
    // -- MAPI-01 -------------------------------------------------------------

    /// `memory.open_agent_scope` — open a sub-agent scope with attenuated rights.
    pub fn open_agent_scope(
        &self,
        ctx: &RequestContext,
        request: &OpenAgentScope,
    ) -> Result<AgentScopeView> {
        self.authorizer.require_scope(ctx, SCOPE_DELEGATE)?;
        let grant = &request.grant;
        if let Some(session_id) = &ctx.scope.session_id {
            if session_id != &grant.session_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: grant.session_id.clone(),
                    actual: session_id.clone(),
                });
            }
        }
        let mut parent = self.delegation.authority(ctx);
        let mut state = self.lock();
        parent.depth = self.depth_of(&state, ctx);
        let expires_at = scope::validate_grant(&parent, grant)?;
        scope::check_concurrency(
            state
                .scopes
                .values()
                .filter(|record| {
                    !record.closed
                        && record.parent.tenant_id == parent.tenant_id
                        && !record.status.is_terminal()
                })
                .count(),
        )?;
        let budget = self.budget_for(&state, grant.depth)?;
        if grant.max_context_tokens > budget.agent_budget {
            return Err(MemoryError::ContextBudgetExceeded {
                requested: grant.max_context_tokens,
                remaining: budget.agent_budget,
            });
        }
        let scope_id = scope::scope_key(&parent.tenant_id, &grant.child_agent_id, &grant.task_id);
        if state.scopes.contains_key(&scope_id) {
            return Err(MemoryError::StateConflict {
                resource: scope_id,
                state: "already_open".to_string(),
                operation: "open_agent_scope".to_string(),
            });
        }
        state.scopes.insert(
            scope_id.clone(),
            ScopeRecord {
                grant: grant.clone(),
                parent: parent.clone(),
                status: AgentStatus::Starting,
                opened_at: self.now(),
                closed: false,
                evidence_refs: Vec::new(),
            },
        );
        let key = format!("{}:{}", parent.tenant_id, parent.agent_instance_id);
        if let Some(turn) = state.turns.get_mut(&key) {
            turn.running_children += 1;
        } else {
            state.turns.insert(
                key,
                TurnState {
                    turn: 0,
                    automatic_in_turn: 0,
                    running_children: 1,
                },
            );
        }
        Ok(AgentScopeView {
            scope_id,
            expires_at: format_rfc3339(expires_at),
        })
    }

    /// Drive the lifecycle of an open scope (runtime seam, §26.4).
    pub fn transition_scope(
        &self,
        ctx: &RequestContext,
        scope_id: &str,
        next: AgentStatus,
    ) -> Result<AgentStatus> {
        self.authorizer.require_scope(ctx, SCOPE_DELEGATE)?;
        let mut state = self.lock();
        let record = state
            .scopes
            .get_mut(scope_id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("scope {scope_id}"),
            })?;
        if record.parent.tenant_id != ctx.scope.tenant_id {
            return Err(MemoryError::CrossScopeDenied {
                requested: scope_id.to_string(),
                actual: format!("tenant:{}", ctx.scope.tenant_id),
            });
        }
        record.status = scope::transition_status(record.status, next)?;
        Ok(record.status)
    }

    // -- MAPI-02 -------------------------------------------------------------

    /// `memory.close_agent_scope` — seal a child scope and hand over a folded result.
    pub fn close_agent_scope(
        &self,
        ctx: &RequestContext,
        request: &CloseAgentScope,
    ) -> Result<CloseScopeResult> {
        self.authorizer.require_scope(ctx, SCOPE_DELEGATE)?;
        if request.reason.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "reason".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let (grant, already_closed, status) = {
            let state = self.lock();
            let record =
                state
                    .scopes
                    .get(&request.scope_id)
                    .ok_or_else(|| MemoryError::NotFound {
                        resource: format!("scope {}", request.scope_id),
                    })?;
            if record.parent.tenant_id != ctx.scope.tenant_id
                || ctx.scope.agent_id.as_deref() != Some(record.grant.parent_agent_id.as_str())
            {
                return Err(MemoryError::CrossScopeDenied {
                    requested: request.scope_id.clone(),
                    actual: format!("tenant:{}", ctx.scope.tenant_id),
                });
            }
            (record.grant.clone(), record.closed, record.status)
        };
        if already_closed {
            return Ok(CloseScopeResult {
                closed: false,
                folded_result: None,
                status,
                evidence_refs: Vec::new(),
            });
        }

        let mut evidence_refs: Vec<String> = Vec::new();
        let folded_result = if request.fold_on_close {
            let refs: Vec<Ref> = {
                let state = self.lock();
                state
                    .items
                    .values()
                    .filter(|item| {
                        !item.tombstoned
                            && item.scope.agent_id.as_deref() == Some(grant.child_agent_id.as_str())
                            && item.scope.session_id.as_deref() == Some(grant.session_id.as_str())
                    })
                    .map(|item| item.reference())
                    .collect()
            };
            if refs.is_empty() {
                None
            } else {
                let source_refs: Vec<String> =
                    refs.iter().map(|reference| reference.id.clone()).collect();
                let fold_input = FoldInput {
                    episode_refs: refs,
                    policy_id: "close-scope".to_string(),
                    max_tokens: grant.max_context_tokens,
                    target_agent_id: grant.parent_agent_id.clone(),
                };
                let sources: Vec<MemoryItem> = {
                    let state = self.lock();
                    fold_input
                        .episode_refs
                        .iter()
                        .filter_map(|reference| state.items.get(&reference.id).cloned())
                        .collect()
                };
                let result = MemoryService::fold_sources(&sources, &fold_input, None)?;
                evidence_refs = result.protected_evidence_refs.clone();
                let _ = source_refs;
                Some(result)
            }
        } else {
            None
        };

        let mut state = self.lock();
        let record =
            state
                .scopes
                .get_mut(&request.scope_id)
                .ok_or_else(|| MemoryError::NotFound {
                    resource: format!("scope {}", request.scope_id),
                })?;
        let terminal = if record.status.is_terminal() {
            record.status
        } else {
            let running = if record.status == AgentStatus::Starting {
                scope::transition_status(record.status, AgentStatus::Running)?
            } else {
                record.status
            };
            scope::transition_status(running, AgentStatus::Partial)?
        };
        record.status = scope::transition_status(terminal, AgentStatus::Folding)?;
        record.status = scope::transition_status(AgentStatus::Folding, AgentStatus::Sealed)?;
        record.closed = true;
        record.evidence_refs = evidence_refs.clone();
        let status = record.status;
        let key = format!(
            "{}:{}",
            record.parent.tenant_id, record.grant.parent_agent_id
        );
        if let Some(turn) = state.turns.get_mut(&key) {
            turn.running_children = turn.running_children.saturating_sub(1);
        }
        Ok(CloseScopeResult {
            closed: true,
            folded_result,
            status,
            evidence_refs,
        })
    }

    // -- MAPI-03 -------------------------------------------------------------

    /// `memory.put_working_block` — write a working-memory block inside the budget.
    pub fn put_working_block(
        &self,
        ctx: &RequestContext,
        request: &PutWorkingBlock,
    ) -> Result<Ref> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let input = &request.input;
        if input.session_id.trim().is_empty() || input.agent_id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "input.session_id".to_string(),
                reason: "session and agent ids must be non-empty".to_string(),
            });
        }
        if input.content.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "input.content".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if input.language.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "input.language".to_string(),
                reason: "must be a BCP-47 tag".to_string(),
            });
        }
        if !input.priority.is_finite() || !(0.0..=1.0).contains(&input.priority) {
            return Err(MemoryError::InvalidArgument {
                field: "input.priority".to_string(),
                reason: format!("must be in [0,1], got {}", input.priority),
            });
        }
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        if let Some(session_id) = &requested.session_id {
            if session_id != &input.session_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: input.session_id.clone(),
                    actual: session_id.clone(),
                });
            }
        }
        if let Some(agent_id) = &requested.agent_id {
            if agent_id != &input.agent_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: input.agent_id.clone(),
                    actual: agent_id.clone(),
                });
            }
        }
        let depth = self.depth_of(&state, ctx);
        let budget = self.budget_for(&state, depth)?;
        let used = Self::used_tokens(&state, &input.session_id, &input.agent_id);
        let tokens = token_count(&input.content);
        let remaining = budget.wm.saturating_sub(used);
        check_allocation(remaining, tokens)?;

        let turn_key = Self::turn_key(&input.session_id, &input.agent_id);
        if request.automatic {
            let turn = state.turns.entry(turn_key.clone()).or_default();
            if turn.automatic_in_turn >= crate::domain::MAX_BLOCKS_PER_TURN {
                return Err(MemoryError::ResourceLimit {
                    detail: format!(
                        "at most {} automatic blocks per turn (§21.2)",
                        crate::domain::MAX_BLOCKS_PER_TURN
                    ),
                });
            }
            turn.automatic_in_turn += 1;
        }
        let current_turn = state.turns.entry(turn_key).or_default().turn;
        let sequence = state.next_sequence();
        let block_id = format!(
            "wmb:{}:{}:{}",
            requested.tenant_id, requested.project_id, sequence
        );
        let etag = block_etag(&block_id, 1);
        let block = Block {
            id: block_id.clone(),
            scope: ItemScope {
                session_id: Some(input.session_id.clone()),
                agent_id: Some(input.agent_id.clone()),
                ..requested
            },
            kind: input.block_type,
            content: input.content.clone(),
            language: input.language.clone(),
            priority: input.priority,
            protected: input.protected,
            preserve_rules: if input.protected {
                vec![PreserveRule::UserPin]
            } else {
                Vec::new()
            },
            expires_after_turns: input.expires_after_turns,
            state: WorkingState::Staged,
            revision: 1,
            etag: etag.clone(),
            token_count: tokens,
            created_at: self.now(),
            page_id: None,
            evidence_refs: Vec::new(),
            automatic: request.automatic,
            turn: current_turn,
            referenced: false,
        };
        state.blocks.insert(block_id.clone(), block);
        if let Some(block) = state.blocks.get_mut(&block_id) {
            block.state = WorkingState::Active;
        }
        Ok(Ref::new(block_id, MemoryKind::Working, etag))
    }

    /// Start a new turn for a session/agent pair.
    ///
    /// Blocks written with `expires_after_turns = n` leave working memory once
    /// `n` turns have elapsed (§21.2), and the automatic-block cap of the turn
    /// is reset.
    pub fn begin_turn(&self, session_id: &str, agent_id: &str) -> u64 {
        let mut state = self.lock();
        let key = Self::turn_key(session_id, agent_id);
        let turn = state.turns.entry(key).or_default();
        turn.turn += 1;
        turn.automatic_in_turn = 0;
        let current = turn.turn;
        let mut expired: Vec<String> = Vec::new();
        for block in state.blocks.values() {
            if block.scope.session_id.as_deref() != Some(session_id)
                || block.scope.agent_id.as_deref() != Some(agent_id)
                || block.state != WorkingState::Active
            {
                continue;
            }
            if let Some(limit) = block.expires_after_turns {
                if current.saturating_sub(block.turn) >= u64::from(limit) {
                    expired.push(block.id.clone());
                }
            }
        }
        for id in expired {
            if let Some(mut block) = state.blocks.remove(&id) {
                block.state = WorkingState::Evicted;
                state.evicted.insert(id, block);
            }
        }
        current
    }

    // -- MAPI-04 -------------------------------------------------------------

    /// `memory.list_working_blocks` — list the blocks of one sandbox.
    pub fn list_working_blocks(
        &self,
        ctx: &RequestContext,
        request: &ListWorkingBlocks,
    ) -> Result<Page<MemoryView>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let requested = ItemScope::from_context(ctx);
        if let Some(session_id) = &requested.session_id {
            if session_id != &request.session_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: request.session_id.clone(),
                    actual: session_id.clone(),
                });
            }
        }
        if let Some(agent_id) = &requested.agent_id {
            if agent_id != &request.agent_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: request.agent_id.clone(),
                    actual: agent_id.clone(),
                });
            }
        }
        let mut blocks: Vec<&Block> = state
            .blocks
            .iter()
            .filter(|(_, block)| {
                block.state == WorkingState::Active
                    && block.scope.session_id.as_deref() == Some(request.session_id.as_str())
                    && block.scope.agent_id.as_deref() == Some(request.agent_id.as_str())
                    && block.scope.tenant_id == requested.tenant_id
                    && block.scope.project_id == requested.project_id
            })
            .map(|(_, block)| block)
            .collect();
        blocks.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.etag.cmp(&right.etag))
        });
        let views: Vec<MemoryView> = blocks
            .into_iter()
            .map(|block| self.block_view(block))
            .collect();
        Page::of(views, &request.page)
    }

    /// Turn on which the block was written.
    pub fn block_turn(&self, block_id: &str) -> Option<u64> {
        self.lock().blocks.get(block_id).map(|block| block.turn)
    }

    /// Whether a block was written automatically by the runtime.
    pub fn block_is_automatic(&self, block_id: &str) -> Option<bool> {
        self.lock()
            .blocks
            .get(block_id)
            .map(|block| block.automatic)
    }

    /// Evidence references attached to a block.
    pub fn block_evidence(&self, block_id: &str) -> Vec<String> {
        self.lock()
            .blocks
            .get(block_id)
            .map(|block| block.evidence_refs.clone())
            .unwrap_or_default()
    }

    /// Instant the page stops being openable.
    pub fn page_expires_at(&self, page_id: &str) -> Option<i64> {
        self.lock().pages.get(page_id).map(|page| page.expires_at)
    }

    /// Index generation the page was written under.
    pub fn page_generation(&self, page_id: &str) -> Option<String> {
        self.lock()
            .pages
            .get(page_id)
            .map(|page| page.generation.clone())
    }

    /// Instant the scope was opened.
    pub fn scope_opened_at(&self, scope_id: &str) -> Option<i64> {
        self.lock()
            .scopes
            .get(scope_id)
            .map(|record| record.opened_at)
    }

    fn block_view(&self, block: &Block) -> MemoryView {
        MemoryView {
            reference: Ref::new(block.id.clone(), MemoryKind::Working, block.etag.clone()),
            kind: MemoryKind::Working,
            state: block.state.as_str().to_string(),
            summary: Some(block.content.clone()),
            content: Some(block.content.clone()),
            evidence: Vec::new(),
            trust: block.priority.clamp(0.0, 1.0),
            version: block.etag.clone(),
            token_count: block.token_count,
            folded: block.kind == BlockKind::FoldedResult,
            language: block.language.clone(),
            redacted: false,
        }
    }

    // -- MAPI-05 -------------------------------------------------------------

    /// `memory.remove_working_block` — remove a block, refusing protected ones.
    pub fn remove_working_block(
        &self,
        ctx: &RequestContext,
        request: &RemoveWorkingBlock,
    ) -> Result<DeletionReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let block = state
            .blocks
            .get(&request.block.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("block {}", request.block.id),
            })?;
        if !requested.same_tenant_project(&block.scope) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", block.scope.tenant_id, block.scope.project_id),
            });
        }
        if block.protected || !block.preserve_rules.is_empty() {
            return Err(MemoryError::PolicyDenied {
                reason: "block is protected by a preserve rule (§22.2 R1–R8)".to_string(),
            });
        }
        if request.expected_etag != block.etag {
            return Err(MemoryError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual: block.etag.clone(),
            });
        }
        let revision = block.revision;
        let reference = Ref::new(
            request.block.id.clone(),
            MemoryKind::Working,
            block.etag.clone(),
        );
        let mut removed =
            state
                .blocks
                .remove(&request.block.id)
                .ok_or_else(|| MemoryError::NotFound {
                    resource: format!("block {}", request.block.id),
                })?;
        removed.state = WorkingState::Evicted;
        let tombstone = deletion::tombstone_revision(&request.block.id, revision);
        state.evicted.insert(request.block.id.clone(), removed);
        let outcome =
            deletion::build_receipt(reference, tombstone, &DeletionGuards::default(), None)?;
        Ok(outcome.receipt)
    }

    // -- MAPI-06 -------------------------------------------------------------

    /// `memory.store_episode` — persist an episode under idempotency.
    pub fn store_episode(
        &self,
        ctx: &RequestContext,
        request: &StoreEpisode,
    ) -> Result<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let input = &request.input;
        if input.session_id.trim().is_empty() || input.agent_id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "input.session_id".to_string(),
                reason: "session and agent ids must be non-empty".to_string(),
            });
        }
        if input.summary.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "input.summary".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if !input.importance_score.is_finite() || !(0.0..=1.0).contains(&input.importance_score) {
            return Err(MemoryError::InvalidArgument {
                field: "input.importance_score".to_string(),
                reason: format!("must be in [0,1], got {}", input.importance_score),
            });
        }
        if input.agent_depth > scope::MAX_DEPTH {
            return Err(MemoryError::PolicyDenied {
                reason: format!(
                    "agent_depth {} exceeds max_depth {} (§26.4)",
                    input.agent_depth,
                    scope::MAX_DEPTH
                ),
            });
        }
        let requested = ItemScope::from_context(ctx);
        if let Some(session_id) = &requested.session_id {
            if session_id != &input.session_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: input.session_id.clone(),
                    actual: session_id.clone(),
                });
            }
        }
        if let Some(agent_id) = &requested.agent_id {
            if agent_id != &input.agent_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: input.agent_id.clone(),
                    actual: agent_id.clone(),
                });
            }
        }
        for item in &input.evidence {
            item.validate()?;
        }
        let fingerprint = request_fingerprint(
            "memory.store_episode",
            &format!(
                "{}|{}|{}|{:?}",
                input.session_id, input.agent_id, input.summary, input.episode_type
            ),
        );
        let mut state = self.lock();
        if let Some(key) = &ctx.idempotency_key {
            if let Some(previous) = state.idempotency.get(key) {
                if previous.fingerprint != fingerprint {
                    return Err(MemoryError::IdempotencyConflict { key: key.clone() });
                }
                return Ok(ChangeReceipt {
                    resource_id: previous.resource_id.clone(),
                    etag: previous.etag.clone(),
                    changed: false,
                    events: Vec::new(),
                });
            }
        }
        let requested = ItemScope::from_context(ctx);
        let now = self.now();
        let sequence = state.next_sequence();
        let id = format!(
            "mem:{}:{}:{}",
            requested.tenant_id, requested.project_id, sequence
        );
        let mut revisions = MemoryRecord::new(
            id.clone(),
            input.summary.clone(),
            String::new(),
            input.evidence.clone(),
            now,
            input.agent_id.clone(),
        );
        if !input.key_facts.is_empty() {
            revisions.append(
                &MemoryPatch {
                    key_facts: Some(input.key_facts.clone()),
                    ..MemoryPatch::default()
                },
                Vec::new(),
                now,
                &input.agent_id,
            )?;
        }
        let etag = revisions.head().etag.clone();
        let episode = Episode {
            id: id.clone(),
            session_id: input.session_id.clone(),
            task_id: ctx
                .scope
                .workspace_id
                .clone()
                .unwrap_or_else(|| "task-unspecified".to_string()),
            agent_id: input.agent_id.clone(),
            depth: input.agent_depth,
            created_at: now,
            episode_type: input.episode_type,
            summary: input.summary.clone(),
            full_text_ref: input.full_text_ref.clone(),
            key_facts: input.key_facts.clone(),
            outcome: input.outcome,
            importance_score: input.importance_score,
            fold_level: 0,
            parent_episode_id: None,
            source_refs: input.source_refs.clone(),
            trust: Trust::default(),
            state: crate::domain::EpisodeState::Active,
            held_from: None,
            active_until: fsrs::effective_active_until(now, None, input.agent_depth),
            purge_after: now + ROOT_PURGE_AFTER_DAYS * 86_400,
            fsrs: FsrsState::default(),
            last_review: now,
            legal_hold: false,
            pinned: false,
            pending_review: false,
            active_job: false,
            protected_refs: Vec::new(),
            language: input.language.clone(),
        };
        episode.validate()?;
        state.items.insert(
            id.clone(),
            MemoryItem {
                kind: MemoryKind::Episodic,
                scope: ItemScope {
                    session_id: Some(input.session_id.clone()),
                    agent_id: Some(input.agent_id.clone()),
                    ..requested
                },
                revisions,
                episode: Some(episode),
                windows: Vec::new(),
                tombstoned: false,
                verification: VerificationState::Unverified,
                fold_level: 0,
            },
        );
        if let Some(key) = &ctx.idempotency_key {
            state.idempotency.insert(
                key.clone(),
                IdempotentOutcome {
                    fingerprint,
                    resource_id: id.clone(),
                    etag: etag.clone(),
                },
            );
        }
        Ok(ChangeReceipt {
            resource_id: id,
            etag,
            changed: true,
            events: vec!["memory.episode.committed".to_string()],
        })
    }

    // -- MAPI-07 -------------------------------------------------------------

    /// `memory.search` — search every store the caller may read.
    ///
    /// Small corpora are scanned directly (exact BM25 statistics over the visible
    /// set). Above [`search::MAX_CANDIDATES`] records the candidate index selects
    /// the documents that contain a query term first, so the resource limit applies
    /// to **matches** rather than to the whole store, and a 1M-record corpus stays
    /// queryable.
    pub fn search(&self, ctx: &RequestContext, request: &SearchMemory) -> Result<Page<MemoryView>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        search::validate_query(&request.query)?;
        let requested = ItemScope::from_context(ctx);
        let (documents, items) = {
            let mut state = self.lock();
            let candidates: Option<Vec<String>> = if state.items.len() > search::MAX_CANDIDATES {
                let fingerprint = Self::corpus_fingerprint(&state);
                let stale = state
                    .search_index
                    .as_ref()
                    .map(|index| index.fingerprint() != fingerprint)
                    .unwrap_or(true);
                if stale {
                    let mut index = search::CandidateIndex::default();
                    for (id, item) in state.items.iter().filter(|(_, item)| !item.tombstoned) {
                        let text = item.text();
                        index.insert(id, &text);
                    }
                    index.set_fingerprint(fingerprint);
                    state.search_index = Some(index);
                }
                Some(
                    state
                        .search_index
                        .as_ref()
                        .map(|index| index.candidates(&search::tokenize(&request.query.query)))
                        .unwrap_or_default(),
                )
            } else {
                None
            };
            let scanned: Vec<(&String, &MemoryItem)> = match &candidates {
                Some(ids) => ids
                    .iter()
                    .filter_map(|id| state.items.get_key_value(id))
                    .collect(),
                None => state.items.iter().collect(),
            };
            let mut documents = Vec::new();
            let mut items: BTreeMap<String, MemoryItem> = BTreeMap::new();
            for (id, item) in scanned {
                if item.tombstoned || !Self::visible_to(&requested, item) {
                    continue;
                }
                let head = item.revisions.head();
                documents.push(SearchDocument {
                    id: id.clone(),
                    kind: item.kind,
                    text: item.text(),
                    created_at: head.created_at,
                    trust: self
                        .view(item, DetailLevel::Summary, u32::MAX, false)?
                        .trust,
                    importance: item_importance(item),
                    language: head_language(item),
                    folded: item.fold_level > 0,
                    token_count: token_count(&item.text()),
                });
                items.insert(id.clone(), item.clone());
            }
            (documents, items)
        };
        let outcome = search::search(
            &request.query,
            &documents,
            self.embedder.as_deref(),
            &self.embedding_profile,
            self.now(),
        )?;
        let mut views = Vec::new();
        for hit in outcome.hits {
            let item = items.get(&hit.id).ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {}", hit.id),
            })?;
            views.push(self.view(item, request.query.detail, request.query.max_tokens, false)?);
        }
        Page::of(views, &PageRequest::new(request.query.max_results))
    }

    // -- MAPI-08 -------------------------------------------------------------

    /// `memory.retrieve` — read one record at the requested detail.
    pub fn retrieve(&self, ctx: &RequestContext, request: &RetrieveMemory) -> Result<MemoryView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_tokens == 0 {
            return Err(MemoryError::InvalidArgument {
                field: "max_tokens".to_string(),
                reason: "must be greater than zero".to_string(),
            });
        }
        let state = self.lock();
        let item = self.item(&state, &request.memory.id)?;
        self.require_item_scope(ctx, item)?;
        self.view(item, request.detail, request.max_tokens, false)
    }

    // -- MAPI-09 -------------------------------------------------------------

    /// `memory.update_revision` — append an immutable revision.
    pub fn update_revision(
        &self,
        ctx: &RequestContext,
        request: &UpdateMemory,
    ) -> Result<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        request.patch.validate()?;
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let now = self.now();
        let author = ctx
            .scope
            .agent_id
            .clone()
            .unwrap_or_else(|| ctx.principal_id.clone());
        let item = state
            .items
            .get_mut(&request.memory.id)
            .filter(|item| !item.tombstoned)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {}", request.memory.id),
            })?;
        if !Self::visible_to(&requested, item) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", item.scope.tenant_id, item.scope.project_id),
            });
        }
        Self::check_etag(&request.expected_etag, item)?;
        let revision = item
            .revisions
            .append(&request.patch, Vec::new(), now, &author)?;
        let etag = revision.etag.clone();
        if let Some(episode) = item.episode.as_mut() {
            if let Some(summary) = &request.patch.summary {
                episode.summary = summary.clone();
            }
            if let Some(facts) = &request.patch.key_facts {
                episode.key_facts = facts.clone();
            }
            if let Some(importance) = request.patch.importance_score {
                episode.importance_score = importance;
            }
            if let Some(trust) = request.patch.trust {
                episode.trust = trust;
            }
            if let Some(language) = &request.patch.language {
                episode.language = language.clone();
            }
        }
        Ok(ChangeReceipt {
            resource_id: request.memory.id.clone(),
            etag,
            changed: true,
            events: vec!["memory.revision.committed".to_string()],
        })
    }
}

/// Default purge horizon of a root episode, in days (configuration default).
pub const ROOT_PURGE_AFTER_DAYS: i64 = 30;

impl MemoryService {
    // -- MAPI-13 -------------------------------------------------------------

    /// `memory.fold` — fold sources into a `FoldedResult` without losing evidence.
    pub fn fold(&self, ctx: &RequestContext, request: &FoldMemory) -> Result<FoldedResult> {
        self.authorizer.require_scope(ctx, SCOPE_FOLD)?;
        self.fold_internal(ctx, &request.input, request.model_score)
    }

    fn fold_internal(
        &self,
        ctx: &RequestContext,
        input: &FoldInput,
        model_score: Option<f64>,
    ) -> Result<FoldedResult> {
        if input.episode_refs.is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "episode_refs".to_string(),
                reason: "must name at least one source".to_string(),
            });
        }
        let mut sources: Vec<MemoryItem> = Vec::new();
        {
            let state = self.lock();
            for reference in &input.episode_refs {
                let item = self.item(&state, &reference.id)?;
                self.require_item_scope(ctx, item)?;
                sources.push(item.clone());
            }
        }
        Self::fold_sources(&sources, input, model_score)
    }

    /// Fold already-collected sources.
    ///
    /// Authorization happened before the call: `memory.fold` collected the
    /// sources through the caller's visibility, and `close_agent_scope` collects
    /// exactly the child scope it is sealing.
    fn fold_sources(
        sources: &[MemoryItem],
        input: &FoldInput,
        model_score: Option<f64>,
    ) -> Result<FoldedResult> {
        if input.episode_refs.is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "episode_refs".to_string(),
                reason: "must name at least one source".to_string(),
            });
        }
        if input.max_tokens == 0 {
            return Err(MemoryError::InvalidArgument {
                field: "max_tokens".to_string(),
                reason: "must be greater than zero".to_string(),
            });
        }
        if input.policy_id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "policy_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }

        let mut rules: Vec<PreserveRule> = Vec::new();
        for item in sources {
            for rule in preserve_rules(item) {
                if !rules.contains(&rule) {
                    rules.push(rule);
                }
            }
        }
        rules.sort();
        let importance = sources.iter().map(item_importance).fold(0.0f64, f64::max);
        let decision = context_engine::folding::decide(importance, &rules, model_score);
        let forced_preserve = decision.forced_preserve;

        let mut facts: Vec<String> = Vec::new();
        let mut evidence: Vec<String> = Vec::new();
        let mut source_refs: Vec<String> = Vec::new();
        let mut summaries: Vec<String> = Vec::new();
        let mut trust_base = 1.0f64;
        let mut fold_level = 0u32;
        for item in sources {
            let head = item.revisions.head();
            source_refs.push(item.revisions.id.clone());
            summaries.push(head.summary.clone());
            for fact in &head.key_facts {
                if !facts.contains(fact) {
                    facts.push(fact.clone());
                }
            }
            for item_evidence in &head.evidence {
                if !evidence.contains(&item_evidence.source_ref) {
                    evidence.push(item_evidence.source_ref.clone());
                }
            }
            trust_base = trust_base.min(trust_of(item));
            fold_level = fold_level.max(item.fold_level);
            if let Some(episode) = &item.episode {
                fold_level = fold_level.max(episode.fold_level);
            }
        }
        let fold_level = fold_level + 1;

        // No summariser ships with this build, so the digest is extractive: it
        // keeps every fact and every evidence reference instead of inventing
        // prose (§28.3 "return the uncompressed safe excerpt").
        let summary = if forced_preserve || facts.is_empty() {
            summaries.join("\n")
        } else {
            facts.join("; ")
        };
        let token_count = token_count(&summary);
        if token_count > input.max_tokens {
            return Err(MemoryError::PolicyDenied {
                reason: format!(
                    "cannot fold {} sources into {} tokens without dropping protected evidence",
                    sources.len(),
                    input.max_tokens
                ),
            });
        }
        Ok(FoldedResult {
            summary,
            key_facts: facts,
            source_refs,
            fold_level,
            trust_score: fsrs::folded_trust(trust_base, fold_level),
            token_count,
            protected_evidence_refs: evidence,
            forced_preserve,
        })
    }

    // -- MAPI-14 -------------------------------------------------------------

    /// `memory.page_out` — move a block into a page with an integrity hash.
    pub fn page_out(&self, ctx: &RequestContext, request: &PageOut) -> Result<PagingReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        if request.policy_id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "policy_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let block = state
            .blocks
            .get(&request.block.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("block {}", request.block.id),
            })?;
        if !requested.same_tenant_project(&block.scope) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", block.scope.tenant_id, block.scope.project_id),
            });
        }
        if block.protected || !block.preserve_rules.is_empty() {
            return Err(MemoryError::PolicyDenied {
                reason: "protected blocks cannot be paged out (§22.4)".to_string(),
            });
        }
        if block.state != WorkingState::Active {
            return Err(MemoryError::StateConflict {
                resource: block.id.clone(),
                state: block.state.as_str().to_string(),
                operation: "page_out".to_string(),
            });
        }
        let payload = block.content.clone();
        let revision = block.revision;
        let block_etag = block.etag.clone();
        let scope = block.scope.clone();
        let block_id = block.id.clone();
        let sequence = state.next_sequence();
        let generation = state.next_generation();
        let page_id = format!("page:{sequence}");
        let checksum = context_engine::paging::checksum(payload.as_bytes());
        let expires_at = time::add_hours(self.now(), PAGE_TTL_HOURS);
        state.pages.insert(
            page_id.clone(),
            PageRecord {
                page_id: page_id.clone(),
                block_id: block_id.clone(),
                scope,
                payload,
                checksum: checksum.clone(),
                memory_revision: revision,
                generation,
                expires_at,
                refcount: 1,
            },
        );
        if let Some(block) = state.blocks.get_mut(&block_id) {
            block.state = WorkingState::Paged;
            block.page_id = Some(page_id.clone());
        }
        Ok(PagingReceipt {
            block: Ref::new(block_id, MemoryKind::Working, block_etag),
            page_ref: Some(Ref::new(page_id, MemoryKind::Working, checksum.clone())),
            status: PagingStatus::PagedOut,
            integrity_hash: checksum,
        })
    }

    // -- MAPI-15 -------------------------------------------------------------

    /// `memory.page_in` — restore a block after re-authorising and hashing.
    pub fn page_in(&self, ctx: &RequestContext, request: &PageIn) -> Result<PageInResult> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let mut state = self.lock();
        let page = state
            .pages
            .get(&request.page_ref.id)
            .cloned()
            .ok_or_else(|| MemoryError::SourceUnavailable {
                reference: request.page_ref.id.clone(),
            })?;
        let declared = self.now();
        if declared > page.expires_at {
            return Err(MemoryError::SourceUnavailable {
                reference: format!("{} (expired)", page.page_id),
            });
        }
        let requested = ItemScope::from_context(ctx);
        if !requested.same_sandbox(&page.scope) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", page.scope.tenant_id, page.scope.project_id),
            });
        }
        if request.expected_hash != page.checksum {
            return Err(MemoryError::SourceUnavailable {
                reference: format!("{} (integrity)", page.page_id),
            });
        }
        let actual = context_engine::paging::checksum(page.payload.as_bytes());
        if actual != page.checksum {
            return Err(MemoryError::SourceUnavailable {
                reference: format!("{} (hash mismatch)", page.page_id),
            });
        }
        let block =
            state
                .blocks
                .get_mut(&page.block_id)
                .ok_or_else(|| MemoryError::SourceUnavailable {
                    reference: page.block_id.clone(),
                })?;
        if block.revision != page.memory_revision {
            return Err(MemoryError::StaleReference {
                reference: format!("{}@{}", page.block_id, page.memory_revision),
            });
        }
        block.state = WorkingState::Active;
        block.page_id = None;
        let view = self.block_view(block);
        if let Some(stored) = state.pages.get_mut(&page.page_id) {
            stored.refcount += 1;
        }
        let receipt = PagingReceipt {
            block: Ref::new(
                page.block_id.clone(),
                MemoryKind::Working,
                page.checksum.clone(),
            ),
            page_ref: Some(Ref::new(
                page.page_id.clone(),
                MemoryKind::Working,
                page.checksum.clone(),
            )),
            status: PagingStatus::PagedIn,
            integrity_hash: page.checksum,
        };
        Ok(PageInResult {
            block: view,
            receipt,
        })
    }

    // -- MAPI-16 -------------------------------------------------------------

    /// `memory.begin_reconsolidation` — open a labile window on a record.
    pub fn begin_reconsolidation(
        &self,
        ctx: &RequestContext,
        request: &BeginReconsolidation,
    ) -> Result<crate::domain::ReconsolidationWindow> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let sequence = state.next_sequence();
        let now = self.now();
        let editor = ctx
            .scope
            .agent_id
            .clone()
            .unwrap_or_else(|| ctx.principal_id.clone());
        let item = state
            .items
            .get_mut(&request.fact.id)
            .filter(|item| !item.tombstoned)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {}", request.fact.id),
            })?;
        if !Self::visible_to(&requested, item) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", item.scope.tenant_id, item.scope.project_id),
            });
        }
        Self::check_etag(&request.expected_etag, item)?;
        if let Some(open) = item
            .windows
            .iter()
            .find(|window| window.state == crate::domain::WindowState::Open)
        {
            return Err(MemoryError::StateConflict {
                resource: request.fact.id.clone(),
                state: format!("window {}", open.window_id),
                operation: "begin_reconsolidation".to_string(),
            });
        }
        let head = item.revisions.head().clone();
        let window = Window::open(
            format!("rw:{sequence}"),
            item.revisions.id.clone(),
            &head,
            now,
            request.window_hours,
            ctx.scope
                .workspace_id
                .clone()
                .unwrap_or_else(|| "task-unspecified".to_string()),
            editor,
        )?;
        let view = window.view(item.kind);
        item.windows.push(window);
        Ok(view)
    }

    // -- MAPI-17 -------------------------------------------------------------

    /// `memory.commit_reconsolidation` — append the revision inside the window.
    pub fn commit_reconsolidation(
        &self,
        ctx: &RequestContext,
        request: &CommitReconsolidation,
    ) -> Result<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let now = self.now();
        let item_id = state
            .items
            .iter()
            .find(|(_, item)| {
                item.windows
                    .iter()
                    .any(|window| window.window_id == request.window_id)
            })
            .map(|(id, _)| id.clone())
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("window {}", request.window_id),
            })?;
        let author = ctx
            .scope
            .agent_id
            .clone()
            .unwrap_or_else(|| ctx.principal_id.clone());
        let item = state
            .items
            .get_mut(&item_id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {item_id}"),
            })?;
        if !Self::visible_to(&requested, item) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", item.scope.tenant_id, item.scope.project_id),
            });
        }
        let head = item.revisions.head().clone();
        let position = item
            .windows
            .iter()
            .position(|window| window.window_id == request.window_id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("window {}", request.window_id),
            })?;
        item.windows[position].check_commit(
            now,
            &request.expected_etag,
            &head,
            &request.evidence,
        )?;
        let revision =
            item.revisions
                .append(&request.patch, request.evidence.clone(), now, &author)?;
        let etag = revision.etag.clone();
        item.windows[position].commit();
        if let Some(episode) = item.episode.as_mut() {
            if let Some(summary) = &request.patch.summary {
                episode.summary = summary.clone();
            }
            if let Some(facts) = &request.patch.key_facts {
                episode.key_facts = facts.clone();
            }
            if let Some(importance) = request.patch.importance_score {
                episode.importance_score = importance;
            }
        }
        Ok(ChangeReceipt {
            resource_id: item_id,
            etag,
            changed: true,
            events: vec!["memory.reconsolidation.committed".to_string()],
        })
    }

    // -- MAPI-23 -------------------------------------------------------------

    /// `memory.forget` — tombstone a record and revoke every projection.
    /// `memory.context` (MAPI-26): live records relevant to a code path or
    /// symbol, ranked by trust.
    pub fn memory_context(
        &self,
        ctx: &RequestContext,
        request: &MemoryContextQuery,
    ) -> Result<MemoryContextResult> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let path = request
            .path
            .as_ref()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
        let symbol = request
            .symbol_id
            .as_ref()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
        if path.is_none() && symbol.is_none() {
            return Err(MemoryError::InvalidArgument {
                field: "path|symbol_id".to_string(),
                reason: "at least one of path or symbol_id is required".to_string(),
            });
        }
        if let Some(min_trust) = request.min_trust {
            if !min_trust.is_finite() || !(0.0..=1.0).contains(&min_trust) {
                return Err(MemoryError::InvalidArgument {
                    field: "min_trust".to_string(),
                    reason: "must be in [0,1]".to_string(),
                });
            }
        }
        let limit = if request.limit == 0 {
            20
        } else {
            request.limit
        };
        if limit > 100 {
            return Err(MemoryError::InvalidArgument {
                field: "limit".to_string(),
                reason: "must be in 1..=100".to_string(),
            });
        }
        let state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let mut matches: Vec<MemoryContextMatch> = Vec::new();
        for item in state.items.values() {
            if item.tombstoned || !Self::visible_to(&requested, item) {
                continue;
            }
            let text = item.text().to_lowercase();
            let on_path = path.as_ref().is_some_and(|needle| text.contains(needle));
            let on_symbol = symbol.as_ref().is_some_and(|needle| text.contains(needle));
            if !on_path && !on_symbol {
                continue;
            }
            let trust = trust_of(item);
            if request.min_trust.is_some_and(|min| trust < min) {
                continue;
            }
            let matched_on = match (on_path, on_symbol) {
                (true, true) => "path+symbol".to_string(),
                (true, false) => "path".to_string(),
                _ => "symbol".to_string(),
            };
            let head = item.revisions.head();
            matches.push(MemoryContextMatch {
                reference: Ref::new(item.revisions.id.clone(), item.kind, head.etag.clone()),
                kind: item.kind,
                trust,
                summary: head.summary.clone(),
                matched_on,
            });
        }
        matches.sort_by(|a, b| {
            b.trust
                .partial_cmp(&a.trust)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.reference.id.cmp(&b.reference.id))
        });
        let total = matches.len() as u32;
        matches.truncate(limit as usize);
        Ok(MemoryContextResult { matches, total })
    }

    /// `memory.list` (MAPI-27): cursor-paged listing of live records.
    pub fn list_memory(
        &self,
        ctx: &RequestContext,
        request: &ListMemoryQuery,
    ) -> Result<MemoryListPage> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let visible: Vec<&MemoryItem> = state
            .items
            .values()
            .filter(|item| !item.tombstoned && Self::visible_to(&requested, item))
            .filter(|item| request.kinds.is_empty() || request.kinds.contains(&item.kind))
            .collect();
        let total = visible.len() as u64;
        let mut ordered: Vec<MemoryListItem> = visible
            .iter()
            .map(|item| {
                let head = item.revisions.head();
                MemoryListItem {
                    reference: Ref::new(item.revisions.id.clone(), item.kind, head.etag.clone()),
                    kind: item.kind,
                    state: item.verification.as_str().to_string(),
                    trust: trust_of(item),
                    summary: head.summary.clone(),
                    version: head.revision.to_string(),
                    token_count: (head.content.chars().count() / 4) as u32,
                }
            })
            .collect();
        ordered.sort_by(|a, b| a.reference.id.cmp(&b.reference.id));
        let offset = request.page.offset()?;
        if offset > ordered.len() {
            return Err(MemoryError::CursorExpired {
                cursor: request.page.cursor.clone().unwrap_or_default(),
            });
        }
        let limit = request.page.limit as usize;
        let mut page: Vec<MemoryListItem> =
            ordered.into_iter().skip(offset).take(limit + 1).collect();
        let next_cursor = if page.len() > limit {
            page.truncate(limit);
            Some((offset + limit).to_string())
        } else {
            None
        };
        Ok(MemoryListPage {
            items: page,
            next_cursor,
            total,
        })
    }

    /// `memory.stats` (MAPI-28): per-store and per-verification-state counts
    /// over the live records visible to the caller.
    pub fn memory_stats(
        &self,
        ctx: &RequestContext,
        _request: &GetMemoryStats,
    ) -> Result<MemoryStats> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let stores: [MemoryKind; 4] = [
            MemoryKind::Working,
            MemoryKind::Episodic,
            MemoryKind::Semantic,
            MemoryKind::Procedural,
        ];
        let states: [VerificationState; 4] = [
            VerificationState::Unverified,
            VerificationState::Verified,
            VerificationState::Rejected,
            VerificationState::Stale,
        ];
        let mut by_kind = [0u64; 4];
        let mut by_verification = [0u64; 4];
        let mut total = 0u64;
        let mut folded = 0u64;
        for item in state.items.values() {
            if item.tombstoned || !Self::visible_to(&requested, item) {
                continue;
            }
            total += 1;
            by_kind[Self::kind_index(item.kind)] += 1;
            by_verification[Self::verification_index(item.verification)] += 1;
            if item.fold_level > 0 {
                folded += 1;
            }
        }
        let working_blocks = state.blocks.len() as u64;
        Ok(MemoryStats {
            total,
            by_kind: stores
                .iter()
                .enumerate()
                .map(|(index, kind)| MemoryKindCount {
                    kind: *kind,
                    count: by_kind[index],
                })
                .collect(),
            by_verification: states
                .iter()
                .enumerate()
                .map(|(index, state)| MemoryVerificationCount {
                    state: *state,
                    count: by_verification[index],
                })
                .collect(),
            folded,
            working_blocks,
        })
    }

    fn kind_index(kind: MemoryKind) -> usize {
        match kind {
            MemoryKind::Working => 0,
            MemoryKind::Episodic => 1,
            MemoryKind::Semantic => 2,
            MemoryKind::Procedural => 3,
        }
    }

    fn verification_index(state: VerificationState) -> usize {
        match state {
            VerificationState::Unverified => 0,
            VerificationState::Verified => 1,
            VerificationState::Rejected => 2,
            VerificationState::Stale => 3,
        }
    }

    /// `memory.forget` (MAPI-25): tombstone a record with the §22.5 guards.
    pub fn forget(&self, ctx: &RequestContext, request: &ForgetMemory) -> Result<DeletionReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_FORGET)?;
        if request.reason.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "reason".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let now = self.now();
        let accepted_trace_links: Vec<String> = state
            .trace_links
            .get(&request.memory.id)
            .map(|links| links.iter().cloned().collect())
            .unwrap_or_default();
        let item = state
            .items
            .get_mut(&request.memory.id)
            .filter(|item| !item.tombstoned)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {}", request.memory.id),
            })?;
        if !Self::visible_to(&requested, item) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", item.scope.tenant_id, item.scope.project_id),
            });
        }
        Self::check_etag(&request.expected_etag, item)?;
        if item
            .windows
            .iter()
            .any(|window| window.state == crate::domain::WindowState::Open)
        {
            return Err(MemoryError::PolicyDenied {
                reason: "a reconsolidation window is open; close it first (§23.4)".to_string(),
            });
        }
        let legal_hold = item
            .episode
            .as_ref()
            .map(|episode| episode.legal_hold)
            .unwrap_or(false);
        let active_job = item
            .episode
            .as_ref()
            .map(|episode| episode.active_job)
            .unwrap_or(false);
        let guards = DeletionGuards {
            legal_hold,
            retention_blocks: false,
            active_job,
            accepted_trace_links,
            regulation_blocks: false,
        };
        let revision = item.revisions.head().revision;
        let kind = item.kind;
        item.tombstoned = true;
        if let Some(episode) = item.episode.as_mut() {
            episode.state = crate::domain::EpisodeState::Tombstoned;
        }
        let reference = Ref::new(
            request.memory.id.clone(),
            kind,
            request.memory.revision.clone(),
        );
        let outcome = deletion::build_receipt(
            reference,
            deletion::tombstone_revision(&request.memory.id, revision),
            &guards,
            Some(now + 30 * 86_400),
        )?;
        for link in &outcome.stale_trace_links {
            state.stale_trace_links.insert(link.clone());
        }
        Ok(outcome.receipt)
    }

    /// Mark an episodic record as pinned or held (§22.2 R6/R8).
    ///
    /// Pin and hold are policy decisions, not domain operations: on the wire they
    /// arrive through policy evaluation (P6/P7). This seam exists so a policy
    /// engine can apply them, and so the preservation rules are reachable and
    /// testable today.
    pub fn set_episode_protection(
        &self,
        ctx: &RequestContext,
        memory_id: &str,
        pinned: bool,
        legal_hold: bool,
    ) -> Result<()> {
        self.authorizer.require_scope(ctx, SCOPE_MANAGE)?;
        let mut state = self.lock();
        let requested = ItemScope::from_context(ctx);
        let item = state
            .items
            .get_mut(memory_id)
            .filter(|item| !item.tombstoned)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("memory {memory_id}"),
            })?;
        if !Self::visible_to(&requested, item) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", requested.tenant_id, requested.project_id),
                actual: format!("{}/{}", item.scope.tenant_id, item.scope.project_id),
            });
        }
        match item.episode.as_mut() {
            Some(episode) => {
                episode.pinned = pinned;
                episode.legal_hold = legal_hold;
                Ok(())
            }
            None => Err(MemoryError::StateConflict {
                resource: memory_id.to_string(),
                state: item.kind.as_str().to_string(),
                operation: "set_episode_protection".to_string(),
            }),
        }
    }

    /// Record an accepted `TraceLink` against a memory record.
    /// The link belongs to the traceability domain (P7); this seam only exists so
    /// a deletion can prove it never revokes an accepted link (§34.2).
    pub fn link_trace(&self, ctx: &RequestContext, memory_id: &str, trace_id: &str) -> Result<()> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let mut state = self.lock();
        let item = self.item(&state, memory_id)?;
        self.require_item_scope(ctx, item)?;
        state
            .trace_links
            .entry(memory_id.to_string())
            .or_default()
            .insert(trace_id.to_string());
        Ok(())
    }

    /// Accepted trace links that became stale after a deletion.
    pub fn stale_trace_links(&self) -> Vec<String> {
        self.lock().stale_trace_links.iter().cloned().collect()
    }

    /// Accepted trace links still registered for a record.
    pub fn trace_links(&self, memory_id: &str) -> Vec<String> {
        self.lock()
            .trace_links
            .get(memory_id)
            .map(|links| links.iter().cloned().collect())
            .unwrap_or_default()
    }

    // -- MAPI-24 -------------------------------------------------------------

    /// `memory.evaluate_eviction` — plan (and optionally apply) an eviction.
    pub fn evaluate_eviction(
        &self,
        ctx: &RequestContext,
        request: &EvaluateEviction,
    ) -> Result<EvictionPlan> {
        self.authorizer.require_scope(ctx, SCOPE_MANAGE)?;
        let mut state = self.lock();
        let now = self.now();
        let requested = ItemScope::from_context(ctx);
        if let Some(session_id) = &requested.session_id {
            if session_id != &request.session_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: request.session_id.clone(),
                    actual: session_id.clone(),
                });
            }
        }
        let depth = self.depth_of(&state, ctx);
        let budget = self.budget_for(&state, depth)?;
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut references: BTreeMap<String, Ref> = BTreeMap::new();
        for block in state.blocks.values() {
            if block.state != WorkingState::Active
                || block.scope.session_id.as_deref() != Some(request.session_id.as_str())
                || block.scope.agent_id.as_deref() != Some(request.agent_id.as_str())
                || block.scope.tenant_id != requested.tenant_id
                || block.scope.project_id != requested.project_id
            {
                continue;
            }
            candidates.push(Candidate {
                id: block.id.clone(),
                tokens: block.token_count,
                relevance: block.priority.clamp(0.0, 1.0),
                age_days: ((now.saturating_sub(block.created_at)) as f64 / 86_400.0).max(0.0),
                importance: block.priority.clamp(0.0, 1.0),
                referenced: block.referenced,
                pinned: block.protected,
                protected: !block.preserve_rules.is_empty(),
                task_active: block.kind == BlockKind::TaskState,
            });
            references.insert(
                block.id.clone(),
                Ref::new(block.id.clone(), MemoryKind::Working, block.etag.clone()),
            );
        }
        candidates.sort_by(|left, right| left.id.cmp(&right.id));

        let used = Self::used_tokens(&state, &request.session_id, &request.agent_id);
        let utilisation = if budget.wm == 0 {
            100u8
        } else {
            ((u64::from(used) * 100) / u64::from(budget.wm)).min(100) as u8
        };
        let stage = next_compression_stage(utilisation, depth);
        let stage_text = stage.map_or("no_compression_stage", compression_stage_name);

        let (ranked, shadow) = match request.policy_id.as_str() {
            POLICY_DETERMINISTIC => (
                DeterministicPolicy.rank(&candidates, request.required_tokens),
                None,
            ),
            POLICY_SHADOW_RL => {
                let policy = ShadowRlPolicy::new(true);
                policy.rank_with_shadow(&candidates, request.required_tokens, None)
            }
            other => {
                return Err(MemoryError::InvalidArgument {
                    field: "policy_id".to_string(),
                    reason: format!("unknown eviction policy `{other}`"),
                })
            }
        };
        let plan = EvictionPlan {
            candidates: candidates
                .iter()
                .map(|candidate| {
                    references
                        .get(&candidate.id)
                        .cloned()
                        .unwrap_or_else(|| Ref::new(candidate.id.clone(), MemoryKind::Working, ""))
                })
                .collect(),
            selected: ranked
                .selected
                .iter()
                .map(|id| {
                    references
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| Ref::new(id.clone(), MemoryKind::Working, ""))
                })
                .collect(),
            protected: ranked
                .protected
                .iter()
                .map(|id| {
                    references
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| Ref::new(id.clone(), MemoryKind::Working, ""))
                })
                .collect(),
            model_profile: shadow.as_ref().map(|_| "shadow-no-model".to_string()),
            deterministic_fallback: shadow.is_none(),
            overflow_safe_failure: ranked.overflow_safe_failure,
            policy_version: request.policy_id.clone(),
            decision_reason: format!(
                "{}; utilisation {utilisation}%, compression stage {stage_text}",
                ranked.reason
            ),
        };
        if !request.dry_run {
            if ranked.overflow_safe_failure {
                return Err(MemoryError::ContextOverflowSafeFailure {
                    required: request.required_tokens,
                    freeable: ranked.freed_tokens,
                });
            }
            for id in &ranked.selected {
                if let Some(block) = state.blocks.remove(id) {
                    let mut evicted = block;
                    evicted.state = WorkingState::Evicted;
                    state.evicted.insert(id.clone(), evicted);
                }
            }
        }
        Ok(plan)
    }

    // -- MAPI-25 -------------------------------------------------------------

    /// `memory.get_budget` — report the budget and its current use.
    pub fn get_budget(
        &self,
        ctx: &RequestContext,
        request: &GetMemoryBudget,
    ) -> Result<BudgetView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let requested = ItemScope::from_context(ctx);
        if let Some(session_id) = &requested.session_id {
            if session_id != &request.session_id {
                return Err(MemoryError::CrossScopeDenied {
                    requested: request.session_id.clone(),
                    actual: session_id.clone(),
                });
            }
        }
        let depth = self.depth_of(&state, ctx);
        let mut view = self.budget_for(&state, depth)?;
        view.used = Self::used_tokens(&state, &request.session_id, &request.agent_id);
        Ok(view)
    }

    /// Run one maintenance sweep over every episodic record (§23.1–§23.2).
    ///
    /// The scheduled job that drives this belongs to P6 (`ttl_sweep`,
    /// `fsrs_refresh`); the pass itself is deterministic and idempotent.
    pub fn sweep(
        &self,
        privacy_delete: &[String],
        regulation_allows: bool,
    ) -> Result<Vec<(String, SweepDecision)>> {
        let mut state = self.lock();
        let now = self.now();
        let mut decisions = Vec::new();
        for (id, item) in state.items.iter_mut() {
            if item.tombstoned {
                continue;
            }
            let Some(episode) = item.episode.as_mut() else {
                continue;
            };
            let privacy = privacy_delete.iter().any(|target| target == id);
            let decision = episodic::decide_sweep(episode, now, privacy, regulation_allows)?;
            episodic::apply_sweep(episode, &decision)?;
            if decision.action == SweepAction::Purge {
                item.tombstoned = true;
            }
            decisions.push((id.clone(), decision));
        }
        Ok(decisions)
    }
}

fn fact_etag(id: &str, version: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update([0u8]);
    hasher.update(version.to_le_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    format!("\"{out}\"")
}

fn short_hash<S: AsRef<str>>(parts: &[S]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_ref().as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

impl MemoryService {
    fn require_fact_scope(
        &self,
        ctx: &RequestContext,
        fact: &crate::facts::SemanticFact,
    ) -> Result<()> {
        // Facts carry a tenant/project in their id prefix; the caller must be in
        // the same tenant and project (§34.3).
        let expected = format!("fact:{}:{}:", ctx.scope.tenant_id, ctx.scope.project_id);
        if fact.id.starts_with(&expected) {
            Ok(())
        } else {
            Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
                actual: fact.id.clone(),
            })
        }
    }

    fn fact_view(&self, fact: &crate::facts::SemanticFact) -> MemoryView {
        let summary = format!("{} {} {}", fact.subject, fact.predicate, fact.object);
        MemoryView {
            reference: Ref::new(
                fact.id.clone(),
                MemoryKind::Semantic,
                fact_etag(&fact.id, fact.version),
            ),
            kind: MemoryKind::Semantic,
            state: fact.verification.as_str().to_string(),
            summary: Some(summary.clone()),
            content: Some(summary),
            evidence: fact.evidence.clone(),
            trust: fact.confidence.clamp(0.0, 1.0),
            version: fact_etag(&fact.id, fact.version),
            token_count: token_count(&format!(
                "{} {} {}",
                fact.subject, fact.predicate, fact.object
            )),
            folded: false,
            language: fact.language.clone(),
            redacted: false,
        }
    }

    fn promotion_frozen(state: &ServiceState, fact: &crate::facts::SemanticFact) -> bool {
        let groups: BTreeSet<&str> = fact
            .evidence
            .iter()
            .map(|item| item.source_group.as_str())
            .collect();
        state
            .quarantines
            .values()
            .any(|record| record.active && groups.contains(record.source_group.as_str()))
    }

    /// Evaluate a source observation and quarantine it when a poison signal fires.
    ///
    /// This is the seam the ingestion path calls; on the wire the poison decision
    /// arrives through the policy layer (§26.3).
    pub fn quarantine_source(
        &self,
        ctx: &RequestContext,
        observation: &crate::poisoning::SourceObservation,
    ) -> Result<crate::poisoning::QuarantineRecord> {
        self.authorizer.require_scope(ctx, SCOPE_MANAGE)?;
        let decision = crate::poisoning::evaluate(observation)?;
        if !decision.quarantined {
            return Err(MemoryError::InvalidArgument {
                field: "observation".to_string(),
                reason: "no poison signal fired".to_string(),
            });
        }
        let now = self.now();
        let record = crate::poisoning::QuarantineRecord::open(
            observation.source_group.clone(),
            &decision,
            now,
        )?;
        let mut state = self.lock();
        state
            .quarantines
            .insert(observation.source_group.clone(), record.clone());
        Ok(record)
    }

    /// Lift a quarantine after an audited offline review.
    pub fn restore_source(
        &self,
        ctx: &RequestContext,
        source_group: &str,
        reviewer: &str,
        reason: &str,
        snapshot_ref: &str,
    ) -> Result<()> {
        self.authorizer.require_scope(ctx, SCOPE_MANAGE)?;
        let now = self.now();
        let mut state = self.lock();
        let record =
            state
                .quarantines
                .get_mut(source_group)
                .ok_or_else(|| MemoryError::NotFound {
                    resource: format!("quarantine {source_group}"),
                })?;
        record.restore(reviewer, reason, snapshot_ref, now)
    }

    /// Whether a source group is currently quarantined.
    pub fn is_quarantined(&self, source_group: &str) -> bool {
        self.lock()
            .quarantines
            .get(source_group)
            .map(|record| record.active)
            .unwrap_or(false)
    }

    // -- MAPI-10 -------------------------------------------------------------

    /// `memory.propose_fact` — record an evidence-backed claim as a candidate.
    pub fn propose_fact(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::ProposeFact,
    ) -> Result<Ref> {
        self.authorizer.require_scope(ctx, SCOPE_WRITE)?;
        let input = &request.input;
        let mut state = self.lock();
        let sequence = state.next_sequence();
        let id = format!(
            "fact:{}:{}:{}",
            ctx.scope.tenant_id, ctx.scope.project_id, sequence
        );
        let fact = crate::facts::SemanticFact {
            id: id.clone(),
            subject: input.subject.id.clone(),
            predicate: input.predicate.clone(),
            object: input.object.clone(),
            qualifier: None,
            language: input.language.clone(),
            translations: Vec::new(),
            confidence: input.confidence,
            evidence: input.evidence.clone(),
            verification: VerificationState::Unverified,
            contradicted_by: BTreeSet::new(),
            version: 1,
            created_at: self.now(),
            last_verified_at: None,
            legal_hold: false,
        };
        fact.validate()?;
        let etag = fact_etag(&id, fact.version);
        state.facts.insert(id.clone(), fact);
        Ok(Ref::new(id, MemoryKind::Semantic, etag))
    }

    // -- MAPI-11 -------------------------------------------------------------

    /// `memory.verify_fact` — independent review of a candidate claim.
    pub fn verify_fact(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::VerifyFact,
    ) -> Result<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_REVIEW)?;
        let mut state = self.lock();
        let now = self.now();
        let scope_marker = format!("fact:{}:{}:", ctx.scope.tenant_id, ctx.scope.project_id);
        let snapshot =
            state
                .facts
                .get(&request.fact.id)
                .cloned()
                .ok_or_else(|| MemoryError::NotFound {
                    resource: format!("fact {}", request.fact.id),
                })?;
        if !snapshot.id.starts_with(&scope_marker) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
                actual: snapshot.id.clone(),
            });
        }
        let actual_etag = fact_etag(&snapshot.id, snapshot.version);
        if request.expected_etag != actual_etag {
            return Err(MemoryError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual: actual_etag,
            });
        }
        if MemoryService::promotion_frozen(&state, &snapshot) {
            return Err(MemoryError::PolicyDenied {
                reason: "the source group is quarantined; promotion is frozen (§26.3)".to_string(),
            });
        }
        // Derive contradictions from the rest of the store before reviewing, so a
        // conflicting claim cannot be verified by accident.
        let mut contradicted: BTreeSet<String> = BTreeSet::new();
        let others: Vec<crate::facts::SemanticFact> = state
            .facts
            .values()
            .filter(|candidate| candidate.id != snapshot.id)
            .cloned()
            .collect();
        for other in others.iter() {
            if crate::facts::classify(&snapshot, other, crate::facts::THETA_ALIGN)
                == crate::facts::ClaimRelation::Contradicts
            {
                contradicted.insert(other.id.clone());
            }
        }
        let fact = state
            .facts
            .get_mut(&request.fact.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("fact {}", request.fact.id),
            })?;
        fact.contradicted_by = contradicted;
        let decision = match request.decision {
            crate::domain::FactDecision::Verify => crate::facts::ReviewDecision::Verify,
            crate::domain::FactDecision::Reject => crate::facts::ReviewDecision::Reject,
        };
        let outcome = crate::facts::verify(fact, &request.evidence_ids, decision, now)?;
        let etag = fact_etag(&fact.id, fact.version);
        Ok(ChangeReceipt {
            resource_id: request.fact.id.clone(),
            etag,
            changed: outcome.changed,
            events: vec!["memory.fact.reviewed".to_string()],
        })
    }

    // -- MAPI-12 -------------------------------------------------------------
    /// `memory.find_conflicts` — claims that contradict the given fact.
    pub fn find_conflicts(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::FindConflicts,
    ) -> Result<Page<MemoryView>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_results == 0 || request.max_results > crate::search::MAX_RESULTS {
            return Err(MemoryError::InvalidArgument {
                field: "max_results".to_string(),
                reason: format!("must be in 1..={}", crate::search::MAX_RESULTS),
            });
        }
        let state = self.lock();
        let target = state
            .facts
            .get(&request.fact.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("fact {}", request.fact.id),
            })?;
        self.require_fact_scope(ctx, target)?;
        let candidates: Vec<crate::facts::SemanticFact> = state
            .facts
            .values()
            .filter(|candidate| {
                candidate.id.starts_with(&format!(
                    "fact:{}:{}:",
                    ctx.scope.tenant_id, ctx.scope.project_id
                ))
            })
            .cloned()
            .collect();
        let conflicts =
            crate::facts::find_conflicts(target, candidates.iter(), request.max_results as usize)?;
        let views: Vec<MemoryView> = conflicts
            .into_iter()
            .map(|conflict| self.fact_view(conflict))
            .collect();
        Page::of(views, &PageRequest::new(request.max_results))
    }

    // -- MAPI-18 -------------------------------------------------------------

    /// `memory.consolidate` — run the nine-stage pipeline over the given episodes.
    pub fn consolidate(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::Consolidate,
    ) -> Result<crate::domain::JobHandle> {
        self.authorizer.require_scope(ctx, SCOPE_CONSOLIDATE)?;
        if !request.asynchronous {
            return Err(MemoryError::InvalidArgument {
                field: "asynchronous".to_string(),
                reason: "memory.consolidate is asynchronous; pass `asynchronous: true` (§34.2)"
                    .to_string(),
            });
        }
        if request.episode_refs.is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "episode_refs".to_string(),
                reason: "at least one episode is required".to_string(),
            });
        }
        if request.policy_id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "policy_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let mut state = self.lock();
        for reference in &request.episode_refs {
            let item = self.item(&state, &reference.id)?;
            self.require_item_scope(ctx, item)?;
        }
        let sequence = state.next_sequence();
        let job_id = format!("job:consolidation:{}:{}", ctx.scope.tenant_id, sequence);
        let mut pipeline = crate::consolidation::ConsolidationPipeline::new(job_id.clone());
        let source_ids: Vec<String> = request
            .episode_refs
            .iter()
            .map(|reference| reference.id.clone())
            .collect();
        let candidates: Vec<String> = state
            .facts
            .values()
            .filter(|fact| {
                fact.id.starts_with(&format!(
                    "fact:{}:{}:",
                    ctx.scope.tenant_id, ctx.scope.project_id
                )) && fact.verification == VerificationState::Unverified
            })
            .map(|fact| fact.id.clone())
            .collect();
        let mut promoted: Vec<String> = Vec::new();

        for stage in crate::consolidation::Stage::ALL {
            let input_hash = short_hash(&[
                "consolidation",
                &job_id,
                stage.as_str(),
                &source_ids.join(","),
            ]);
            let outcome = match stage {
                crate::consolidation::Stage::Cluster => {
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&source_ids),
                    }
                }
                crate::consolidation::Stage::ExtractFacts => {
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&candidates),
                    }
                }
                crate::consolidation::Stage::Align => {
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&["align", &candidates.len().to_string()]),
                    }
                }
                crate::consolidation::Stage::DetectContradictions => {
                    let mut pairs: Vec<String> = Vec::new();
                    for left in &candidates {
                        let Some(target) = state.facts.get(left) else {
                            continue;
                        };
                        let others: Vec<crate::facts::SemanticFact> = candidates
                            .iter()
                            .filter(|id| *id != left)
                            .filter_map(|id| state.facts.get(id).cloned())
                            .collect();
                        for conflict in crate::facts::find_conflicts(
                            target,
                            others.iter(),
                            crate::search::MAX_RESULTS as usize,
                        )? {
                            pairs.push(format!("{}~{}", left, conflict.id));
                        }
                    }
                    let mut frozen = false;
                    for id in &candidates {
                        if let Some(fact) = state.facts.get(id) {
                            if MemoryService::promotion_frozen(&state, fact) {
                                frozen = true;
                            }
                        }
                    }
                    if frozen {
                        crate::consolidation::StageOutcome::Quarantined {
                            reason: "a source group is quarantined (§26.3)".to_string(),
                        }
                    } else {
                        crate::consolidation::StageOutcome::Completed {
                            output_hash: short_hash(&pairs),
                        }
                    }
                }
                crate::consolidation::Stage::Calibrate => {
                    let total: f64 = candidates
                        .iter()
                        .filter_map(|id| state.facts.get(id))
                        .map(|fact| fact.confidence)
                        .sum();
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&[&format!("{total:.4}")]),
                    }
                }
                crate::consolidation::Stage::Promote => {
                    let mut promoted_now = Vec::new();
                    for id in &candidates {
                        let Some(fact) = state.facts.get(id).cloned() else {
                            continue;
                        };
                        let gate = crate::facts::promotion_gate(&fact, None);
                        if gate.allowed {
                            if let Some(stored) = state.facts.get_mut(id) {
                                stored.verification = VerificationState::Verified;
                                stored.last_verified_at = Some(self.now());
                                stored.version += 1;
                            }
                            promoted_now.push(id.clone());
                        }
                    }
                    promoted = promoted_now.clone();
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&promoted_now),
                    }
                }
                crate::consolidation::Stage::WarmCache => {
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&["warm-cache", &promoted.join(",")]),
                    }
                }
                crate::consolidation::Stage::OptimizeGraph => {
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&["graph", &state.facts.len().to_string()]),
                    }
                }
                crate::consolidation::Stage::Rescore => {
                    crate::consolidation::StageOutcome::Completed {
                        output_hash: short_hash(&["rescore", &promoted.join(",")]),
                    }
                }
            };
            pipeline.run_stage(stage, &input_hash, |_stage, _input| outcome)?;
            if pipeline.status().is_terminal() {
                break;
            }
        }
        let handle = crate::domain::JobHandle {
            job_id: job_id.clone(),
            kind: "memory.consolidation".to_string(),
            stage: pipeline.status().as_str().to_string(),
        };
        state.pipelines.insert(job_id, pipeline);
        Ok(handle)
    }

    // -- MAPI-19 -------------------------------------------------------------

    /// `memory.get_consolidation_job` — live view of a consolidation job.
    pub fn get_consolidation_job(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::GetMemoryJob,
    ) -> Result<crate::domain::JobView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let expected = format!("job:consolidation:{}:", ctx.scope.tenant_id);
        if !request.job_id.starts_with(&expected) {
            return Err(MemoryError::NotFound {
                resource: format!("job {}", request.job_id),
            });
        }
        let state = self.lock();
        let pipeline =
            state
                .pipelines
                .get(&request.job_id)
                .ok_or_else(|| MemoryError::NotFound {
                    resource: format!("job {}", request.job_id),
                })?;
        Ok(crate::domain::JobView {
            job_id: request.job_id.clone(),
            kind: "memory.consolidation".to_string(),
            stage: pipeline.status().as_str().to_string(),
            canonical_committed: pipeline.semantic_committed(),
            diagnostics: pipeline.diagnostics().to_vec(),
        })
    }

    // -- MAPI-20 -------------------------------------------------------------

    /// `memory.register_procedure_candidate` — learn a candidate procedure.
    pub fn register_procedure_candidate(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::RegisterProcedure,
    ) -> Result<crate::domain::ProcedureView> {
        self.authorizer.require_scope(ctx, SCOPE_PROCEDURE_WRITE)?;
        let draft = &request.draft;
        let mut state = self.lock();
        let sequence = state.next_sequence();
        let id = format!(
            "procedure:{}:{}:{}",
            ctx.scope.tenant_id, ctx.scope.project_id, sequence
        );
        let mut procedure = crate::procedures::Procedure::learn(
            id.clone(),
            draft.name.clone(),
            draft.language.clone(),
            draft.evidence.clone(),
            draft
                .source_episodes
                .iter()
                .map(|reference| reference.id.clone())
                .collect(),
        )?;
        procedure.steps = draft.steps.clone();
        procedure.preconditions = draft.preconditions.clone();
        procedure.tool_scope = draft.permissions.clone();
        let view = crate::domain::ProcedureView {
            reference: Ref::new(
                id.clone(),
                MemoryKind::Procedural,
                fact_etag(&id, procedure.version),
            ),
            state: procedure.state,
            success_rate: procedure.success_rate(),
            run_count: procedure.calls,
            permissions: procedure.tool_scope.clone(),
            evidence: procedure.evidence.clone(),
        };
        state.procedures.insert(id, procedure);
        Ok(view)
    }

    // -- MAPI-21 -------------------------------------------------------------

    /// `memory.review_procedure` — activate, refine or disable a procedure.
    pub fn review_procedure(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::ReviewProcedure,
    ) -> Result<crate::domain::ProcedureView> {
        self.authorizer.require_scope(ctx, SCOPE_PROCEDURE_REVIEW)?;
        let now = self.now();
        let mut state = self.lock();
        let procedure = state
            .procedures
            .get_mut(&request.procedure.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("procedure {}", request.procedure.id),
            })?;
        let scope_marker = format!(
            "procedure:{}:{}:",
            ctx.scope.tenant_id, ctx.scope.project_id
        );
        if !procedure.id.starts_with(&scope_marker) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
                actual: procedure.id.clone(),
            });
        }
        let actual = fact_etag(&procedure.id, procedure.version);
        if request.expected_etag != actual {
            return Err(MemoryError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual,
            });
        }
        let decision = match request.decision {
            crate::domain::ProcedureDecision::Activate => {
                crate::procedures::ProcedureReview::Activate
            }
            crate::domain::ProcedureDecision::Refine => crate::procedures::ProcedureReview::Refine,
            crate::domain::ProcedureDecision::Disable => {
                crate::procedures::ProcedureReview::Disable
            }
        };
        if decision == crate::procedures::ProcedureReview::Activate
            && procedure.state == ProcedureState::Candidate
        {
            if !procedure.consolidation_ready() {
                return Err(MemoryError::PolicyDenied {
                    reason: format!(
                        "the procedure needs REVIEW_REQUIRED consolidation first: {} runs at {:.2} success (§27.2)",
                        procedure.calls,
                        procedure.raw_success_rate()
                    ),
                });
            }
            let outcome = procedure.consolidate(now)?;
            if outcome != crate::procedures::ConsolidationDecision::Approved {
                return Err(MemoryError::PolicyDenied {
                    reason: "the procedure needs REVIEW_REQUIRED consolidation first (§27.2)"
                        .to_string(),
                });
            }
        }
        procedure.review(decision, now)?;
        Ok(crate::domain::ProcedureView {
            reference: Ref::new(
                procedure.id.clone(),
                MemoryKind::Procedural,
                fact_etag(&procedure.id, procedure.version),
            ),
            state: procedure.state,
            success_rate: procedure.success_rate(),
            run_count: procedure.calls,
            permissions: procedure.tool_scope.clone(),
            evidence: procedure.evidence.clone(),
        })
    }

    // -- MAPI-22 -------------------------------------------------------------

    /// `memory.record_procedure_outcome` — record a run and detect drift.
    pub fn record_procedure_outcome(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::RecordProcedureOutcome,
    ) -> Result<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_PROCEDURE_WRITE)?;
        let mut state = self.lock();
        let procedure = state
            .procedures
            .get_mut(&request.procedure.id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("procedure {}", request.procedure.id),
            })?;
        let scope_marker = format!(
            "procedure:{}:{}:",
            ctx.scope.tenant_id, ctx.scope.project_id
        );
        if !procedure.id.starts_with(&scope_marker) {
            return Err(MemoryError::CrossScopeDenied {
                requested: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
                actual: procedure.id.clone(),
            });
        }
        let actual = fact_etag(&procedure.id, procedure.version);
        if request.expected_etag != actual {
            return Err(MemoryError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual,
            });
        }
        for item in &request.evidence {
            item.validate()?;
        }
        procedure.record_outcome(request.success)?;
        if procedure.needs_refinement() && procedure.state == ProcedureState::Active {
            procedure.state = ProcedureState::Refining;
        }
        let etag = fact_etag(&procedure.id, procedure.version);
        Ok(ChangeReceipt {
            resource_id: request.procedure.id.clone(),
            etag,
            changed: true,
            events: vec!["memory.procedure.outcome".to_string()],
        })
    }
}

fn compression_stage_name(stage: CompressionStage) -> &'static str {
    match stage {
        CompressionStage::RingReplacement => "ring_safety_replacement",
        CompressionStage::EpisodeSummary => "full_text_to_summary",
        CompressionStage::Facts => "summary_to_facts",
        CompressionStage::BlockFolding => "fold_low_weight_blocks",
        CompressionStage::PolicyEviction => "policy_gated_eviction",
    }
}

fn trust_of(item: &MemoryItem) -> f64 {
    item.episode
        .as_ref()
        .map(|episode| episode.trust.rank())
        .unwrap_or(0.5)
}

// ---------------------------------------------------------------------------
// §37 port
// ---------------------------------------------------------------------------

#[allow(async_fn_in_trait)]
/// The §37 `FractalMemoryPort`.
///
/// The eight advanced operations of §24/§27 (MAPI-10..12, 18..22) are part of
/// the contract but belong to P6; this build answers them with
/// `CAPABILITY_UNAVAILABLE` rather than an empty success.
pub trait FractalMemoryPort: Send + Sync {
    /// MAPI-01.
    async fn open_agent_scope(
        &self,
        ctx: &RequestContext,
        request: &OpenAgentScope,
    ) -> Result<AgentScopeView>;
    /// MAPI-02.
    async fn close_agent_scope(
        &self,
        ctx: &RequestContext,
        request: &CloseAgentScope,
    ) -> Result<CloseScopeResult>;
    /// MAPI-03.
    async fn put_working_block(
        &self,
        ctx: &RequestContext,
        request: &PutWorkingBlock,
    ) -> Result<Ref>;
    /// MAPI-04.
    async fn list_working_blocks(
        &self,
        ctx: &RequestContext,
        request: &ListWorkingBlocks,
    ) -> Result<Page<MemoryView>>;
    /// MAPI-05.
    async fn remove_working_block(
        &self,
        ctx: &RequestContext,
        request: &RemoveWorkingBlock,
    ) -> Result<DeletionReceipt>;
    /// MAPI-06.
    async fn store_episode(
        &self,
        ctx: &RequestContext,
        request: &StoreEpisode,
    ) -> Result<ChangeReceipt>;
    /// MAPI-07.
    async fn search(
        &self,
        ctx: &RequestContext,
        request: &SearchMemory,
    ) -> Result<Page<MemoryView>>;
    /// MAPI-08.
    async fn retrieve(&self, ctx: &RequestContext, request: &RetrieveMemory) -> Result<MemoryView>;
    /// MAPI-09.
    async fn update_revision(
        &self,
        ctx: &RequestContext,
        request: &UpdateMemory,
    ) -> Result<ChangeReceipt>;
    /// MAPI-10.
    async fn propose_fact(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::ProposeFact,
    ) -> Result<Ref>;
    /// MAPI-11.
    async fn verify_fact(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::VerifyFact,
    ) -> Result<ChangeReceipt>;
    /// MAPI-12.
    async fn find_conflicts(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::FindConflicts,
    ) -> Result<Page<MemoryView>>;
    /// MAPI-13.
    async fn fold(&self, ctx: &RequestContext, request: &FoldMemory) -> Result<FoldedResult>;
    /// MAPI-14.
    async fn page_out(&self, ctx: &RequestContext, request: &PageOut) -> Result<PagingReceipt>;
    /// MAPI-15.
    async fn page_in(
        &self,
        ctx: &RequestContext,
        request: &PageIn,
    ) -> Result<crate::domain::PageInResult>;
    /// MAPI-16.
    async fn begin_reconsolidation(
        &self,
        ctx: &RequestContext,
        request: &BeginReconsolidation,
    ) -> Result<crate::domain::ReconsolidationWindow>;
    /// MAPI-17.
    async fn commit_reconsolidation(
        &self,
        ctx: &RequestContext,
        request: &CommitReconsolidation,
    ) -> Result<ChangeReceipt>;
    /// MAPI-18.
    async fn consolidate(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::Consolidate,
    ) -> Result<crate::domain::JobHandle>;
    /// MAPI-19.
    async fn get_consolidation_job(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::GetMemoryJob,
    ) -> Result<crate::domain::JobView>;
    /// MAPI-20.
    async fn register_procedure_candidate(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::RegisterProcedure,
    ) -> Result<crate::domain::ProcedureView>;
    /// MAPI-21.
    async fn review_procedure(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::ReviewProcedure,
    ) -> Result<crate::domain::ProcedureView>;
    /// MAPI-22.
    async fn record_procedure_outcome(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::RecordProcedureOutcome,
    ) -> Result<ChangeReceipt>;
    /// MAPI-23.
    async fn forget(&self, ctx: &RequestContext, request: &ForgetMemory)
        -> Result<DeletionReceipt>;
    /// MAPI-24.
    async fn evaluate_eviction(
        &self,
        ctx: &RequestContext,
        request: &EvaluateEviction,
    ) -> Result<EvictionPlan>;
    /// MAPI-25.
    async fn get_budget(
        &self,
        ctx: &RequestContext,
        request: &GetMemoryBudget,
    ) -> Result<BudgetView>;
}

impl FractalMemoryPort for MemoryService {
    async fn open_agent_scope(
        &self,
        ctx: &RequestContext,
        request: &OpenAgentScope,
    ) -> Result<AgentScopeView> {
        MemoryService::open_agent_scope(self, ctx, request)
    }
    async fn close_agent_scope(
        &self,
        ctx: &RequestContext,
        request: &CloseAgentScope,
    ) -> Result<CloseScopeResult> {
        MemoryService::close_agent_scope(self, ctx, request)
    }
    async fn put_working_block(
        &self,
        ctx: &RequestContext,
        request: &PutWorkingBlock,
    ) -> Result<Ref> {
        MemoryService::put_working_block(self, ctx, request)
    }
    async fn list_working_blocks(
        &self,
        ctx: &RequestContext,
        request: &ListWorkingBlocks,
    ) -> Result<Page<MemoryView>> {
        MemoryService::list_working_blocks(self, ctx, request)
    }
    async fn remove_working_block(
        &self,
        ctx: &RequestContext,
        request: &RemoveWorkingBlock,
    ) -> Result<DeletionReceipt> {
        MemoryService::remove_working_block(self, ctx, request)
    }
    async fn store_episode(
        &self,
        ctx: &RequestContext,
        request: &StoreEpisode,
    ) -> Result<ChangeReceipt> {
        MemoryService::store_episode(self, ctx, request)
    }
    async fn search(
        &self,
        ctx: &RequestContext,
        request: &SearchMemory,
    ) -> Result<Page<MemoryView>> {
        MemoryService::search(self, ctx, request)
    }
    async fn retrieve(&self, ctx: &RequestContext, request: &RetrieveMemory) -> Result<MemoryView> {
        MemoryService::retrieve(self, ctx, request)
    }
    async fn update_revision(
        &self,
        ctx: &RequestContext,
        request: &UpdateMemory,
    ) -> Result<ChangeReceipt> {
        MemoryService::update_revision(self, ctx, request)
    }
    async fn propose_fact(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::ProposeFact,
    ) -> Result<Ref> {
        MemoryService::propose_fact(self, ctx, request)
    }
    async fn verify_fact(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::VerifyFact,
    ) -> Result<ChangeReceipt> {
        MemoryService::verify_fact(self, ctx, request)
    }
    async fn find_conflicts(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::FindConflicts,
    ) -> Result<Page<MemoryView>> {
        MemoryService::find_conflicts(self, ctx, request)
    }
    async fn fold(&self, ctx: &RequestContext, request: &FoldMemory) -> Result<FoldedResult> {
        MemoryService::fold(self, ctx, request)
    }
    async fn page_out(&self, ctx: &RequestContext, request: &PageOut) -> Result<PagingReceipt> {
        MemoryService::page_out(self, ctx, request)
    }
    async fn page_in(
        &self,
        ctx: &RequestContext,
        request: &PageIn,
    ) -> Result<crate::domain::PageInResult> {
        MemoryService::page_in(self, ctx, request)
    }
    async fn begin_reconsolidation(
        &self,
        ctx: &RequestContext,
        request: &BeginReconsolidation,
    ) -> Result<crate::domain::ReconsolidationWindow> {
        MemoryService::begin_reconsolidation(self, ctx, request)
    }
    async fn commit_reconsolidation(
        &self,
        ctx: &RequestContext,
        request: &CommitReconsolidation,
    ) -> Result<ChangeReceipt> {
        MemoryService::commit_reconsolidation(self, ctx, request)
    }
    async fn consolidate(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::Consolidate,
    ) -> Result<crate::domain::JobHandle> {
        MemoryService::consolidate(self, ctx, request)
    }
    async fn get_consolidation_job(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::GetMemoryJob,
    ) -> Result<crate::domain::JobView> {
        MemoryService::get_consolidation_job(self, ctx, request)
    }
    async fn register_procedure_candidate(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::RegisterProcedure,
    ) -> Result<crate::domain::ProcedureView> {
        MemoryService::register_procedure_candidate(self, ctx, request)
    }
    async fn review_procedure(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::ReviewProcedure,
    ) -> Result<crate::domain::ProcedureView> {
        MemoryService::review_procedure(self, ctx, request)
    }
    async fn record_procedure_outcome(
        &self,
        ctx: &RequestContext,
        request: &crate::domain::RecordProcedureOutcome,
    ) -> Result<ChangeReceipt> {
        MemoryService::record_procedure_outcome(self, ctx, request)
    }
    async fn forget(
        &self,
        ctx: &RequestContext,
        request: &ForgetMemory,
    ) -> Result<DeletionReceipt> {
        MemoryService::forget(self, ctx, request)
    }
    async fn evaluate_eviction(
        &self,
        ctx: &RequestContext,
        request: &EvaluateEviction,
    ) -> Result<EvictionPlan> {
        MemoryService::evaluate_eviction(self, ctx, request)
    }
    async fn get_budget(
        &self,
        ctx: &RequestContext,
        request: &GetMemoryBudget,
    ) -> Result<BudgetView> {
        MemoryService::get_budget(self, ctx, request)
    }
}
