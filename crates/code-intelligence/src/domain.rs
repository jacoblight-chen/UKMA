//! Canonical Code Intelligence request/response types (§32.1) and the port
//! input/output shapes used by the 13 code operations (CAPI-01..13).
//!
//! These are domain types, not wire types: the wire envelopes live in
//! `ukma-contracts` and are validated against `contracts/schema/v1`. Nothing
//! here imports a database SDK.

use serde::{Deserialize, Serialize};

use crate::{CodeEdge, CodeEdgeKind, CodeLocation, Symbol, SymbolKind};

/// Which revision of a repository a request is bound to (`§32.1 RepoRevision`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRevision {
    /// Repository resource id.
    pub repository_id: String,
    /// Version control system; only `git` is supported today.
    pub vcs: String,
    /// Full commit SHA the request is bound to.
    pub commit_sha: String,
    /// Optional tree SHA.
    pub tree_sha: Option<String>,
    /// What to do when the worktree is dirty.
    pub dirty_policy: DirtyPolicy,
}

impl RepoRevision {
    /// Git revision bound to `commit_sha` that rejects dirty worktrees.
    pub fn git(repository_id: &str, commit_sha: &str) -> Self {
        Self {
            repository_id: repository_id.to_string(),
            vcs: "git".to_string(),
            commit_sha: commit_sha.to_string(),
            tree_sha: None,
            dirty_policy: DirtyPolicy::Reject,
        }
    }

    /// Validate the shape of the revision reference.
    pub fn validate(&self) -> crate::CodeResult<()> {
        if self.repository_id.trim().is_empty() {
            return Err(crate::CodeError::InvalidArgument {
                field: "repository_id",
                reason: "must be non-empty".into(),
            });
        }
        if self.vcs != "git" {
            return Err(crate::CodeError::InvalidArgument {
                field: "vcs",
                reason: format!("`{}` is not supported; only `git`", self.vcs),
            });
        }
        if !is_full_sha(&self.commit_sha) {
            return Err(crate::CodeError::InvalidArgument {
                field: "commit_sha",
                reason: "must be a full 40-character hex SHA".into(),
            });
        }
        Ok(())
    }
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Dirty-worktree handling (§32.1 `dirty_policy`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirtyPolicy {
    /// Refuse to analyze a dirty worktree.
    Reject,
    /// Analyze the dirty content and record it as a snapshot.
    Snapshot,
}

/// One source file of a repository snapshot.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// File content.
    pub content: String,
}

impl SourceFile {
    /// New file.
    pub fn new(path: &str, content: &str) -> Self {
        Self {
            path: path.to_string(),
            content: content.to_string(),
        }
    }

    /// Language identifier derived from the file extension.
    pub fn language(&self) -> Option<String> {
        let extension = self.path.rsplit('.').next()?;
        crate::LanguageRegistry::new()
            .by_extension(extension)
            .map(|entry| entry.language.clone())
    }
}

/// A resolved source snapshot for one revision (§4.1: never a floating branch).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Commit the snapshot was taken from.
    pub commit_sha: String,
    /// Files in repository order.
    pub files: Vec<SourceFile>,
    /// Whether uncommitted local changes were included.
    pub dirty: bool,
}

/// A page request (§31.4). `limit` is bounded; `cursor` is opaque.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodePageRequest"))]
pub struct PageRequest {
    /// Opaque cursor from a previous page.
    pub cursor: Option<String>,
    /// Maximum items to return (1..=200).
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
    /// Largest page a single response may return.
    pub const MAX_LIMIT: u32 = 200;

    /// Page request with an explicit limit.
    pub fn new(limit: u32) -> Self {
        Self {
            cursor: None,
            limit,
        }
    }

    /// Validate the limit range.
    pub fn validate(&self) -> crate::CodeResult<()> {
        if self.limit == 0 || self.limit > Self::MAX_LIMIT {
            return Err(crate::CodeError::InvalidArgument {
                field: "page.limit",
                reason: format!("must be in 1..={}", Self::MAX_LIMIT),
            });
        }
        Ok(())
    }

    /// Cursor offset, if any.
    pub fn offset(&self) -> crate::CodeResult<usize> {
        match &self.cursor {
            None => Ok(0),
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| crate::CodeError::CursorExpired {
                    cursor: cursor.clone(),
                }),
        }
    }
}

/// One page of results.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items in this page.
    pub items: Vec<T>,
    /// Cursor for the next page, when more items exist.
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Build a page from the full ordered result set plus the request.
    pub fn of(items: Vec<T>, request: &PageRequest) -> crate::CodeResult<Self> {
        let offset = request.offset()?;
        if offset > items.len() {
            return Err(crate::CodeError::CursorExpired {
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

/// Handle returned by an accepted analysis job.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodeJobHandle"))]
pub struct JobHandle {
    /// Job id.
    pub job_id: String,
    /// Job kind (`code.analyze_repository`).
    pub kind: String,
    /// Stage at the time the handle was produced.
    pub stage: String,
}

/// Live view of a job (`JobView` in §32.2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodeJobView"))]
pub struct JobView {
    /// Job id.
    pub job_id: String,
    /// Job kind.
    pub kind: String,
    /// Canonical stage name.
    pub stage: String,
    /// Whether canonical data was committed. Never inferred from `stage`
    /// alone (§40.4).
    pub canonical_committed: bool,
    /// Reference to the authoritative manifest, available only after commit.
    pub result_ref: Option<String>,
    /// Diagnostics collected while running.
    pub diagnostics: Vec<String>,
}

/// Request: start (or join) a repository analysis (`CAPI-01`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzeRepository {
    /// Revision to analyze.
    pub revision: RepoRevision,
    /// Parser profile the caller requires.
    pub parser_profile: String,
    /// Include globs (empty means "everything the parser supports").
    pub include_globs: Vec<String>,
    /// Exclude globs.
    pub exclude_globs: Vec<String>,
    /// Callers must pass `true`; a synchronous contract would hide job state.
    pub asynchronous: bool,
}

/// Request: read a job (`CAPI-02`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodeGetJob"))]
pub struct GetJob {
    /// Job id.
    pub job_id: String,
}

/// Request: cancel a job (`CAPI-03`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodeCancelJob"))]
pub struct CancelJob {
    /// Job id.
    pub job_id: String,
    /// Reason recorded on the job.
    pub reason: String,
}

/// Request: get one symbol by id (`CAPI-04`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSymbol {
    /// Symbol id.
    pub symbol_id: String,
}

/// Request: symbol search (`CAPI-05`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolQuery {
    /// Repository the query is bound to.
    pub repository_id: String,
    /// Revision (commit SHA).
    pub revision: String,
    /// Case-insensitive substring matched against name and qualified name.
    pub query: String,
    /// Restrict to these kinds (empty means all).
    pub kinds: Vec<SymbolKind>,
    /// Restrict to this path glob.
    pub path_glob: Option<String>,
    /// Restrict to this language.
    pub language: Option<String>,
    /// Page.
    pub page: PageRequest,
}

/// Request: references to a symbol (`CAPI-06`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceQuery {
    /// Symbol id whose references are wanted.
    pub symbol_id: String,
    /// Include the declaration itself.
    pub include_declaration: bool,
    /// Restrict to these edge kinds (empty means every kind).
    pub relation_kinds: Vec<CodeEdgeKind>,
    /// Page.
    pub page: PageRequest,
}

/// Direction of a traversal (`§32.1 TraversalQuery.direction`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Callers / incoming edges.
    In,
    /// Callees / outgoing edges.
    Out,
}

/// Request: bounded traversal of the code graph (`CAPI-07/08/09`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalQuery {
    /// Start symbol.
    pub symbol_id: String,
    /// Direction.
    pub direction: Direction,
    /// Edge kinds to follow (empty means every kind).
    pub edge_kinds: Vec<CodeEdgeKind>,
    /// Maximum hops.
    pub max_depth: u32,
    /// Maximum visited nodes.
    pub max_nodes: u32,
    /// Page over the collected edges.
    pub page: PageRequest,
}

/// Request: impact analysis (`CAPI-10`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactQuery {
    /// Changed symbols.
    pub changed: Vec<String>,
    /// Revision the analysis is bound to.
    pub revision: String,
    /// Edge kinds to follow (empty means calls/references/implements).
    pub edge_kinds: Vec<CodeEdgeKind>,
    /// Maximum hops.
    pub max_depth: u32,
    /// Maximum visited nodes.
    pub max_nodes: u32,
    /// Whether test candidates should be reported.
    pub include_test_candidates: bool,
}

/// Result of `code.analyze_impact` (`§32.1 ImpactReport`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactReport {
    /// Impacted symbol ids, ordered by distance then id.
    pub impacted: Vec<String>,
    /// One edge path per impacted symbol, from a changed symbol.
    pub paths: Vec<Vec<CodeEdge>>,
    /// Symbols that look like tests.
    pub test_candidates: Vec<String>,
    /// Call edges that could not be resolved and were therefore not followed.
    pub unresolved_dynamic_calls: u32,
    /// `complete` only when nothing was truncated and no dynamic call was seen.
    pub completeness: String,
    /// Evidence for the report itself.
    pub evidence_ids: Vec<String>,
}

/// Request: assemble edit context (`CAPI-11`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditContextQuery {
    /// Caller intent, recorded for audit.
    pub intent: String,
    /// Repository the request is bound to.
    pub repository_id: String,
    /// Revision (commit SHA).
    pub revision: String,
    /// Symbols whose source is wanted.
    pub target_refs: Vec<String>,
    /// Hard token budget.
    pub max_tokens: u32,
}

/// One block of `EditContextResult`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlock {
    /// Symbol the block belongs to.
    pub symbol_id: String,
    /// Source text of the symbol's definition.
    pub content: String,
    /// Estimated tokens.
    pub token_count: u32,
    /// Evidence ids.
    pub evidence_ids: Vec<String>,
}

/// Result of `code.get_edit_context` (`§32.1 EditContextResult`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditContextResult {
    /// Included blocks, in request order.
    pub blocks: Vec<ContextBlock>,
    /// Sum of the block token counts.
    pub total_tokens: u32,
    /// Whether anything was dropped.
    pub truncation: bool,
    /// Symbols that did not fit the budget.
    pub omitted_refs: Vec<String>,
}

/// Change kind between two revisions.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Present only in the new revision.
    Added,
    /// Present only in the old revision.
    Removed,
    /// Present in both with a different signature or span.
    Modified,
}

/// One entry of `code.compare_revisions`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeChange {
    /// Change classification.
    pub change_kind: ChangeKind,
    /// Old symbol, when present.
    pub old_symbol: Option<Symbol>,
    /// New symbol, when present.
    pub new_symbol: Option<Symbol>,
    /// Evidence ids.
    pub evidence_ids: Vec<String>,
}

/// Request: diff two revisions (`CAPI-12`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRevisionDiff {
    /// Repository.
    pub repository_id: String,
    /// Old revision.
    pub from_revision: String,
    /// New revision.
    pub to_revision: String,
    /// Page.
    pub page: PageRequest,
}

/// Request: invalidate a revision (`CAPI-13`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidateCodeRevision {
    /// Repository.
    pub repository_id: String,
    /// Revision to invalidate.
    pub revision: String,
    /// Reason recorded on the receipt.
    pub reason: String,
    /// Optimistic concurrency token of the revision index.
    pub expected_etag: String,
}

/// Receipt of a mutation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodeChangeReceipt"))]
pub struct ChangeReceipt {
    /// Resource affected.
    pub resource_id: String,
    /// New resource etag.
    pub etag: String,
    /// Whether anything actually changed.
    pub changed: bool,
    /// Events that would be published for this change.
    pub events: Vec<String>,
}

/// One resolved reference location.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceLocation {
    /// Symbol that owns the reference site.
    pub symbol_id: String,
    /// Where the reference is written.
    pub location: CodeLocation,
    /// Edge kind.
    pub kind: CodeEdgeKind,
}
