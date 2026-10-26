//! Code Intelligence service: the ingestion pipeline (`P3-WP3`), the thirteen
//! code operations (`P3-WP4`) and impact / edit-context assembly (`P3-WP5`).
//!
//! Design notes that matter for the contract:
//! - The code index is a **derived projection** (§32.3). The authoritative
//!   artifacts are the source snapshot plus the analysis manifest, which are
//!   retained when a revision is invalidated (`stale-not-delete`).
//! - Symbol ids are version-scoped identities per §32.3:
//!   `sha256(repo, commit, path, qualified_name, kind, signature, span)`.
//!   A rename across revisions is therefore a *new* id, never an in-place
//!   replacement.
//! - Jobs use [`indexing_engine::JobManager`], so execution, commit and
//!   projection stay orthogonal (§40.4) and a cancel after the commit point can
//!   never undo canonical data.
//! - Dyn-free state: the service is a single-process reference implementation.
//!   `analyze_repository` drives a submitted job to completion inline; a worker
//!   replaces that call at P8 without changing the operation surface.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use embedding_engine::{BackendMode, EmbeddingPort};
use indexing_engine::{GenerationPointer, JobError, JobManager, Stage};
use sha2::{Digest, Sha256};
use ukma_contracts::{RequestContext, ScopeRef};

use crate::domain::{
    AiContextQuery, AiContextResult, AiContextSection, AnalyzeRepository, CancelJob, ChangeKind,
    ChangeReceipt, CircularDependency, CircularDepsQuery, CodeChange, CodeRevisionDiff,
    ComplexityQuery, ComplexityReport, ContextBlock, ContextIntent, CuratedContext,
    CuratedContextQuery, CuratedMatch, DeadImport, DeadImportsQuery, DetailedSymbol, Direction,
    DirtyPolicy, EditContextQuery, EditContextResult, EntryPoint, EntryPointsQuery, ErrorMatch,
    ErrorSearchQuery, ExportGraphQuery, FindImplementorsQuery, FindImportersQuery,
    GetDetailedSymbolQuery, GetJob, GetSymbol, GraphExport, HotPath, HotPathsQuery, ImpactQuery,
    ImpactReport, ImplementorMatch, ImportMatchMode, ImporterMatch, IndexDirectory, IndexFile,
    IndexFiles, IndexedRevision, InvalidateCodeRevision, JobHandle, JobView, ModuleSummary,
    ModuleSummaryQuery, Page, PatternMatch, PatternScope, PatternSearchQuery, PrContextFormat,
    PrContextQuery, PrContextReport, ReferenceLocation, ReferenceQuery, RelatedTest,
    RelatedTestsQuery, RepoRevision, SignatureQuery, SourceFile, SourceSnapshot, SymbolQuery,
    SymbolRef, TraversalQuery, TraverseQuery, TraverseResult,
};
use crate::{
    CodeAnalysisManifest, CodeDiagnostic, CodeEdge, CodeEdgeKind, CodeError, CodeLocation,
    CodeResult, EdgeResolution, ParsedSource, ParserPort, SupportLevel, Symbol, SymbolKind,
};

/// Maximum traversal depth accepted by a bounded traversal.
pub const MAX_TRAVERSAL_DEPTH: u32 = 32;
/// Maximum number of nodes a bounded traversal may visit.
pub const MAX_TRAVERSAL_NODES: u32 = 10_000;
/// Reference token estimator: one token per four characters.
pub const CHARS_PER_TOKEN: u32 = 4;

/// Scope names used by the code operations (§41.1).
pub const SCOPE_READ: &str = "code:read";
/// Indexing scope.
pub const SCOPE_INDEX: &str = "code:index";
/// Index administration scope.
pub const SCOPE_INDEX_ADMIN: &str = "code:index_admin";

// ---------------------------------------------------------------------------
// Source port
// ---------------------------------------------------------------------------

/// Read the source snapshot a revision resolves to (§4.1: a revision is a
/// commit, never a floating branch).
pub trait SourceSnapshotPort: Send + Sync {
    /// Resolve `revision` to its files.
    fn snapshot(&self, revision: &RepoRevision) -> CodeResult<SourceSnapshot>;

    /// Resolve a registered alias (for example `@cwd`) to the real revision
    /// id. The default is the identity, so sources without aliases are
    /// unaffected.
    fn resolve(&self, commit_sha: &str) -> String {
        commit_sha.to_string()
    }

    /// The files (path -> content) for a revision, when the source can expose
    /// them (used to populate the Pro corpus). The default is empty, so
    /// sources that do not expose file contents are unaffected.
    fn files_for(&self, _commit_sha: &str) -> Option<BTreeMap<String, String>> {
        None
    }
}

/// In-memory source snapshots. Development/test only: it makes no durability or
/// cross-process claim.
#[derive(Debug, Default)]
pub struct InMemorySources {
    snapshots: BTreeMap<String, SourceSnapshot>,
    aliases: BTreeMap<String, String>,
}

impl InMemorySources {
    /// Empty source set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a clean snapshot for `commit_sha`.
    pub fn with_commit(mut self, commit_sha: &str, files: Vec<SourceFile>) -> Self {
        self.insert(SourceSnapshot {
            commit_sha: commit_sha.to_string(),
            files,
            dirty: false,
        });
        self
    }

    /// Register a snapshot that contains uncommitted changes.
    pub fn with_dirty_commit(mut self, commit_sha: &str, files: Vec<SourceFile>) -> Self {
        self.insert(SourceSnapshot {
            commit_sha: commit_sha.to_string(),
            files,
            dirty: true,
        });
        self
    }

    /// Add one snapshot.
    pub fn insert(&mut self, snapshot: SourceSnapshot) {
        self.snapshots.insert(snapshot.commit_sha.clone(), snapshot);
    }

    /// Register `alias` (for example `@cwd`) as standing for the real
    /// `commit_sha`. An alias is inert unless registered, so the default
    /// behavior for every existing source is unchanged.
    pub fn with_alias(mut self, alias: &str, commit_sha: &str) -> Self {
        self.aliases
            .insert(alias.to_string(), commit_sha.to_string());
        self
    }
}

impl SourceSnapshotPort for InMemorySources {
    fn snapshot(&self, revision: &RepoRevision) -> CodeResult<SourceSnapshot> {
        let sha = self.resolve(&revision.commit_sha);
        let resolved = RepoRevision {
            commit_sha: sha,
            ..revision.clone()
        };
        resolved.validate()?;
        self.snapshots
            .get(&resolved.commit_sha)
            .cloned()
            .ok_or_else(|| CodeError::SourceUnavailable {
                revision: resolved.commit_sha.clone(),
            })
    }

    fn resolve(&self, commit_sha: &str) -> String {
        self.aliases
            .get(commit_sha)
            .cloned()
            .unwrap_or_else(|| commit_sha.to_string())
    }

    fn files_for(&self, commit_sha: &str) -> Option<BTreeMap<String, String>> {
        self.snapshots.get(commit_sha).map(|snapshot| {
            snapshot
                .files
                .iter()
                .map(|file| (file.path.clone(), file.content.clone()))
                .collect()
        })
    }
}

/// A [`SourceSnapshotPort`] whose snapshot set can be atomically replaced at
/// runtime (CodeGraph `--watch` hot-swap). The code domain reads the snapshot
/// on every call, so a `replace` is immediately visible to in-flight and future
/// tool calls without rebuilding the gateway.
pub struct SwappableSources {
    inner: std::sync::RwLock<InMemorySources>,
}

impl SwappableSources {
    /// Wrap an initial source set.
    pub fn new(initial: InMemorySources) -> Self {
        Self {
            inner: std::sync::RwLock::new(initial),
        }
    }

    /// Atomically replace the whole snapshot set (including `@cwd` alias).
    pub fn replace(&self, new: InMemorySources) {
        let mut guard = self.inner.write().expect("source lock poisoned");
        *guard = new;
    }
}

impl SourceSnapshotPort for SwappableSources {
    fn snapshot(&self, revision: &RepoRevision) -> CodeResult<SourceSnapshot> {
        self.inner
            .read()
            .expect("source lock poisoned")
            .snapshot(revision)
    }

    fn resolve(&self, commit_sha: &str) -> String {
        self.inner
            .read()
            .expect("source lock poisoned")
            .resolve(commit_sha)
    }

    fn files_for(&self, commit_sha: &str) -> Option<BTreeMap<String, String>> {
        self.inner
            .read()
            .expect("source lock poisoned")
            .files_for(commit_sha)
    }
}

// ---------------------------------------------------------------------------
// Authorization port
// ---------------------------------------------------------------------------

/// Authorization decisions for the code operations. The default
/// [`ScopeAuthorizer`] only checks the scope list; a policy-backed authorizer
/// (P6/P10) adds per-resource rules without changing the operation surface.
pub trait CodeAuthorizer: Send + Sync {
    /// Require `scope` on the request context.
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> CodeResult<()>;

    /// Per-symbol read permission. Defaults to allow.
    fn check_symbol(&self, _ctx: &RequestContext, _symbol: &Symbol) -> CodeResult<()> {
        Ok(())
    }

    /// Per-file read permission. Defaults to allow.
    fn check_path(&self, _ctx: &RequestContext, _path: &str) -> CodeResult<()> {
        Ok(())
    }
}

/// Default authorizer: the delegated scope must be present on the context.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScopeAuthorizer;

impl CodeAuthorizer for ScopeAuthorizer {
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> CodeResult<()> {
        if ctx.scopes.iter().any(|granted| granted == scope) {
            Ok(())
        } else {
            Err(CodeError::Forbidden {
                required_scope: scope.to_string(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

/// Identity of one analyzed revision inside one tenant/project.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RevisionKey {
    tenant_id: String,
    project_id: String,
    repository_id: String,
    commit_sha: String,
}

impl RevisionKey {
    fn new(scope: &ScopeRef, repository_id: &str, commit_sha: &str) -> Self {
        Self {
            tenant_id: scope.tenant_id.clone(),
            project_id: scope.project_id.clone(),
            repository_id: repository_id.to_string(),
            commit_sha: commit_sha.to_string(),
        }
    }

    fn label(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.tenant_id, self.project_id, self.repository_id, self.commit_sha
        )
    }

    fn generation(&self, sequence: u64) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.label().as_bytes());
        hasher.update(sequence.to_le_bytes());
        format!(
            "gen-{:x}",
            &hasher.finalize()[..8]
                .iter()
                .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte))
        )
    }
}

/// One analyzed revision: the derived code index plus its authoritative manifest.
#[derive(Debug, Clone)]
struct RevisionIndex {
    scope: ScopeRef,
    repository_id: String,
    commit_sha: String,
    generation: String,
    etag: String,
    stale: bool,
    manifest: CodeAnalysisManifest,
    symbols: BTreeMap<String, Symbol>,
    edges: Vec<CodeEdge>,
    incoming: BTreeMap<String, Vec<usize>>,
    outgoing: BTreeMap<String, Vec<usize>>,
    sources: BTreeMap<String, String>,
}

impl RevisionIndex {
    fn rebuild_adjacency(&mut self) {
        self.incoming.clear();
        self.outgoing.clear();
        for (position, edge) in self.edges.iter().enumerate() {
            self.outgoing
                .entry(edge.source.clone())
                .or_default()
                .push(position);
            if let Some(target) = &edge.target {
                self.incoming
                    .entry(target.clone())
                    .or_default()
                    .push(position);
            }
        }
    }

    fn edge_at(&self, position: usize) -> &CodeEdge {
        &self.edges[position]
    }

    fn symbols_in_path_order(&self) -> Vec<&Symbol> {
        let mut symbols: Vec<&Symbol> = self.symbols.values().collect();
        symbols.sort_by(|left, right| {
            (path_of(&left.definition), &left.qualified_name, &left.id).cmp(&(
                path_of(&right.definition),
                &right.qualified_name,
                &right.id,
            ))
        });
        symbols
    }
}

fn path_of(location: &CodeLocation) -> &str {
    location
        .file_id
        .strip_prefix("file:")
        .unwrap_or(&location.file_id)
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ServiceState {
    revisions: HashMap<String, RevisionIndex>,
    latest: HashMap<String, String>,
    jobs: JobManager,
    job_revisions: HashMap<String, String>,
    job_scopes: HashMap<String, ScopeRef>,
    job_requests: HashMap<String, AnalyzeRepository>,
    dedup: HashMap<String, String>,
    idempotency: HashMap<String, (String, String)>,
    generation: GenerationPointer,
    sequence: u64,
}

impl ServiceState {
    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
}

/// Reference implementation of the Code Intelligence domain service.
pub struct CodeIntelligenceService {
    parser: Arc<dyn ParserPort>,
    sources: Arc<dyn SourceSnapshotPort>,
    authorizer: Arc<dyn CodeAuthorizer>,
    state: Mutex<ServiceState>,
    search_policy: crate::search::SearchPolicy,
    /// Optional embedding port + selected profile (CodeGraph
    /// `--embedding-model` parity). `None` keeps the static embedder.
    embedder: Option<(Arc<dyn EmbeddingPort>, String)>,
    /// Effective mode after the RAM gate (`WithModel` or `GraphOnly`).
    backend_mode: BackendMode,
}

impl std::fmt::Debug for CodeIntelligenceService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeIntelligenceService").finish()
    }
}

impl CodeIntelligenceService {
    /// Service over a parser port and a source port.
    pub fn new(parser: Arc<dyn ParserPort>, sources: Arc<dyn SourceSnapshotPort>) -> Self {
        Self {
            parser,
            sources,
            authorizer: Arc::new(ScopeAuthorizer),
            state: Mutex::new(ServiceState::default()),
            search_policy: crate::search::SearchPolicy::default(),
            embedder: None,
            backend_mode: BackendMode::WithModel,
        }
    }

    /// Replace the authorizer.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn CodeAuthorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    /// Set the search / embedding policy (CodeGraph `--split-identifiers`,
    /// `--full-body-embedding`, `--graph-only` parity).
    pub fn with_search_policy(mut self, policy: crate::search::SearchPolicy) -> Self {
        self.search_policy = policy;
        self
    }

    /// Set the embedding port + selected profile and the effective mode after
    /// the RAM gate (CodeGraph `--embedding-model` + `CODEGRAPH_SKIP_MEMORY_CHECK`
    /// parity). A `None` embedder keeps the static channel; an unavailable
    /// profile degrades the semantic channel to the static fallback.
    pub fn with_embedding_backend(
        mut self,
        embedder: Option<(Arc<dyn EmbeddingPort>, String)>,
        backend_mode: BackendMode,
    ) -> Self {
        self.embedder = embedder;
        self.backend_mode = backend_mode;
        self
    }

    /// The selected embedding profile id, if an embedder is wired.
    pub fn embedding_profile_id(&self) -> Option<&str> {
        self.embedder.as_ref().map(|(_, id)| id.as_str())
    }

    /// The effective mode after the RAM gate.
    pub fn backend_mode(&self) -> BackendMode {
        self.backend_mode
    }

    /// Whether the selected profile is usable (mode `WithModel` + the port
    /// reports the profile available).
    pub fn embedding_available(&self) -> bool {
        if self.backend_mode != BackendMode::WithModel {
            return false;
        }
        match &self.embedder {
            Some((port, id)) => port.model_available(id),
            None => true,
        }
    }

    fn lock(&self) -> MutexGuard<'_, ServiceState> {
        self.state.lock().expect("code service state poisoned")
    }

    /// The revision to process: a source-registered alias (for example
    /// `@cwd`) is resolved to its concrete revision id; anything else
    /// passes through unchanged.
    fn resolve_revision(&self, revision: &RepoRevision) -> RepoRevision {
        let sha = self.sources.resolve(&revision.commit_sha);
        if sha == revision.commit_sha {
            revision.clone()
        } else {
            RepoRevision {
                commit_sha: sha,
                ..revision.clone()
            }
        }
    }

    // -- CAPI-01 -------------------------------------------------------------

    /// `code.analyze_repository` ? accept the analysis and run it to completion.
    ///
    /// A gateway with a worker pool calls [`Self::submit_analysis`] and
    /// [`Self::run_analysis`] separately; this convenience method is what the
    /// in-process reference deployment uses.
    pub fn analyze_repository(
        &self,
        ctx: &RequestContext,
        request: &AnalyzeRepository,
    ) -> CodeResult<JobHandle> {
        let handle = self.submit_analysis(ctx, request)?;
        if handle.stage == Stage::Queued.as_str() {
            self.run_analysis(ctx, &handle.job_id)?;
        }
        let state = self.lock();
        let job = state.jobs.get(&handle.job_id).map_err(map_job_error)?;
        Ok(JobHandle {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            stage: job.stage.as_str().to_string(),
        })
    }

    /// Accept an analysis job without running it (`async: true` semantics).
    pub fn submit_analysis(
        &self,
        ctx: &RequestContext,
        request: &AnalyzeRepository,
    ) -> CodeResult<JobHandle> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        // An alias registered on the source port (for example `@cwd`) is
        // resolved to its concrete revision before validation, so every
        // transport can use the alias.
        let request = AnalyzeRepository {
            revision: self.resolve_revision(&request.revision),
            ..request.clone()
        };
        request.revision.validate()?;
        if !request.asynchronous {
            return Err(CodeError::InvalidArgument {
                field: "async",
                reason: "code.analyze_repository is asynchronous; pass `async: true` (§32.2)"
                    .into(),
            });
        }
        if request.parser_profile.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "parser_profile",
                reason: "must be non-empty".into(),
            });
        }
        let snapshot = self.sources.snapshot(&request.revision)?;
        if snapshot.dirty && request.revision.dirty_policy == DirtyPolicy::Reject {
            return Err(CodeError::SourceUnavailable {
                revision: request.revision.commit_sha.clone(),
            });
        }
        self.require_parseable(&snapshot, &request)?;

        let dedup_key = format!(
            "{}|{}|{}|{}|{}|{}",
            ctx.scope.tenant_id,
            ctx.scope.project_id,
            request.revision.repository_id,
            request.revision.commit_sha,
            request.parser_profile,
            options_fingerprint(&request.include_globs, &request.exclude_globs)
        );

        let mut state = self.lock();
        if let Some(existing) = state.dedup.get(&dedup_key).cloned() {
            // Only a job that is still running, or that succeeded, may be joined.
            // A cancelled or failed job must not block a legitimate retry.
            let joinable = state
                .jobs
                .get(&existing)
                .map(|job| !matches!(job.stage, Stage::Cancelled | Stage::Failed | Stage::Stale))
                .unwrap_or(false);
            if joinable {
                self.remember_idempotency(&mut state, ctx, &dedup_key, &existing)?;
                let job = state.jobs.get(&existing).map_err(map_job_error)?;
                return Ok(JobHandle {
                    job_id: job.job_id.clone(),
                    kind: job.kind.clone(),
                    stage: job.stage.as_str().to_string(),
                });
            }
        }
        self.remember_idempotency(&mut state, ctx, &dedup_key, "")?;

        let sequence = state.next_sequence();
        let job_id = format!("job-code-{sequence}");
        state.jobs.submit(&job_id, "code.analyze_repository");
        state.job_scopes.insert(job_id.clone(), ctx.scope.clone());
        state.dedup.insert(dedup_key.clone(), job_id.clone());
        state.job_requests.insert(job_id.clone(), request.clone());
        self.remember_idempotency(&mut state, ctx, &dedup_key, &job_id)?;
        let job = state.jobs.get(&job_id).map_err(map_job_error)?;
        Ok(JobHandle {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            stage: job.stage.as_str().to_string(),
        })
    }

    /// Drive a queued job to completion. Idempotent: a job past `QUEUED` is
    /// returned as-is, so a cancelled or failed job is never resurrected.
    pub fn run_analysis(&self, ctx: &RequestContext, job_id: &str) -> CodeResult<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        let (request, stage) =
            {
                let state = self.lock();
                let scope = state.job_scopes.get(job_id).cloned().ok_or_else(|| {
                    CodeError::JobNotFound {
                        job_id: job_id.to_string(),
                    }
                })?;
                require_same_scope(ctx, &scope)?;
                let job = state.jobs.get(job_id).map_err(map_job_error)?;
                (state.job_requests.get(job_id).cloned(), job.stage)
            };
        let request = request.ok_or_else(|| CodeError::JobNotFound {
            job_id: job_id.to_string(),
        })?;
        if stage != Stage::Queued {
            return self.job_view(ctx, job_id);
        }

        let snapshot = self.sources.snapshot(&request.revision)?;
        let discovered = discover(&snapshot, &request.include_globs, &request.exclude_globs);
        let parseable: Vec<&SourceFile> = discovered
            .iter()
            .filter(|file| {
                file.language()
                    .map(|language| self.parser.support(&language) == SupportLevel::Ported)
                    .unwrap_or(false)
            })
            .copied()
            .collect();

        {
            let mut state = self.lock();
            advance_to(&mut state, job_id, Stage::Parsing)?;
        }
        let built = self.build_revision(ctx, &request, &snapshot, &discovered, &parseable);
        let (mut index, diagnostics) = match built {
            Ok(value) => value,
            Err(error) => {
                let mut state = self.lock();
                let _ = state.jobs.fail(job_id, &error.to_string());
                return Err(error);
            }
        };

        let (generation, revision_label) = {
            let mut state = self.lock();
            advance_to(&mut state, job_id, Stage::Staging)?;
            let sequence = state.next_sequence();
            let key = RevisionKey::new(
                &ctx.scope,
                &request.revision.repository_id,
                &request.revision.commit_sha,
            );
            let generation = key.generation(sequence);
            let label = key.label();
            state
                .job_revisions
                .insert(job_id.to_string(), label.clone());
            (generation, label)
        };
        index.generation = generation.clone();
        index.manifest.graph_generation = generation.clone();
        index.manifest.diagnostics = diagnostics;

        {
            let mut state = self.lock();
            advance_to(&mut state, job_id, Stage::Committing)?;
            state.revisions.insert(revision_label, index.clone());
            state.latest.insert(
                repository_latest_key(&ctx.scope, &index.repository_id),
                index.commit_sha.clone(),
            );
            state.generation.publish(&generation);
            advance_to(&mut state, job_id, Stage::Ready)?;
        }
        self.job_view(ctx, job_id)
    }

    fn require_parseable(
        &self,
        snapshot: &SourceSnapshot,
        request: &AnalyzeRepository,
    ) -> CodeResult<()> {
        let discovered = discover(snapshot, &request.include_globs, &request.exclude_globs);
        let parseable = discovered.iter().any(|file| {
            file.language()
                .map(|language| self.parser.support(&language) == SupportLevel::Ported)
                .unwrap_or(false)
        });
        if parseable {
            return Ok(());
        }
        let mut languages: Vec<String> = discovered
            .iter()
            .filter_map(|file| file.language())
            .collect();
        languages.sort();
        languages.dedup();
        Err(CodeError::CapabilityUnavailable {
            language: if languages.is_empty() {
                "unknown (no recognized source file in the revision)".to_string()
            } else {
                languages.join(",")
            },
        })
    }

    fn remember_idempotency(
        &self,
        state: &mut ServiceState,
        ctx: &RequestContext,
        fingerprint: &str,
        job_id: &str,
    ) -> CodeResult<()> {
        let Some(key) = &ctx.idempotency_key else {
            return Ok(());
        };
        match state.idempotency.get(key) {
            Some((stored, existing)) if stored == fingerprint => {
                if existing.is_empty() && !job_id.is_empty() {
                    state
                        .idempotency
                        .insert(key.clone(), (fingerprint.to_string(), job_id.to_string()));
                }
                Ok(())
            }
            Some(_) => Err(CodeError::IdempotencyConflict { key: key.clone() }),
            None => {
                state
                    .idempotency
                    .insert(key.clone(), (fingerprint.to_string(), job_id.to_string()));
                Ok(())
            }
        }
    }

    fn build_revision(
        &self,
        ctx: &RequestContext,
        request: &AnalyzeRepository,
        _snapshot: &SourceSnapshot,
        discovered: &[&SourceFile],
        parseable: &[&SourceFile],
    ) -> CodeResult<(RevisionIndex, Vec<CodeDiagnostic>)> {
        let mut symbols: BTreeMap<String, Symbol> = BTreeMap::new();
        let mut edges: Vec<CodeEdge> = Vec::new();
        let mut sources: BTreeMap<String, String> = BTreeMap::new();
        let mut diagnostics: Vec<CodeDiagnostic> = Vec::new();
        let mut skipped = 0u64;

        for file in discovered {
            let Some(language) = file.language() else {
                skipped += 1;
                diagnostics.push(CodeDiagnostic {
                    severity: crate::DiagnosticSeverity::Info,
                    code: "CAPABILITY_UNAVAILABLE".to_string(),
                    message: format!("{}: no language for this extension", file.path),
                });
                continue;
            };
            if !parseable
                .iter()
                .any(|candidate| candidate.path == file.path)
            {
                skipped += 1;
                diagnostics.push(CodeDiagnostic {
                    severity: crate::DiagnosticSeverity::Warning,
                    code: "CAPABILITY_UNAVAILABLE".to_string(),
                    message: format!("{}: no ported grammar for {}", file.path, language),
                });
                continue;
            }
            let parsed: ParsedSource = self.parser.parse(&file.path, &language, &file.content)?;
            diagnostics.extend(parsed.diagnostics.iter().cloned());
            sources.insert(file.path.clone(), file.content.clone());
            index_parsed(&request.revision, &parsed, &mut symbols, &mut edges);
        }

        if symbols.is_empty() {
            return Err(CodeError::SourceParseFailed {
                message: "no symbol could be extracted from the revision".to_string(),
            });
        }
        let unresolved = edges
            .iter()
            .filter(|edge| edge.resolution == EdgeResolution::Unresolved)
            .count();
        if unresolved > 0 {
            diagnostics.push(CodeDiagnostic {
                severity: crate::DiagnosticSeverity::Info,
                code: "UNRESOLVED_REFERENCE".to_string(),
                message: format!("{unresolved} reference(s) could not be resolved"),
            });
        }
        let manifest = CodeAnalysisManifest {
            repository_id: request.revision.repository_id.clone(),
            commit_sha: request.revision.commit_sha.clone(),
            parser_profile: request.parser_profile.clone(),
            indexed_files: sources.len() as u64,
            skipped_files: skipped,
            diagnostics: Vec::new(),
            graph_generation: String::new(),
        };
        let mut index = RevisionIndex {
            scope: ctx.scope.clone(),
            repository_id: request.revision.repository_id.clone(),
            commit_sha: request.revision.commit_sha.clone(),
            generation: String::new(),
            etag: format!("\"etag-{}-{}\"", request.revision.commit_sha, sources.len()),
            stale: false,
            manifest,
            symbols,
            edges,
            incoming: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            sources,
        };
        index.rebuild_adjacency();
        Ok((index, diagnostics))
    }

    // -- CAPI-02 / CAPI-03 ---------------------------------------------------

    /// `code.get_analysis_job`.
    pub fn get_analysis_job(&self, ctx: &RequestContext, request: &GetJob) -> CodeResult<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        self.job_view(ctx, &request.job_id)
    }

    fn job_view(&self, ctx: &RequestContext, job_id: &str) -> CodeResult<JobView> {
        let state = self.lock();
        let job = state.jobs.get(job_id).map_err(map_job_error)?;
        let scope = state
            .job_scopes
            .get(job_id)
            .ok_or_else(|| CodeError::JobNotFound {
                job_id: job_id.to_string(),
            })?;
        require_same_scope(ctx, scope)?;
        let result_ref = if job.outcome.commit == indexing_engine::CommitPhase::Committed {
            state.job_revisions.get(job_id).cloned()
        } else {
            None
        };
        Ok(JobView {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            stage: job.stage.as_str().to_string(),
            canonical_committed: job.outcome.commit == indexing_engine::CommitPhase::Committed,
            result_ref,
            diagnostics: job.outcome.diagnostics.clone(),
        })
    }

    /// Authoritative manifest of a committed analysis job.
    pub fn analysis_manifest(
        &self,
        ctx: &RequestContext,
        job_id: &str,
    ) -> CodeResult<CodeAnalysisManifest> {
        let view = self.get_analysis_job(
            ctx,
            &GetJob {
                job_id: job_id.to_string(),
            },
        )?;
        let label = view.result_ref.ok_or_else(|| CodeError::IndexNotReady {
            revision: job_id.to_string(),
        })?;
        let state = self.lock();
        let index = state
            .revisions
            .get(&label)
            .ok_or_else(|| CodeError::IndexNotReady {
                revision: label.clone(),
            })?;
        Ok(index.manifest.clone())
    }

    /// `code.cancel_analysis_job`.
    pub fn cancel_analysis_job(
        &self,
        ctx: &RequestContext,
        request: &CancelJob,
    ) -> CodeResult<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        let mut state = self.lock();
        let scope = state
            .job_scopes
            .get(&request.job_id)
            .cloned()
            .ok_or_else(|| CodeError::JobNotFound {
                job_id: request.job_id.clone(),
            })?;
        require_same_scope(ctx, &scope)?;
        let job = state
            .jobs
            .cancel(&request.job_id, &request.reason)
            .map_err(map_job_error)?;
        Ok(JobView {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            stage: job.stage.as_str().to_string(),
            canonical_committed: false,
            result_ref: None,
            diagnostics: job.outcome.diagnostics.clone(),
        })
    }

    // -- CAPI-04 -------------------------------------------------------------

    /// `code.get_symbol`.
    pub fn get_symbol(&self, ctx: &RequestContext, request: &GetSymbol) -> CodeResult<Symbol> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let index = self.index_containing(ctx, &state, &request.symbol_id)?;
        let symbol = index
            .symbols
            .get(&request.symbol_id)
            .cloned()
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("symbol {}", request.symbol_id),
            })?;
        self.authorizer.check_symbol(ctx, &symbol)?;
        Ok(symbol)
    }

    // -- CAPI-05 -------------------------------------------------------------

    /// `code.find_symbols`.
    pub fn find_symbols(
        &self,
        ctx: &RequestContext,
        request: &SymbolQuery,
    ) -> CodeResult<Page<Symbol>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;
        let needle = request.query.to_ascii_lowercase();
        let mut items: Vec<Symbol> = Vec::new();
        for symbol in index.symbols_in_path_order() {
            if !needle.is_empty()
                && !symbol.name.to_ascii_lowercase().contains(&needle)
                && !symbol.qualified_name.to_ascii_lowercase().contains(&needle)
            {
                continue;
            }
            if !request.kinds.is_empty() && !request.kinds.contains(&symbol.kind) {
                continue;
            }
            if let Some(language) = &request.language {
                if &symbol.language != language {
                    continue;
                }
            }
            if let Some(glob) = &request.path_glob {
                if !glob_match(glob, path_of(&symbol.definition)) {
                    continue;
                }
            }
            if self.authorizer.check_symbol(ctx, symbol).is_err() {
                continue;
            }
            items.push(symbol.clone());
        }
        Page::of(items, &request.page)
    }

    // -- CAPI-06 -------------------------------------------------------------

    /// `code.find_references`.
    pub fn find_references(
        &self,
        ctx: &RequestContext,
        request: &ReferenceQuery,
    ) -> CodeResult<Page<ReferenceLocation>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_containing(ctx, &state, &request.symbol_id)?;
        let symbol = index
            .symbols
            .get(&request.symbol_id)
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("symbol {}", request.symbol_id),
            })?;
        self.authorizer.check_symbol(ctx, symbol)?;

        let mut items: Vec<ReferenceLocation> = Vec::new();
        if request.include_declaration {
            items.push(ReferenceLocation {
                symbol_id: symbol.id.clone(),
                location: symbol.definition.clone(),
                kind: CodeEdgeKind::Defines,
            });
        }
        for position in index
            .incoming
            .get(&request.symbol_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let edge = index.edge_at(*position);
            if edge.kind == CodeEdgeKind::Defines {
                continue;
            }
            if !request.relation_kinds.is_empty() && !request.relation_kinds.contains(&edge.kind) {
                continue;
            }
            let Some(source) = index.symbols.get(&edge.source) else {
                continue;
            };
            if self.authorizer.check_symbol(ctx, source).is_err() {
                continue;
            }
            items.push(ReferenceLocation {
                symbol_id: source.id.clone(),
                location: source.definition.clone(),
                kind: edge.kind,
            });
        }
        items.sort_by(|left, right| {
            (
                path_of(&left.location),
                left.location.line_start,
                &left.symbol_id,
            )
                .cmp(&(
                    path_of(&right.location),
                    right.location.line_start,
                    &right.symbol_id,
                ))
        });
        items.dedup_by(|left, right| left.symbol_id == right.symbol_id && left.kind == right.kind);
        Page::of(items, &request.page)
    }

    // -- CAPI-07 / CAPI-08 / CAPI-09 ----------------------------------------

    /// `code.query_callers` (`direction = in`).
    pub fn query_callers(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>> {
        self.traverse(ctx, request, Direction::In)
    }

    /// `code.query_callees` (`direction = out`).
    pub fn query_callees(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>> {
        self.traverse(ctx, request, Direction::Out)
    }

    /// `code.query_dependencies` — outgoing `depends_on` / `imports` edges.
    pub fn query_dependencies(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>> {
        let allowed = [CodeEdgeKind::DependsOn, CodeEdgeKind::Imports];
        let kinds: Vec<CodeEdgeKind> = if request.edge_kinds.is_empty() {
            allowed.to_vec()
        } else {
            request.edge_kinds.clone()
        };
        if !kinds.iter().any(|kind| allowed.contains(kind)) {
            return Err(CodeError::InvalidArgument {
                field: "edge_kinds",
                reason: "code.query_dependencies needs `depends_on` or `imports`".into(),
            });
        }
        let mut narrowed = request.clone();
        narrowed.edge_kinds = kinds;
        self.traverse(ctx, &narrowed, Direction::Out)
    }

    fn traverse(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
        direction: Direction,
    ) -> CodeResult<Page<CodeEdge>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        check_bounds(request.max_depth, request.max_nodes)?;
        let state = self.lock();
        let index = self.index_containing(ctx, &state, &request.symbol_id)?;
        let start = index
            .symbols
            .get(&request.symbol_id)
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("symbol {}", request.symbol_id),
            })?;
        self.authorizer.check_symbol(ctx, start)?;

        let mut visited: Vec<String> = vec![request.symbol_id.clone()];
        let mut queue: Vec<(String, u32)> = vec![(request.symbol_id.clone(), 0)];
        let mut collected: Vec<CodeEdge> = Vec::new();
        let mut seen_edges: BTreeMap<(String, String, CodeEdgeKind), ()> = BTreeMap::new();

        while let Some((node, depth)) = queue.pop() {
            if depth >= request.max_depth {
                continue;
            }
            let positions: &[usize] = match direction {
                Direction::In => index
                    .incoming
                    .get(&node)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                Direction::Out => index
                    .outgoing
                    .get(&node)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            };
            for position in positions {
                let edge = index.edge_at(*position);
                // containment is structural, not a reference: it is never
                // followed by a caller/callee/dependency traversal
                if edge.kind == CodeEdgeKind::Defines {
                    continue;
                }
                if !request.edge_kinds.is_empty() && !request.edge_kinds.contains(&edge.kind) {
                    continue;
                }
                let neighbour = match direction {
                    Direction::In => edge.source.clone(),
                    Direction::Out => match &edge.target {
                        Some(target) => target.clone(),
                        None => continue,
                    },
                };
                let Some(symbol) = index.symbols.get(&neighbour) else {
                    continue;
                };
                if self.authorizer.check_symbol(ctx, symbol).is_err() {
                    continue;
                }
                let fingerprint = (
                    edge.source.clone(),
                    edge.target.clone().unwrap_or_default(),
                    edge.kind,
                );
                if seen_edges.insert(fingerprint, ()).is_none() {
                    collected.push(edge.clone());
                }
                if !visited.iter().any(|seen| seen == &neighbour) {
                    if visited.len() as u32 >= request.max_nodes {
                        return Err(CodeError::ResourceLimit {
                            detail: format!(
                                "traversal exceeded max_nodes={} at depth {}",
                                request.max_nodes,
                                depth + 1
                            ),
                        });
                    }
                    visited.push(neighbour.clone());
                    queue.push((neighbour, depth + 1));
                }
            }
        }
        collected.sort_by(|left, right| {
            (left.kind as u8, &left.source, &left.target).cmp(&(
                right.kind as u8,
                &right.source,
                &right.target,
            ))
        });
        Page::of(collected, &request.page)
    }

    // -- CAPI-10 -------------------------------------------------------------

    /// `code.analyze_impact`.
    pub fn analyze_impact(
        &self,
        ctx: &RequestContext,
        request: &ImpactQuery,
    ) -> CodeResult<ImpactReport> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        check_bounds(request.max_depth, request.max_nodes)?;
        if request.changed.is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "changed",
                reason: "must name at least one symbol".into(),
            });
        }
        let state = self.lock();
        let index = self.index_containing(ctx, &state, &request.changed[0])?;
        if index.commit_sha != request.revision {
            return Err(CodeError::SourceRevisionMismatch {
                requested: request.revision.clone(),
                actual: index.commit_sha.clone(),
            });
        }
        for symbol_id in &request.changed {
            if !index.symbols.contains_key(symbol_id) {
                return Err(CodeError::NotFound {
                    resource: format!("symbol {symbol_id}"),
                });
            }
        }

        let kinds: Vec<CodeEdgeKind> = if request.edge_kinds.is_empty() {
            vec![
                CodeEdgeKind::Calls,
                CodeEdgeKind::References,
                CodeEdgeKind::Implements,
                CodeEdgeKind::DependsOn,
            ]
        } else {
            request.edge_kinds.clone()
        };

        let mut visited: BTreeMap<String, ()> =
            request.changed.iter().map(|id| (id.clone(), ())).collect();
        let mut queue: Vec<(String, u32, Vec<CodeEdge>)> = request
            .changed
            .iter()
            .map(|id| (id.clone(), 0u32, Vec::new()))
            .collect();
        let mut impacted: Vec<(u32, String)> = Vec::new();
        let mut paths: Vec<Vec<CodeEdge>> = Vec::new();
        let mut unresolved_dynamic_calls = 0u32;
        let mut saw_possible_call = false;
        let mut truncated = false;

        while let Some((node, depth, path)) = queue.pop() {
            if depth >= request.max_depth {
                truncated = true;
                continue;
            }
            let positions: &[usize] = index
                .incoming
                .get(&node)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for position in positions {
                let edge = index.edge_at(*position);
                if edge.kind == CodeEdgeKind::Defines {
                    continue;
                }
                if !kinds.contains(&edge.kind) {
                    continue;
                }
                if edge.kind == CodeEdgeKind::Calls {
                    match edge.resolution {
                        EdgeResolution::Unresolved => unresolved_dynamic_calls += 1,
                        EdgeResolution::StaticPossible => saw_possible_call = true,
                        EdgeResolution::StaticExact => {}
                    }
                }
                let Some(symbol) = index.symbols.get(&edge.source) else {
                    continue;
                };
                if self.authorizer.check_symbol(ctx, symbol).is_err() {
                    continue;
                }
                if visited.contains_key(&edge.source) {
                    continue;
                }
                if visited.len() as u32 >= request.max_nodes {
                    return Err(CodeError::ResourceLimit {
                        detail: format!("impact analysis exceeded max_nodes={}", request.max_nodes),
                    });
                }
                visited.insert(edge.source.clone(), ());
                let mut next_path = path.clone();
                next_path.push(edge.clone());
                impacted.push((depth + 1, edge.source.clone()));
                paths.push(next_path.clone());
                queue.push((edge.source.clone(), depth + 1, next_path));
            }
        }

        impacted.sort();
        let mut test_candidates: Vec<String> = Vec::new();
        if request.include_test_candidates {
            for (_, symbol_id) in &impacted {
                if let Some(symbol) = index.symbols.get(symbol_id) {
                    if looks_like_test(symbol) {
                        test_candidates.push(symbol_id.clone());
                    }
                }
            }
            test_candidates.sort();
        }

        // §32.1: a graph containing dynamic/possible calls must never be
        // presented as a complete call graph.
        let completeness = if truncated || unresolved_dynamic_calls > 0 || saw_possible_call {
            "partial"
        } else {
            "complete"
        };
        let mut evidence_ids: Vec<String> = paths
            .iter()
            .flat_map(|path| path.iter().flat_map(|edge| edge.evidence_ids.clone()))
            .collect();
        evidence_ids.sort();
        evidence_ids.dedup();

        Ok(ImpactReport {
            impacted: impacted.into_iter().map(|(_, id)| id).collect(),
            paths,
            test_candidates,
            unresolved_dynamic_calls,
            completeness: completeness.to_string(),
            evidence_ids,
        })
    }

    // -- CAPI-11 -------------------------------------------------------------

    /// `code.get_edit_context`.
    pub fn get_edit_context(
        &self,
        ctx: &RequestContext,
        request: &EditContextQuery,
    ) -> CodeResult<EditContextResult> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_tokens == 0 {
            return Err(CodeError::ContextBudgetExceeded {
                required: 1,
                budget: 0,
            });
        }
        if request.target_refs.is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "target_refs",
                reason: "must name at least one symbol".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut blocks: Vec<ContextBlock> = Vec::new();
        let mut omitted_refs: Vec<String> = Vec::new();
        let mut total_tokens = 0u32;

        for symbol_id in &request.target_refs {
            let symbol = index
                .symbols
                .get(symbol_id)
                .ok_or_else(|| CodeError::NotFound {
                    resource: format!("symbol {symbol_id}"),
                })?;
            let path = path_of(&symbol.definition);
            self.authorizer.check_symbol(ctx, symbol)?;
            self.authorizer.check_path(ctx, path)?;
            let content = slice_lines(
                index
                    .sources
                    .get(path)
                    .map(String::as_str)
                    .unwrap_or_default(),
                symbol.definition.line_start,
                symbol.definition.line_end,
            );
            let token_count = estimate_tokens(&content);
            if token_count > request.max_tokens {
                if blocks.is_empty() {
                    return Err(CodeError::ContextBudgetExceeded {
                        required: token_count,
                        budget: request.max_tokens,
                    });
                }
                omitted_refs.push(symbol.id.clone());
                continue;
            }
            if total_tokens.saturating_add(token_count) > request.max_tokens {
                omitted_refs.push(symbol.id.clone());
                continue;
            }
            total_tokens += token_count;
            blocks.push(ContextBlock {
                symbol_id: symbol.id.clone(),
                content,
                token_count,
                evidence_ids: symbol.evidence_ids.clone(),
            });
        }

        Ok(EditContextResult {
            blocks,
            total_tokens,
            truncation: !omitted_refs.is_empty(),
            omitted_refs,
        })
    }

    // -- CAPI-12 -------------------------------------------------------------

    /// `code.compare_revisions`.
    pub fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &CodeRevisionDiff,
    ) -> CodeResult<Page<CodeChange>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let from = self.index_for(ctx, &state, &request.repository_id, &request.from_revision)?;
        let to = self.index_for(ctx, &state, &request.repository_id, &request.to_revision)?;

        let key_of = |symbol: &Symbol| -> (String, String, String) {
            (
                path_of(&symbol.definition).to_string(),
                symbol.qualified_name.clone(),
                format!("{:?}", symbol.kind),
            )
        };
        let from_symbols: BTreeMap<(String, String, String), &Symbol> = from
            .symbols
            .values()
            .map(|symbol| (key_of(symbol), symbol))
            .collect();
        let to_symbols: BTreeMap<(String, String, String), &Symbol> = to
            .symbols
            .values()
            .map(|symbol| (key_of(symbol), symbol))
            .collect();

        let mut changes: Vec<CodeChange> = Vec::new();
        for (key, old) in &from_symbols {
            match to_symbols.get(key) {
                None => changes.push(CodeChange {
                    change_kind: ChangeKind::Removed,
                    old_symbol: Some((*old).clone()),
                    new_symbol: None,
                    evidence_ids: old.evidence_ids.clone(),
                }),
                Some(new) => {
                    if old.signature != new.signature || old.definition != new.definition {
                        changes.push(CodeChange {
                            change_kind: ChangeKind::Modified,
                            old_symbol: Some((*old).clone()),
                            new_symbol: Some((*new).clone()),
                            evidence_ids: new.evidence_ids.clone(),
                        });
                    }
                }
            }
        }
        for (key, new) in &to_symbols {
            if !from_symbols.contains_key(key) {
                changes.push(CodeChange {
                    change_kind: ChangeKind::Added,
                    old_symbol: None,
                    new_symbol: Some((*new).clone()),
                    evidence_ids: new.evidence_ids.clone(),
                });
            }
        }
        changes.sort_by(|left, right| {
            let left_old = left.old_symbol.as_ref().or(left.new_symbol.as_ref());
            let right_old = right.old_symbol.as_ref().or(right.new_symbol.as_ref());
            match (left_old, right_old) {
                (Some(a), Some(b)) => (
                    path_of(&a.definition),
                    &a.qualified_name,
                    left.change_kind as u8,
                )
                    .cmp(&(
                        path_of(&b.definition),
                        &b.qualified_name,
                        right.change_kind as u8,
                    )),
                _ => std::cmp::Ordering::Equal,
            }
        });
        Page::of(changes, &request.page)
    }

    // -- CAPI-13 -------------------------------------------------------------

    /// `code.invalidate_revision` — mark a revision stale without deleting the
    /// historical content (§32.2, §32.3).
    pub fn invalidate_revision(
        &self,
        ctx: &RequestContext,
        request: &InvalidateCodeRevision,
    ) -> CodeResult<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX_ADMIN)?;
        let label = {
            let state = self.lock();
            // deliberately no freshness gate: re-invalidating a stale revision
            // must report `changed: false` instead of failing
            let index = lookup_index(ctx, &state, &request.repository_id, &request.revision)?;
            RevisionKey::new(&index.scope, &index.repository_id, &index.commit_sha).label()
        };
        let mut state = self.lock();
        let index =
            state
                .revisions
                .get_mut(&label)
                .ok_or_else(|| CodeError::SourceUnavailable {
                    revision: request.revision.clone(),
                })?;
        if index.etag != request.expected_etag {
            return Err(CodeError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual: index.etag.clone(),
            });
        }
        let changed = !index.stale;
        index.stale = true;
        // The historical content stays; only the freshness marker and the etag
        // move, so a later re-analysis can supersede this revision.
        let next_etag = format!("\"stale-{}\"", index.generation);
        index.etag = next_etag.clone();
        let mut events = Vec::new();
        if changed {
            events.push("code.revision.stale".to_string());
        }
        Ok(ChangeReceipt {
            resource_id: label,
            etag: next_etag,
            changed,
            events,
        })
    }

    // -- internal lookups ----------------------------------------------------

    fn index_for<'a>(
        &self,
        ctx: &RequestContext,
        state: &'a ServiceState,
        repository_id: &str,
        revision: &str,
    ) -> CodeResult<&'a RevisionIndex> {
        // Aliases (e.g. `@cwd`) resolve at the service layer so every
        // transport (HTTP, CLI, MCP) resolves them identically.
        let resolved = self.sources.resolve(revision);
        let key = RevisionKey::new(&ctx.scope, repository_id, &resolved).label();
        let index = state
            .revisions
            .get(&key)
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("revision {revision} of repository {repository_id}"),
            })?;
        require_same_scope(ctx, &index.scope)?;
        check_fresh(ctx, index)?;
        Ok(index)
    }

    fn index_containing<'a>(
        &self,
        ctx: &RequestContext,
        state: &'a ServiceState,
        symbol_id: &str,
    ) -> CodeResult<&'a RevisionIndex> {
        let mut matches: Vec<&RevisionIndex> = state
            .revisions
            .values()
            .filter(|index| {
                index.scope.tenant_id == ctx.scope.tenant_id
                    && index.scope.project_id == ctx.scope.project_id
                    && index.symbols.contains_key(symbol_id)
            })
            .collect();
        matches.sort_by(|left, right| left.commit_sha.cmp(&right.commit_sha));
        let index = matches.pop().ok_or_else(|| CodeError::NotFound {
            resource: format!("symbol {symbol_id}"),
        })?;
        check_fresh(ctx, index)?;
        Ok(index)
    }

    // -- Parity operations (CodeGraph gap closure) ---------------------------

    /// `code.get_ai_context`: intent-aware context assembly.
    pub fn get_ai_context(
        &self,
        ctx: &RequestContext,
        request: &AiContextQuery,
    ) -> CodeResult<AiContextResult> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_tokens == 0 {
            return Err(CodeError::ContextBudgetExceeded {
                required: 1,
                budget: 0,
            });
        }
        if request.target_refs.is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "target_refs",
                reason: "must name at least one symbol".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut sections: Vec<AiContextSection> = Vec::new();
        let mut total = 0u32;
        let mut truncated = false;

        for symbol_id in &request.target_refs {
            let symbol = index
                .symbols
                .get(symbol_id)
                .ok_or_else(|| CodeError::NotFound {
                    resource: format!("symbol {symbol_id}"),
                })?;
            self.authorizer.check_symbol(ctx, symbol)?;
            self.authorizer
                .check_path(ctx, path_of(&symbol.definition))?;
            let body = slice_lines(
                index
                    .sources
                    .get(path_of(&symbol.definition))
                    .map(String::as_str)
                    .unwrap_or_default(),
                symbol.definition.line_start,
                symbol.definition.line_end,
            );

            let parts: Vec<(String, String)> = match request.intent {
                ContextIntent::Explain => vec![(
                    "definition".to_string(),
                    format!(
                        "{}{}\n{body}",
                        symbol.name,
                        symbol.signature.clone().unwrap_or_default()
                    ),
                )],
                ContextIntent::Modify => {
                    let callers = incoming_names(index, &symbol.id, CodeEdgeKind::Calls, 3);
                    let refs = incoming_names(index, &symbol.id, CodeEdgeKind::References, 3);
                    vec![
                        (
                            "definition".to_string(),
                            format!(
                                "{}{}\n{body}",
                                symbol.name,
                                symbol.signature.clone().unwrap_or_default()
                            ),
                        ),
                        ("callers".to_string(), join_or("none", &callers)),
                        ("references".to_string(), join_or("none", &refs)),
                    ]
                }
                ContextIntent::Debug => {
                    let callers = incoming_names(index, &symbol.id, CodeEdgeKind::Calls, 3);
                    vec![
                        (
                            "definition".to_string(),
                            format!(
                                "{}{}\n{body}",
                                symbol.name,
                                symbol.signature.clone().unwrap_or_default()
                            ),
                        ),
                        ("callers".to_string(), join_or("none", &callers)),
                        (
                            "debug_hints".to_string(),
                            error_hints_of(&body)
                                .into_iter()
                                .take(5)
                                .collect::<Vec<_>>()
                                .join("; "),
                        ),
                    ]
                }
                ContextIntent::Test => {
                    let tests = related_test_names(index, &symbol.id, 5);
                    let refs = incoming_names(index, &symbol.id, CodeEdgeKind::References, 3);
                    vec![
                        ("tests".to_string(), join_or("no related tests", &tests)),
                        ("references".to_string(), join_or("none", &refs)),
                    ]
                }
            };

            for (section, content) in parts {
                let token_count = estimate_tokens(&content);
                if token_count > request.max_tokens {
                    if sections.is_empty() {
                        return Err(CodeError::ContextBudgetExceeded {
                            required: token_count,
                            budget: request.max_tokens,
                        });
                    }
                    truncated = true;
                    continue;
                }
                if total.saturating_add(token_count) > request.max_tokens {
                    truncated = true;
                    continue;
                }
                total += token_count;
                sections.push(AiContextSection {
                    section,
                    content,
                    token_count,
                });
            }
        }

        Ok(AiContextResult {
            intent: request.intent,
            sections,
            token_total: total,
            truncated,
        })
    }

    /// `code.get_curated_context`: hybrid (keyword + static-semantic) search
    /// over the symbol corpus for a natural-language query.
    pub fn get_curated_context(
        &self,
        ctx: &RequestContext,
        request: &CuratedContextQuery,
    ) -> CodeResult<CuratedContext> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_results == 0 || request.max_results > 25 {
            return Err(CodeError::InvalidArgument {
                field: "max_results",
                reason: "must be in 1..=25".into(),
            });
        }
        if request.query.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "query",
                reason: "must be non-empty".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let policy = self.search_policy;
        let mut corpus: Vec<Vec<String>> = Vec::new();
        let mut order: Vec<String> = Vec::new();
        for symbol in index.symbols_in_path_order() {
            if self.authorizer.check_symbol(ctx, symbol).is_err() {
                continue;
            }
            let mut text = format!(
                "{} {} {}",
                symbol.name,
                symbol.qualified_name,
                symbol.signature.clone().unwrap_or_default()
            );
            if policy.full_body {
                // Pull the body from the indexed source via the definition
                // span (CodeGraph `--full-body-embedding`).
                let body = slice_lines(
                    index
                        .sources
                        .get(path_of(&symbol.definition))
                        .map(String::as_str)
                        .unwrap_or_default(),
                    symbol.definition.line_start,
                    symbol.definition.line_end,
                );
                if !body.is_empty() {
                    text.push(' ');
                    text.push_str(&body);
                }
            }
            corpus.push(crate::search::tokenize_with(&text, policy));
            order.push(symbol.id.clone());
        }
        let hybrid = crate::search::HybridIndex::build_with_embedder(
            &corpus,
            policy,
            self.embedder.clone(),
            self.backend_mode,
        );
        let ranked = hybrid.rank_with(&request.query, policy);

        let mut matches: Vec<CuratedMatch> = Vec::new();
        let mut sections: Vec<String> = Vec::new();
        for (doc_index, score) in ranked.into_iter().take(request.max_results as usize) {
            let symbol = index
                .symbols
                .get(&order[doc_index])
                .expect("corpus order is consistent with the symbol list");
            let snippet = symbol
                .signature
                .clone()
                .unwrap_or_else(|| symbol.qualified_name.clone());
            sections.push(format!(
                "### {} ({})\n{}\n{score:.2}",
                symbol.qualified_name, symbol.language, snippet
            ));
            matches.push(CuratedMatch {
                symbol_id: symbol.id.clone(),
                name: symbol.name.clone(),
                kind: symbol.kind,
                language: symbol.language.clone(),
                score,
                snippet,
            });
        }
        Ok(CuratedContext {
            query: request.query.clone(),
            matches,
            sections,
        })
    }

    /// `code.analyze_complexity`: deterministic keyword-based cyclomatic
    /// complexity with a breakdown. The heuristics are registered in ADR-058:
    /// `if/else/match/switch/when/case` count as branches, loop keywords as
    /// loops, and exception keywords as exception handling; nesting is the
    /// maximum brace depth.
    pub fn analyze_complexity(
        &self,
        ctx: &RequestContext,
        request: &ComplexityQuery,
    ) -> CodeResult<Vec<ComplexityReport>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_results == 0 || request.max_results > 100 {
            return Err(CodeError::InvalidArgument {
                field: "max_results",
                reason: "must be in 1..=100".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut candidates: Vec<&Symbol> = index
            .symbols
            .values()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
            .collect();
        if !request.symbol_ids.is_empty() {
            candidates.retain(|s| request.symbol_ids.contains(&s.id));
        }
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        if request.symbol_ids.is_empty() {
            candidates.truncate(request.max_results as usize);
        }

        let mut items: Vec<ComplexityReport> = Vec::new();
        for symbol in candidates {
            self.authorizer.check_symbol(ctx, symbol)?;
            let body = slice_lines(
                index
                    .sources
                    .get(path_of(&symbol.definition))
                    .map(String::as_str)
                    .unwrap_or_default(),
                symbol.definition.line_start,
                symbol.definition.line_end,
            );
            items.push(complexity_report(symbol, &body));
        }
        Ok(items)
    }

    /// `code.find_circular_deps`: DFS cycle detection over the module
    /// (file) graph built from `imports` / `depends_on` edges.
    pub fn find_circular_deps(
        &self,
        ctx: &RequestContext,
        request: &CircularDepsQuery,
    ) -> CodeResult<Page<CircularDependency>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let kinds: Vec<CodeEdgeKind> = if request.edge_kinds.is_empty() {
            vec![CodeEdgeKind::Imports, CodeEdgeKind::DependsOn]
        } else {
            request.edge_kinds.clone()
        };

        let path_of_symbol = |id: &str| {
            index
                .symbols
                .get(id)
                .map(|s| path_of(&s.definition).to_string())
        };

        let mut files: Vec<String> = index
            .symbols
            .values()
            .map(|s| path_of(&s.definition).to_string())
            .collect();
        files.sort();
        files.dedup();

        let mut graph: std::collections::BTreeMap<String, Vec<String>> = BTreeMap::new();
        for edge in &index.edges {
            if !kinds.contains(&edge.kind) {
                continue;
            }
            let Some(target) = &edge.target else {
                continue;
            };
            let (Some(source_path), Some(target_path)) =
                (path_of_symbol(&edge.source), path_of_symbol(target))
            else {
                continue;
            };
            if source_path != target_path {
                graph.entry(source_path).or_default().push(target_path);
            }
        }
        for neighbours in graph.values_mut() {
            neighbours.sort();
            neighbours.dedup();
        }

        let mut cycles: Vec<CircularDependency> = Vec::new();
        for root in &files {
            let mut stack: Vec<String> = Vec::new();
            let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut found = Vec::new();
            visit_cycles(root, &graph, &mut stack, &mut visited, &mut found);
            for cycle in found {
                cycles.push(CircularDependency {
                    files: cycle,
                    edge_kinds: kinds.to_vec(),
                });
            }
        }
        cycles.sort_by(|left, right| left.files.cmp(&right.files));
        cycles.dedup();
        Page::of(cycles, &request.page)
    }

    /// `code.find_hot_paths`: functions ranked by distinct transitive caller
    /// count (reverse breadth-first search over `calls` edges).
    pub fn find_hot_paths(
        &self,
        ctx: &RequestContext,
        request: &HotPathsQuery,
    ) -> CodeResult<Vec<HotPath>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        if request.max_results == 0 || request.max_results > 100 {
            return Err(CodeError::InvalidArgument {
                field: "max_results",
                reason: "must be in 1..=100".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut items: Vec<HotPath> = Vec::new();
        for symbol in index
            .symbols
            .values()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        {
            let direct = index
                .incoming
                .get(&symbol.id)
                .map(|positions| {
                    positions
                        .iter()
                        .filter(|p| index.edges[**p].kind == CodeEdgeKind::Calls)
                        .count() as u32
                })
                .unwrap_or(0);
            let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut frontier: Vec<String> = Vec::new();
            if let Some(positions) = index.incoming.get(&symbol.id) {
                for position in positions {
                    if index.edges[*position].kind == CodeEdgeKind::Calls {
                        frontier.push(index.edges[*position].source.clone());
                    }
                }
            }
            while let Some(current) = frontier.pop() {
                if !seen.insert(current.clone()) {
                    continue;
                }
                if let Some(positions) = index.incoming.get(&current) {
                    for position in positions {
                        if index.edges[*position].kind == CodeEdgeKind::Calls {
                            frontier.push(index.edges[*position].source.clone());
                        }
                    }
                }
            }
            seen.remove(&symbol.id);
            let transitive = seen.len() as u32;
            if (transitive > 0 || direct > 0) && self.authorizer.check_symbol(ctx, symbol).is_ok() {
                items.push(HotPath {
                    symbol_id: symbol.id.clone(),
                    name: symbol.name.clone(),
                    transitive_caller_count: transitive,
                    direct_caller_count: direct,
                });
            }
        }
        items.sort_by(|left, right| {
            right
                .transitive_caller_count
                .cmp(&left.transitive_caller_count)
                .then(left.symbol_id.cmp(&right.symbol_id))
        });
        items.truncate(request.max_results as usize);
        Ok(items)
    }

    /// `code.find_dead_imports`: an import is dead when no reference,
    /// dependency or implementation edge from the importing file names a
    /// symbol whose qualified name contains the imported module's last
    /// segment (documented heuristic, ADR-058).
    pub fn find_dead_imports(
        &self,
        ctx: &RequestContext,
        request: &DeadImportsQuery,
    ) -> CodeResult<Page<DeadImport>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut items: Vec<DeadImport> = Vec::new();
        for (position, edge) in index.edges.iter().enumerate() {
            if edge.kind != CodeEdgeKind::Imports {
                continue;
            }
            let Some(target) = &edge.target else {
                continue;
            };
            let segment = target.rsplit(['/', ':']).next().unwrap_or(target);
            let Some(source_symbol) = index.symbols.get(&edge.source) else {
                continue;
            };
            let source_path = path_of(&source_symbol.definition);
            let mut used = false;
            for (other_position, other) in index.edges.iter().enumerate() {
                if other_position == position
                    || !matches!(
                        other.kind,
                        CodeEdgeKind::References
                            | CodeEdgeKind::DependsOn
                            | CodeEdgeKind::Implements
                    )
                {
                    continue;
                }
                let Some(other_source) = index.symbols.get(&other.source) else {
                    continue;
                };
                if path_of(&other_source.definition) != source_path {
                    continue;
                }
                if let Some(other_target) =
                    other.target.as_ref().and_then(|id| index.symbols.get(id))
                {
                    let haystack = format!("{} {}", other_target.name, other_target.qualified_name)
                        .to_ascii_lowercase();
                    if haystack.contains(&segment.to_ascii_lowercase()) {
                        used = true;
                        break;
                    }
                }
            }
            if !used && self.authorizer.check_path(ctx, source_path).is_ok() {
                items.push(DeadImport {
                    importer_file: source_path.to_string(),
                    target: target.clone(),
                    edge_id: format!("edge-{position}"),
                });
            }
        }
        items.sort_by(|left, right| {
            left.importer_file
                .cmp(&right.importer_file)
                .then(left.edge_id.cmp(&right.edge_id))
        });
        Page::of(items, &request.page)
    }

    /// `code.get_module_summary`: per-directory grouping of the symbol set.
    pub fn get_module_summary(
        &self,
        ctx: &RequestContext,
        request: &ModuleSummaryQuery,
    ) -> CodeResult<Page<ModuleSummary>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut grouped: std::collections::BTreeMap<String, Vec<&Symbol>> = BTreeMap::new();
        for symbol in index.symbols.values() {
            let path = path_of(&symbol.definition);
            let module = module_directory(path);
            if let Some(prefix) = &request.path_prefix {
                if !module.starts_with(prefix) && path != prefix {
                    continue;
                }
            }
            grouped.entry(module).or_default().push(symbol);
        }

        let mut items: Vec<ModuleSummary> = Vec::new();
        for (module, symbols) in &grouped {
            let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut languages: std::collections::BTreeMap<String, u32> = BTreeMap::new();
            let mut function_count = 0u32;
            let mut reports: Vec<ComplexityReport> = Vec::new();
            for symbol in symbols.iter() {
                files.insert(path_of(&symbol.definition).to_string());
                *languages.entry(symbol.language.clone()).or_default() += 1;
                if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
                    function_count += 1;
                    let body = slice_lines(
                        index
                            .sources
                            .get(path_of(&symbol.definition))
                            .map(String::as_str)
                            .unwrap_or_default(),
                        symbol.definition.line_start,
                        symbol.definition.line_end,
                    );
                    reports.push(complexity_report(symbol, &body));
                }
            }
            reports.sort_by_key(|report| std::cmp::Reverse(report.cyclomatic));
            reports.truncate(5);
            if self.authorizer.check_path(ctx, module).is_err() {
                continue;
            }
            items.push(ModuleSummary {
                module: module.clone(),
                file_count: files.len() as u32,
                symbol_count: symbols.len() as u32,
                function_count,
                languages: languages
                    .into_iter()
                    .map(|(language, count)| crate::domain::LanguageCount { language, count })
                    .collect(),
                top_complex_functions: reports,
            });
        }
        Page::of(items, &request.page)
    }

    /// `code.search_by_pattern`: regex search over names, signatures, bodies
    /// and doc comments (first hit per field, documented).
    pub fn search_by_pattern(
        &self,
        ctx: &RequestContext,
        request: &PatternSearchQuery,
    ) -> CodeResult<Page<PatternMatch>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let regex =
            regex::Regex::new(&request.pattern).map_err(|_| CodeError::InvalidArgument {
                field: "pattern",
                reason: "invalid regex".into(),
            })?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let want =
            |scope: PatternScope, field: PatternScope| scope == PatternScope::All || scope == field;

        let mut items: Vec<PatternMatch> = Vec::new();
        for symbol in index.symbols_in_path_order() {
            if self.authorizer.check_symbol(ctx, symbol).is_err() {
                continue;
            }
            if want(request.scope, PatternScope::Name) && regex.is_match(&symbol.name) {
                items.push(PatternMatch {
                    symbol_id: symbol.id.clone(),
                    name: symbol.name.clone(),
                    matched_field: "name".to_string(),
                    line: 0,
                    snippet: symbol.name.clone(),
                });
            }
            if want(request.scope, PatternScope::Signature) {
                if let Some(signature) = &symbol.signature {
                    if let Some(position) = regex.find(signature) {
                        let line = symbol.definition.line_start
                            + signature[..position.start()].lines().count() as u32;
                        items.push(PatternMatch {
                            symbol_id: symbol.id.clone(),
                            name: symbol.name.clone(),
                            matched_field: "signature".to_string(),
                            line,
                            snippet: signature
                                .split_whitespace()
                                .take(8)
                                .collect::<Vec<_>>()
                                .join(" "),
                        });
                    }
                }
            }
            let body = slice_lines(
                index
                    .sources
                    .get(path_of(&symbol.definition))
                    .map(String::as_str)
                    .unwrap_or_default(),
                symbol.definition.line_start,
                symbol.definition.line_end,
            );
            if want(request.scope, PatternScope::Body) {
                for (offset, line) in body.lines().enumerate() {
                    if regex.is_match(line) {
                        items.push(PatternMatch {
                            symbol_id: symbol.id.clone(),
                            name: symbol.name.clone(),
                            matched_field: "body".to_string(),
                            line: symbol.definition.line_start + offset as u32,
                            snippet: line.trim().chars().take(120).collect(),
                        });
                        break;
                    }
                }
            }
            if want(request.scope, PatternScope::Doc) {
                for (offset, line) in body.lines().enumerate() {
                    let trimmed = line.trim_start();
                    let is_doc = trimmed.starts_with("///")
                        || trimmed.starts_with("//!")
                        || trimmed.starts_with("#")
                        || trimmed.starts_with("*")
                        || trimmed.starts_with("<!--");
                    if is_doc && regex.is_match(trimmed) {
                        items.push(PatternMatch {
                            symbol_id: symbol.id.clone(),
                            name: symbol.name.clone(),
                            matched_field: "doc".to_string(),
                            line: symbol.definition.line_start + offset as u32,
                            snippet: trimmed.chars().take(120).collect(),
                        });
                        break;
                    }
                }
            }
        }
        Page::of(items, &request.page)
    }

    /// `code.search_by_error`: keyword scan of function bodies for error
    /// raising / catching / handling sites (documented heuristic, ADR-058).
    pub fn search_by_error(
        &self,
        ctx: &RequestContext,
        request: &ErrorSearchQuery,
    ) -> CodeResult<Page<ErrorMatch>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut items: Vec<ErrorMatch> = Vec::new();
        for symbol in index
            .symbols
            .values()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        {
            let body = slice_lines(
                index
                    .sources
                    .get(path_of(&symbol.definition))
                    .map(String::as_str)
                    .unwrap_or_default(),
                symbol.definition.line_start,
                symbol.definition.line_end,
            );
            for (offset, line) in body.lines().enumerate() {
                let lower = line.to_ascii_lowercase();
                let line_number = symbol.definition.line_start + offset as u32;
                for (keyword, role) in [
                    ("throw", "throws"),
                    ("raise", "throws"),
                    ("panic", "throws"),
                ] {
                    if let Some(position) = lower.find(keyword) {
                        let rest = line[position + keyword.len()..]
                            .trim_start()
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                            .collect::<String>();
                        if request.error_types.is_empty()
                            || request
                                .error_types
                                .iter()
                                .any(|t| lower.contains(&t.to_ascii_lowercase()))
                        {
                            items.push(ErrorMatch {
                                symbol_id: symbol.id.clone(),
                                role: role.to_string(),
                                matched: if rest.is_empty() {
                                    keyword.to_string()
                                } else {
                                    rest
                                },
                                line: line_number,
                            });
                        }
                        break;
                    }
                }
                for (keyword, role) in [
                    ("catch", "catches"),
                    ("except", "catches"),
                    ("rescue", "catches"),
                ] {
                    if lower.contains(keyword) {
                        if request.error_types.is_empty()
                            || request
                                .error_types
                                .iter()
                                .any(|t| lower.contains(&t.to_ascii_lowercase()))
                        {
                            items.push(ErrorMatch {
                                symbol_id: symbol.id.clone(),
                                role: role.to_string(),
                                matched: keyword.to_string(),
                                line: line_number,
                            });
                        }
                        break;
                    }
                }
                if (lower.contains("recover") || lower.trim_start().starts_with('?'))
                    && (request.error_types.is_empty()
                        || request
                            .error_types
                            .iter()
                            .any(|t| lower.contains(&t.to_ascii_lowercase())))
                {
                    items.push(ErrorMatch {
                        symbol_id: symbol.id.clone(),
                        role: "handles".to_string(),
                        matched: "recover".to_string(),
                        line: line_number,
                    });
                }
            }
        }
        items.sort_by(|left, right| {
            left.symbol_id
                .cmp(&right.symbol_id)
                .then(left.line.cmp(&right.line))
                .then(left.matched.cmp(&right.matched))
        });
        Page::of(items, &request.page)
    }

    /// `code.find_by_signature`: signature-shape search (top-level parameter
    /// count, return type substring, modifier keywords — documented
    /// heuristic, ADR-058).
    pub fn find_by_signature(
        &self,
        ctx: &RequestContext,
        request: &SignatureQuery,
    ) -> CodeResult<Page<Symbol>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut items: Vec<Symbol> = Vec::new();
        for symbol in index.symbols_in_path_order() {
            let Some(signature) = &symbol.signature else {
                continue;
            };
            let (param_count, return_type) = signature_shape(signature);
            if let Some(wanted) = request.param_count {
                if param_count != wanted {
                    continue;
                }
            }
            if let Some(wanted) = &request.return_type {
                if !return_type
                    .to_ascii_lowercase()
                    .contains(&wanted.to_ascii_lowercase())
                {
                    continue;
                }
            }
            let lower = signature.to_ascii_lowercase();
            if request.modifiers.iter().any(|modifier| {
                !lower
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|word| word == modifier.to_ascii_lowercase().as_str())
            }) {
                continue;
            }
            if self.authorizer.check_symbol(ctx, symbol).is_err() {
                continue;
            }
            items.push(symbol.clone());
        }
        Page::of(items, &request.page)
    }

    /// `code.find_entry_points`: `main` functions, tests, and functions with
    /// zero incoming `calls` edges (documented heuristic, ADR-058).
    pub fn find_entry_points(
        &self,
        ctx: &RequestContext,
        request: &EntryPointsQuery,
    ) -> CodeResult<Page<EntryPoint>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut items: Vec<EntryPoint> = Vec::new();
        for symbol in index
            .symbols
            .values()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        {
            let kind = if is_main_function(symbol) {
                "main"
            } else if looks_like_test(symbol) {
                "test"
            } else {
                let has_callers = index
                    .incoming
                    .get(&symbol.id)
                    .map(|positions| {
                        positions
                            .iter()
                            .any(|p| index.edges[*p].kind == CodeEdgeKind::Calls)
                    })
                    .unwrap_or(false);
                if has_callers {
                    continue;
                }
                "root_callable"
            };
            let _ = self.authorizer.check_symbol(ctx, symbol);
            items.push(EntryPoint {
                symbol_id: symbol.id.clone(),
                name: symbol.name.clone(),
                kind: kind.to_string(),
                language: symbol.language.clone(),
            });
        }
        items.sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
        Page::of(items, &request.page)
    }

    /// `code.find_related_tests`: tests that reference, import, or share a
    /// name with the target symbol.
    pub fn find_related_tests(
        &self,
        ctx: &RequestContext,
        request: &RelatedTestsQuery,
    ) -> CodeResult<Page<RelatedTest>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let target = index
            .symbols
            .get(&request.symbol_id)
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("symbol {}", request.symbol_id),
            })?;
        self.authorizer.check_symbol(ctx, target)?;

        let target_name = target.name.to_ascii_lowercase();
        let mut items: Vec<RelatedTest> = Vec::new();
        for test in index.symbols.values().filter(|s| looks_like_test(s)) {
            let mut evidence: Vec<String> = Vec::new();
            for (position, edge) in index.edges.iter().enumerate() {
                if !matches!(
                    edge.kind,
                    CodeEdgeKind::References | CodeEdgeKind::DependsOn
                ) || edge.target.as_deref() != Some(&request.symbol_id)
                {
                    continue;
                }
                let Some(source) = index.symbols.get(&edge.source) else {
                    continue;
                };
                if looks_like_test(source) {
                    evidence.push(format!(
                        "edge-{position}: {} references target",
                        source.name
                    ));
                }
            }
            if test.name.to_ascii_lowercase().contains(&target_name)
                || test
                    .qualified_name
                    .to_ascii_lowercase()
                    .contains(&target_name)
            {
                evidence.push("name match".to_string());
            }
            if path_of(&test.definition) == path_of(&target.definition) {
                evidence.push("same file".to_string());
            }
            if evidence.is_empty() {
                continue;
            }
            let _ = self.authorizer.check_symbol(ctx, test);
            items.push(RelatedTest {
                symbol_id: test.id.clone(),
                name: test.name.clone(),
                evidence,
            });
        }
        items.sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
        Page::of(items, &request.page)
    }

    /// `code.index_files`: partial indexing — extend a base revision with a
    /// small set of files, content-addressed into a new dirty revision
    /// (synchronous; the in-memory adapter makes no durability claim).
    pub fn index_files(
        &self,
        ctx: &RequestContext,
        request: &IndexFiles,
    ) -> CodeResult<IndexedRevision> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        if request.files.is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "files",
                reason: "must name at least one file".into(),
            });
        }
        if request.parser_profile.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "parser_profile",
                reason: "must be non-empty".into(),
            });
        }
        for file in &request.files {
            validate_file_path(&file.path)?;
        }
        let base = {
            let state = self.lock();
            self.index_for(ctx, &state, &request.repository_id, &request.base_revision)?
                .clone()
        };

        // Parse every file first: a failure must not leave a half-built index.
        let mut parsed_files: Vec<(IndexFile, ParsedSource)> = Vec::new();
        for file in &request.files {
            let snapshot_file = SourceFile::new(&file.path, &file.content);
            let language =
                snapshot_file
                    .language()
                    .ok_or_else(|| CodeError::CapabilityUnavailable {
                        language: format!(
                            "unknown (no recognized source file {path})",
                            path = file.path
                        ),
                    })?;
            if self.parser.support(&language) != SupportLevel::Ported {
                return Err(CodeError::CapabilityUnavailable { language });
            }
            let parsed = self.parser.parse(&file.path, &language, &file.content)?;
            parsed_files.push((file.clone(), parsed));
        }

        // Content-addressed id: sha256(base id || path/content pairs), hex40.
        let mut hasher = Sha256::new();
        hasher.update(base.commit_sha.as_bytes());
        for (file, _) in &parsed_files {
            hasher.update(file.path.as_bytes());
            hasher.update([0]);
            hasher.update(file.content.as_bytes());
            hasher.update([0]);
        }
        let new_id: String = hasher
            .finalize()
            .iter()
            .take(20)
            .map(|byte| format!("{byte:02x}"))
            .collect();

        let mut index = base;
        index.commit_sha = new_id.clone();

        // Replace the touched files: drop their old symbols and edges.
        for (file, _) in &parsed_files {
            index
                .symbols
                .retain(|_, symbol| path_of(&symbol.definition) != file.path);
            index.edges.retain(|edge| {
                let source_path = index
                    .symbols
                    .get(&edge.source)
                    .map(|s| path_of(&s.definition));
                let target_path = edge
                    .target
                    .as_ref()
                    .and_then(|id| index.symbols.get(id).map(|s| path_of(&s.definition)));
                source_path != Some(file.path.as_str()) && target_path != Some(file.path.as_str())
            });
        }

        let repo_revision = RepoRevision {
            repository_id: request.repository_id.clone(),
            vcs: "git".to_string(),
            commit_sha: new_id.clone(),
            tree_sha: None,
            dirty_policy: DirtyPolicy::Reject,
        };
        for (file, parsed) in &parsed_files {
            index_parsed(&repo_revision, parsed, &mut index.symbols, &mut index.edges);
            index
                .sources
                .insert(file.path.clone(), file.content.clone());
        }
        index.manifest = CodeAnalysisManifest {
            repository_id: request.repository_id.clone(),
            commit_sha: new_id.clone(),
            parser_profile: request.parser_profile.clone(),
            indexed_files: index.sources.len() as u64,
            skipped_files: 0,
            diagnostics: Vec::new(),
            graph_generation: String::new(),
        };
        index.rebuild_adjacency();

        let mut state = self.lock();
        let key = RevisionKey::new(&ctx.scope, &request.repository_id, &new_id);
        let generation = key.generation(state.next_sequence());
        index.generation = generation.clone();
        index.manifest.graph_generation = generation.clone();
        state.revisions.insert(key.label(), index.clone());
        state.latest.insert(
            repository_latest_key(&ctx.scope, &request.repository_id),
            new_id.clone(),
        );
        state.generation.publish(&generation);
        Ok(IndexedRevision {
            revision: new_id,
            base_revision: request.base_revision.clone(),
            indexed_files: request.files.len() as u32,
            total_files: index.sources.len() as u32,
            dirty: true,
        })
    }

    /// `code.pr_context`: one-call review — diff + blast radius + test gaps
    /// + complexity, in markdown or structured form.
    pub fn pr_context(
        &self,
        ctx: &RequestContext,
        request: &PrContextQuery,
    ) -> CodeResult<PrContextReport> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        check_bounds(request.max_depth, request.max_nodes)?;
        let state = self.lock();
        let from = self.index_for(ctx, &state, &request.repository_id, &request.base_revision)?;
        let to = self.index_for(ctx, &state, &request.repository_id, &request.head_revision)?;

        let (changed, signature_changes, body_changes) = symbol_diff_ids(from, to);

        // Blast radius: BFS over incoming edges per changed symbol.
        let mut impacted: Vec<(u32, String)> = Vec::new();
        for symbol_id in &changed {
            if !to.symbols.contains_key(symbol_id) {
                continue;
            }
            let mut depth = 1u32;
            let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut frontier: Vec<String> = Vec::new();
            if let Some(positions) = to.incoming.get(symbol_id) {
                for position in positions {
                    if matches!(
                        to.edges[*position].kind,
                        CodeEdgeKind::Calls
                            | CodeEdgeKind::References
                            | CodeEdgeKind::DependsOn
                            | CodeEdgeKind::Implements
                    ) {
                        frontier.push(to.edges[*position].source.clone());
                    }
                }
            }
            while depth <= request.max_depth && !frontier.is_empty() {
                let mut next: Vec<String> = Vec::new();
                for current in &frontier {
                    if seen.insert(current.clone()) && impacted.len() < request.max_nodes as usize {
                        impacted.push((depth, current.clone()));
                    }
                    if let Some(positions) = to.incoming.get(current) {
                        for position in positions {
                            if matches!(
                                to.edges[*position].kind,
                                CodeEdgeKind::Calls
                                    | CodeEdgeKind::References
                                    | CodeEdgeKind::DependsOn
                                    | CodeEdgeKind::Implements
                            ) {
                                next.push(to.edges[*position].source.clone());
                            }
                        }
                    }
                }
                frontier = next;
                depth += 1;
            }
        }
        impacted.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        let impacted_ids: Vec<String> = impacted.iter().map(|(_, id)| id.clone()).collect();

        // Test gaps: changed symbols without a related test candidate.
        let mut test_gaps: Vec<String> = Vec::new();
        for symbol_id in &changed {
            let target = to.symbols.get(symbol_id).cloned();
            let Some(target) = target else {
                test_gaps.push(symbol_id.clone());
                continue;
            };
            let target_name = target.name.to_ascii_lowercase();
            let mut covered = false;
            for test in to.symbols.values().filter(|s| looks_like_test(s)) {
                if test.name.to_ascii_lowercase().contains(&target_name)
                    || test
                        .qualified_name
                        .to_ascii_lowercase()
                        .contains(&target_name)
                    || path_of(&test.definition) == path_of(&target.definition)
                {
                    covered = true;
                    break;
                }
            }
            if !covered {
                test_gaps.push(symbol_id.clone());
            }
        }

        // Complexity of the changed functions (in the head revision).
        let mut complexity: Vec<ComplexityReport> = Vec::new();
        for symbol_id in &changed {
            if let Some(symbol) = to.symbols.get(symbol_id) {
                if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
                    let body = slice_lines(
                        to.sources
                            .get(path_of(&symbol.definition))
                            .map(String::as_str)
                            .unwrap_or_default(),
                        symbol.definition.line_start,
                        symbol.definition.line_end,
                    );
                    complexity.push(complexity_report(symbol, &body));
                }
            }
        }

        let summary = if request.format == PrContextFormat::Markdown {
            render_pr_markdown(
                &request.base_revision,
                &request.head_revision,
                &changed,
                &impacted_ids,
                &test_gaps,
                &signature_changes,
            )
        } else {
            String::new()
        };

        Ok(PrContextReport {
            summary,
            changed: changed.clone(),
            impacted: impacted_ids,
            test_gaps,
            complexity,
            signature_changes,
            body_changes,
        })
    }

    // -- Phase 8 gap-closure operations (additive only) --------------------

    /// `code.find_implementors` (CAPI-14): the symbols that implement
    /// `target` — `implements` edges, plus class/interface `references`
    /// (the extractor's fallback for languages without a dedicated
    /// implements construct).
    pub fn find_implementors(
        &self,
        ctx: &RequestContext,
        request: &FindImplementorsQuery,
    ) -> CodeResult<Page<ImplementorMatch>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;
        let target = index
            .symbols
            .get(&request.target_symbol_id)
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("symbol {}", request.target_symbol_id),
            })?;
        self.authorizer.check_symbol(ctx, target)?;

        let mut matches: BTreeMap<String, (Symbol, Vec<String>)> = BTreeMap::new();
        for edge in &index.edges {
            let Some(edge_target) = &edge.target else {
                continue;
            };
            if edge_target != &request.target_symbol_id {
                continue;
            }
            let is_implements = edge.kind == CodeEdgeKind::Implements;
            let is_reference = edge.kind == CodeEdgeKind::References;
            if !is_implements && !is_reference {
                continue;
            }
            let Some(symbol) = index.symbols.get(&edge.source) else {
                continue;
            };
            // The reference fallback only counts class-like symbols.
            if is_reference && !matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface) {
                continue;
            }
            if self.authorizer.check_symbol(ctx, symbol).is_err() {
                continue;
            }
            let entry = matches
                .entry(symbol.id.clone())
                .or_insert((symbol.clone(), Vec::new()));
            for evidence in &edge.evidence_ids {
                if !entry.1.contains(evidence) {
                    entry.1.push(evidence.clone());
                }
            }
        }
        let items: Vec<ImplementorMatch> = matches
            .into_iter()
            .map(|(_, (symbol, evidence))| ImplementorMatch {
                symbol_id: symbol.id,
                name: symbol.name,
                language: symbol.language,
                evidence,
            })
            .collect();
        Page::of(items, &request.page)
    }

    /// `code.find_importers` (CAPI-33): reverse import lookup by module
    /// name. An unknown module name is a valid empty page, not an error
    /// (upstream `find_by_imports` parity).
    pub fn find_importers(
        &self,
        ctx: &RequestContext,
        request: &FindImportersQuery,
    ) -> CodeResult<Page<ImporterMatch>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        if request.module_name.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "module_name",
                reason: "must be non-empty".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;
        let needle = request.module_name.trim();
        let mut seen: BTreeSet<(String, String, u32)> = BTreeSet::new();
        let mut items: Vec<ImporterMatch> = Vec::new();
        for edge in &index.edges {
            if edge.kind != CodeEdgeKind::Imports {
                continue;
            }
            let Some(imported_module) = &edge.import_path else {
                continue;
            };
            let matched = match request.match_mode {
                ImportMatchMode::Exact => imported_module == needle,
                ImportMatchMode::Prefix => imported_module.starts_with(needle),
                ImportMatchMode::Contains => imported_module.contains(needle),
                ImportMatchMode::Fuzzy => imported_module
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase()),
            };
            if !matched {
                continue;
            }
            let Some(source_symbol) = index.symbols.get(&edge.source) else {
                continue;
            };
            let importer_file = path_of(&source_symbol.definition).to_string();
            if self.authorizer.check_path(ctx, &importer_file).is_err() {
                continue;
            }
            let line = edge
                .evidence_ids
                .first()
                .and_then(|evidence| evidence.rsplit(':').next())
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            if !seen.insert((importer_file.clone(), imported_module.clone(), line)) {
                continue;
            }
            items.push(ImporterMatch {
                importer_file,
                imported_module: imported_module.clone(),
                line,
                evidence: edge.evidence_ids.clone(),
            });
        }
        items.sort_by(|left, right| {
            left.importer_file
                .cmp(&right.importer_file)
                .then_with(|| left.imported_module.cmp(&right.imported_module))
                .then_with(|| left.line.cmp(&right.line))
        });
        Page::of(items, &request.page)
    }

    /// `code.traverse` (CAPI-15): bounded free traversal. Unlike the
    /// caller/callee/dependency operations, hitting `max_nodes` records
    /// `truncated` instead of failing.
    pub fn traverse_graph(
        &self,
        ctx: &RequestContext,
        request: &TraverseQuery,
    ) -> CodeResult<TraverseResult> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        check_bounds(request.max_depth, request.max_nodes)?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;
        let start =
            index
                .symbols
                .get(&request.start_symbol_id)
                .ok_or_else(|| CodeError::NotFound {
                    resource: format!("symbol {}", request.start_symbol_id),
                })?;
        self.authorizer.check_symbol(ctx, start)?;

        let mut visited: Vec<String> = vec![request.start_symbol_id.clone()];
        let mut queue: Vec<(String, u32)> = vec![(request.start_symbol_id.clone(), 0)];
        let mut collected: Vec<CodeEdge> = Vec::new();
        let mut seen_edges: BTreeMap<(String, String, CodeEdgeKind), ()> = BTreeMap::new();
        let mut truncated = false;

        while let Some((node, depth)) = queue.pop() {
            if depth >= request.max_depth {
                continue;
            }
            let positions: &[usize] = match request.direction {
                Direction::In => index
                    .incoming
                    .get(&node)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                Direction::Out => index
                    .outgoing
                    .get(&node)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            };
            for position in positions {
                let edge = index.edge_at(*position);
                // `defines` is structural, never followed.
                if edge.kind == CodeEdgeKind::Defines {
                    continue;
                }
                if !request.edge_kinds.is_empty() && !request.edge_kinds.contains(&edge.kind) {
                    continue;
                }
                let neighbour = match request.direction {
                    Direction::In => edge.source.clone(),
                    Direction::Out => match &edge.target {
                        Some(target) => target.clone(),
                        None => continue,
                    },
                };
                let Some(symbol) = index.symbols.get(&neighbour) else {
                    continue;
                };
                if self.authorizer.check_symbol(ctx, symbol).is_err() {
                    continue;
                }
                let fingerprint = (
                    edge.source.clone(),
                    edge.target.clone().unwrap_or_default(),
                    edge.kind,
                );
                if seen_edges.insert(fingerprint, ()).is_none() {
                    collected.push(edge.clone());
                }
                if !visited.iter().any(|seen| seen == &neighbour) {
                    if visited.len() as u32 >= request.max_nodes {
                        truncated = true;
                        continue;
                    }
                    visited.push(neighbour.clone());
                    queue.push((neighbour, depth + 1));
                }
            }
        }
        collected.sort_by(|left, right| {
            (left.kind as u8, &left.source, &left.target).cmp(&(
                right.kind as u8,
                &right.source,
                &right.target,
            ))
        });
        let edges = Page::of(collected, &request.page)?;
        Ok(TraverseResult {
            edges,
            visited_count: visited.len() as u32,
            truncated,
        })
    }

    /// `code.get_detailed_symbol` (CAPI-16): one symbol with its complexity
    /// (function/method kinds) and its direct call relationships.
    pub fn get_detailed_symbol(
        &self,
        ctx: &RequestContext,
        request: &GetDetailedSymbolQuery,
    ) -> CodeResult<DetailedSymbol> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;
        let symbol = index
            .symbols
            .get(&request.symbol_id)
            .cloned()
            .ok_or_else(|| CodeError::NotFound {
                resource: format!("symbol {}", request.symbol_id),
            })?;
        self.authorizer.check_symbol(ctx, &symbol)?;

        let complexity = if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            let body = slice_lines(
                index
                    .sources
                    .get(path_of(&symbol.definition))
                    .map(String::as_str)
                    .unwrap_or_default(),
                symbol.definition.line_start,
                symbol.definition.line_end,
            );
            Some(complexity_report(&symbol, &body))
        } else {
            None
        };

        let mut callers: Vec<SymbolRef> = Vec::new();
        if let Some(positions) = index.incoming.get(&symbol.id) {
            for position in positions {
                let edge = &index.edges[*position];
                if edge.kind != CodeEdgeKind::Calls {
                    continue;
                }
                if let Some(referenced) = index.symbols.get(&edge.source) {
                    if self.authorizer.check_symbol(ctx, referenced).is_ok() {
                        callers.push(SymbolRef {
                            id: referenced.id.clone(),
                            name: referenced.name.clone(),
                        });
                    }
                }
            }
        }
        let mut callees: Vec<SymbolRef> = Vec::new();
        if let Some(positions) = index.outgoing.get(&symbol.id) {
            for position in positions {
                let edge = &index.edges[*position];
                if edge.kind != CodeEdgeKind::Calls {
                    continue;
                }
                if let Some(target_id) = &edge.target {
                    if let Some(referenced) = index.symbols.get(target_id) {
                        if self.authorizer.check_symbol(ctx, referenced).is_ok() {
                            callees.push(SymbolRef {
                                id: referenced.id.clone(),
                                name: referenced.name.clone(),
                            });
                        }
                    }
                }
            }
        }
        callers.sort_by(|left, right| left.id.cmp(&right.id));
        callees.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(DetailedSymbol {
            caller_count: callers.len() as u32,
            callee_count: callees.len() as u32,
            symbol,
            complexity,
            callers,
            callees,
        })
    }

    /// `code.index_directory` (CAPI-17): index every file of the base
    /// revision whose path starts with `path_prefix`. The actual indexing
    /// reuses the `index_files` pipeline verbatim.
    pub fn index_directory(
        &self,
        ctx: &RequestContext,
        request: &IndexDirectory,
    ) -> CodeResult<IndexedRevision> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        if request.path_prefix.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "path_prefix",
                reason: "must be non-empty".into(),
            });
        }
        if request.parser_profile.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "parser_profile",
                reason: "must be non-empty".into(),
            });
        }
        let base = {
            let state = self.lock();
            self.index_for(ctx, &state, &request.repository_id, &request.base_revision)?
                .clone()
        };
        let selected: Vec<IndexFile> = base
            .sources
            .iter()
            .filter(|(path, _)| path.starts_with(&request.path_prefix))
            .map(|(path, content)| IndexFile {
                path: path.clone(),
                content: content.clone(),
            })
            .collect();
        if selected.is_empty() {
            return Err(CodeError::NotFound {
                resource: format!(
                    "files under `{}` in revision {}",
                    request.path_prefix, request.base_revision
                ),
            });
        }
        self.index_files(
            ctx,
            &IndexFiles {
                repository_id: request.repository_id.clone(),
                base_revision: request.base_revision.clone(),
                files: selected,
                parser_profile: request.parser_profile.clone(),
            },
        )
    }

    /// `code.export_graph` (CAPI-18): render the code graph of a revision
    /// to dot / json / csv / triples via [`graph_engine::export`].
    pub fn export_graph(
        &self,
        ctx: &RequestContext,
        request: &ExportGraphQuery,
    ) -> CodeResult<GraphExport> {
        use graph_engine::export::{export_graph as render, ExportFormat};
        use graph_engine::ids::IdKind;
        use graph_engine::types::{GraphEdge, GraphNode, PropertyMap, PropertyValue};

        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let format = ExportFormat::from_wire_name(&request.format).ok_or_else(|| {
            CodeError::InvalidArgument {
                field: "format",
                reason: format!(
                    "`{}` is not a supported format (dot, json, csv, triples)",
                    request.format
                ),
            }
        })?;
        if request.max_nodes > 10_000 {
            return Err(CodeError::ResourceLimit {
                detail: "max_nodes must be <= 10000".into(),
            });
        }
        let state = self.lock();
        let index = self.index_for(ctx, &state, &request.repository_id, &request.revision)?;

        let mut node_ids: Vec<String> = index.symbols.keys().cloned().collect();
        node_ids.sort();
        let truncated = request.max_nodes > 0 && node_ids.len() > request.max_nodes as usize;
        if truncated {
            node_ids.truncate(request.max_nodes as usize);
        }
        let kept: std::collections::BTreeSet<String> = node_ids.iter().cloned().collect();

        let mut code_edges: Vec<CodeEdge> = Vec::new();
        for edge in &index.edges {
            if !request.edge_kinds.is_empty() && !request.edge_kinds.contains(&edge.kind) {
                continue;
            }
            let Some(target) = &edge.target else {
                continue;
            };
            if !kept.contains(&edge.source) || !kept.contains(target) {
                continue;
            }
            code_edges.push(edge.clone());
        }
        code_edges.sort_by(|left, right| {
            (
                left.kind as u8,
                &left.source,
                left.target.clone().unwrap_or_default(),
            )
                .cmp(&(
                    right.kind as u8,
                    &right.source,
                    right.target.clone().unwrap_or_default(),
                ))
        });

        let graph_nodes: Vec<GraphNode> = node_ids
            .iter()
            .map(|id| {
                let symbol = index.symbols.get(id).expect("node id from the index");
                let mut properties = PropertyMap::new();
                properties.insert("name", PropertyValue::String(symbol.name.clone()));
                properties.insert(
                    "qualified_name",
                    PropertyValue::String(symbol.qualified_name.clone()),
                );
                properties.insert("language", PropertyValue::String(symbol.language.clone()));
                properties.insert(
                    "path",
                    PropertyValue::String(path_of(&symbol.definition).to_string()),
                );
                GraphNode::new(
                    canonical_id(IdKind::Node, id),
                    symbol.kind.as_graph_node_type(),
                    properties,
                )
            })
            .collect();
        let graph_edges: Vec<GraphEdge> = code_edges
            .iter()
            .map(|edge| {
                let target = edge.target.clone().unwrap_or_default();
                let mut properties = PropertyMap::new();
                if let Some(confidence) = edge.confidence {
                    properties.insert("confidence", PropertyValue::Float(confidence));
                }
                GraphEdge::new(
                    canonical_id(IdKind::Edge, &format!("{}|{}", edge.source, target)),
                    canonical_id(IdKind::Node, &edge.source),
                    canonical_id(IdKind::Node, &target),
                    edge.kind.as_graph_edge_type(),
                    properties,
                )
            })
            .collect();

        Ok(GraphExport {
            format: format.wire_name().to_string(),
            content: render(&graph_nodes, &graph_edges, format),
            node_count: node_ids.len() as u32,
            edge_count: code_edges.len() as u32,
            truncated,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lookup_index<'a>(
    ctx: &RequestContext,
    state: &'a ServiceState,
    repository_id: &str,
    revision: &str,
) -> CodeResult<&'a RevisionIndex> {
    let key = RevisionKey::new(&ctx.scope, repository_id, revision).label();
    let index = state
        .revisions
        .get(&key)
        .ok_or_else(|| CodeError::NotFound {
            resource: format!("revision {revision} of repository {repository_id}"),
        })?;
    require_same_scope(ctx, &index.scope)?;
    Ok(index)
}

fn check_fresh(ctx: &RequestContext, index: &RevisionIndex) -> CodeResult<()> {
    if index.stale {
        return Err(CodeError::IndexStale {
            revision: index.commit_sha.clone(),
        });
    }
    if let Some(required) = &ctx.required_generation {
        if required != &index.generation {
            return Err(CodeError::IndexStale {
                revision: required.clone(),
            });
        }
    }
    Ok(())
}

fn require_same_scope(ctx: &RequestContext, scope: &ScopeRef) -> CodeResult<()> {
    let same_tenant = ctx.scope.tenant_id == scope.tenant_id;
    let same_project = ctx.scope.project_id == scope.project_id;
    if same_tenant && same_project {
        Ok(())
    } else {
        Err(CodeError::CrossScopeDenied {
            requested: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
            actual: format!("{}/{}", scope.tenant_id, scope.project_id),
        })
    }
}

fn map_job_error(error: JobError) -> CodeError {
    match error {
        JobError::JobNotFound { job_id } => CodeError::JobNotFound { job_id },
        JobError::JobNotCancellable { job_id, .. } => CodeError::JobNotCancellable { job_id },
        JobError::StaleGeneration { requested } => CodeError::IndexStale {
            revision: requested,
        },
        JobError::InvalidTransition { .. } => CodeError::InvalidArgument {
            field: "job.stage",
            reason: error.to_string(),
        },
    }
}

fn advance_to(state: &mut ServiceState, job_id: &str, target: Stage) -> CodeResult<()> {
    loop {
        let stage = state.jobs.get(job_id).map_err(map_job_error)?.stage;
        if stage == target {
            return Ok(());
        }
        state.jobs.advance(job_id).map_err(map_job_error)?;
    }
}

fn check_bounds(max_depth: u32, max_nodes: u32) -> CodeResult<()> {
    if max_depth == 0 || max_depth > MAX_TRAVERSAL_DEPTH {
        return Err(CodeError::ResourceLimit {
            detail: format!("max_depth must be in 1..={MAX_TRAVERSAL_DEPTH}"),
        });
    }
    if max_nodes == 0 || max_nodes > MAX_TRAVERSAL_NODES {
        return Err(CodeError::ResourceLimit {
            detail: format!("max_nodes must be in 1..={MAX_TRAVERSAL_NODES}"),
        });
    }
    Ok(())
}

/// Deterministically map an arbitrary stable string (a symbol id, or
/// `source|target` for an edge) to a graph-engine [`CanonicalId`]. The
/// mapping is a SHA-256 prefix, so the same id always renders the same
/// canonical id — the export stays stable across calls (PROP-03).
fn canonical_id(kind: graph_engine::ids::IdKind, key: &str) -> graph_engine::ids::CanonicalId {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    graph_engine::ids::CanonicalId::from_parts(kind, graph_engine::ids::Ulid(bytes))
}

fn options_fingerprint(include_globs: &[String], exclude_globs: &[String]) -> String {
    let mut hasher = Sha256::new();
    for glob in include_globs {
        hasher.update(b"i:");
        hasher.update(glob.as_bytes());
    }
    for glob in exclude_globs {
        hasher.update(b"e:");
        hasher.update(glob.as_bytes());
    }
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn repository_latest_key(scope: &ScopeRef, repository_id: &str) -> String {
    format!("{}/{}/{}", scope.tenant_id, scope.project_id, repository_id)
}

fn discover<'a>(
    snapshot: &'a SourceSnapshot,
    include_globs: &[String],
    exclude_globs: &[String],
) -> Vec<&'a SourceFile> {
    let mut files: Vec<&SourceFile> = snapshot
        .files
        .iter()
        .filter(|file| {
            let included = include_globs.is_empty()
                || include_globs
                    .iter()
                    .any(|glob| glob_match(glob, &file.path));
            let excluded = exclude_globs
                .iter()
                .any(|glob| glob_match(glob, &file.path));
            included && !excluded
        })
        .collect();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

/// Minimal glob matcher supporting `**`, `*` and `?` (no character classes).
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    fn inner(pattern: &[u8], path: &[u8]) -> bool {
        if pattern.is_empty() {
            return path.is_empty();
        }
        match pattern[0] {
            b'*' => {
                if pattern.get(1) == Some(&b'*') {
                    let rest = &pattern[2..];
                    let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                    if rest == path {
                        return true;
                    }
                    for index in 0..=path.len() {
                        if inner(rest, &path[index..]) {
                            return true;
                        }
                    }
                    false
                } else {
                    for index in 0..=path.len() {
                        if inner(&pattern[1..], &path[index..]) {
                            return true;
                        }
                    }
                    false
                }
            }
            b'?' => !path.is_empty() && inner(&pattern[1..], &path[1..]),
            byte => !path.is_empty() && path[0] == byte && inner(&pattern[1..], &path[1..]),
        }
    }
    inner(pattern.as_bytes(), path.as_bytes())
}

fn slice_lines(content: &str, line_start: u32, line_end: u32) -> String {
    if content.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    let start = line_start.saturating_sub(1) as usize;
    let end = (line_end as usize).min(lines.len());
    if start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    chars.div_ceil(CHARS_PER_TOKEN)
}

fn looks_like_test(symbol: &Symbol) -> bool {
    let path = path_of(&symbol.definition).to_ascii_lowercase();
    let name = symbol.name.to_ascii_lowercase();
    path.contains("/tests/")
        || path.starts_with("tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_test.go")
        || path.ends_with("_test.py")
        || path.contains("/test_")
        || name.starts_with("test_")
        || name.starts_with("test")
        || symbol.qualified_name.contains("::tests::")
}

/// The directory (module) a file path belongs to; root files map to `""`.
fn module_directory(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(prefix, _)| prefix)
        .filter(|prefix| !prefix.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `main` entry point (documented heuristic, ADR-058).
fn is_main_function(symbol: &Symbol) -> bool {
    let name = symbol.name.to_ascii_lowercase();
    let qualified = symbol.qualified_name.to_ascii_lowercase();
    name == "main" || qualified.ends_with(".main") || qualified.ends_with("::main")
}

/// Max brace/paren depth of a source slice (nesting heuristic).
fn max_brace_depth(text: &str) -> usize {
    let mut max = 0usize;
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        max = max.max(depth);
    }
    max
}

/// Keyword-based cyclomatic complexity with breakdown (registered heuristic,
/// ADR-058).
fn complexity_report(symbol: &Symbol, body: &str) -> ComplexityReport {
    let lower = body.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let count =
        |word: &str| -> u32 { words.iter().filter(|candidate| **candidate == word).count() as u32 };
    let branches = count("if")
        + count("elif")
        + count("else")
        + count("match")
        + count("switch")
        + count("case")
        + count("when");
    let loops = count("for") + count("while") + count("do") + count("loop") + count("foreach");
    let exceptions = count("try")
        + count("catch")
        + count("except")
        + count("panic")
        + count("raise")
        + count("throw")
        + count("recover");
    let early_returns = count("return");
    let nesting = max_brace_depth(body) as u32;
    let cyclomatic = 1u32.saturating_add(branches).saturating_add(loops);
    let verdict = if cyclomatic <= 10 {
        "low"
    } else if cyclomatic <= 20 {
        "moderate"
    } else {
        "high"
    };
    ComplexityReport {
        symbol_id: symbol.id.clone(),
        name: symbol.name.clone(),
        cyclomatic,
        branches,
        loops,
        nesting,
        exceptions,
        early_returns,
        verdict: verdict.to_string(),
    }
}

/// Up to `max` names of the incoming edges of `kind`.
fn incoming_names(
    index: &RevisionIndex,
    symbol_id: &str,
    kind: CodeEdgeKind,
    max: usize,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(positions) = index.incoming.get(symbol_id) {
        for position in positions {
            if index.edges[*position].kind != kind {
                continue;
            }
            if let Some(source) = index.symbols.get(&index.edges[*position].source) {
                names.push(source.name.clone());
                if names.len() >= max {
                    break;
                }
            }
        }
    }
    names
}

/// Join with ", " or a fallback for the empty list.
fn join_or(fallback: &str, items: &[String]) -> String {
    if items.is_empty() {
        fallback.to_string()
    } else {
        items.join(", ")
    }
}

/// Error-handling hints of a function body (documented heuristic).
fn error_hints_of(body: &str) -> Vec<String> {
    let mut hints: Vec<String> = Vec::new();
    for line in body.lines() {
        let lower = line.to_ascii_lowercase();
        for keyword in ["panic", "throw", "raise", "except", "catch"] {
            if let Some(position) = lower.find(keyword) {
                let rest = line[position + keyword.len()..].trim_start();
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                hints.push(if name.is_empty() {
                    format!("{keyword} site")
                } else {
                    format!("{keyword} {name}")
                });
                break;
            }
        }
    }
    hints.dedup();
    hints
}

/// Up to `max` names of tests related to a symbol (name/file heuristic).
fn related_test_names(index: &RevisionIndex, symbol_id: &str, max: usize) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(target) = index.symbols.get(symbol_id) {
        let target_name = target.name.to_ascii_lowercase();
        for test in index.symbols.values().filter(|s| looks_like_test(s)) {
            if test.name.to_ascii_lowercase().contains(&target_name)
                || test
                    .qualified_name
                    .to_ascii_lowercase()
                    .contains(&target_name)
                || path_of(&test.definition) == path_of(&target.definition)
            {
                names.push(test.name.clone());
                if names.len() >= max {
                    break;
                }
            }
        }
    }
    names
}

/// `(top-level parameter count, return type text)` of a declaration
/// signature (documented heuristic, ADR-058).
fn signature_shape(signature: &str) -> (u32, String) {
    let open = signature.find('(');
    let close = signature.rfind(')');
    match (open, close) {
        (Some(open), Some(close)) if close > open => {
            let inner = &signature[open + 1..close];
            let mut params = 0u32;
            let mut depth = 0usize;
            let mut started = false;
            for ch in inner.chars() {
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => {
                        params += 1;
                        started = false;
                    }
                    c if c.is_alphanumeric() => started = true,
                    _ => {}
                }
            }
            if started {
                params += 1;
            }
            let rest = signature[close + 1..].trim();
            let return_text = rest.strip_prefix("->").map(str::trim).unwrap_or(rest);
            (params, return_text.to_string())
        }
        _ => (0, String::new()),
    }
}

/// Symbol-id diff of two revision indexes: `(changed, signature_changes,
/// body_changes)`, all sorted and deduplicated.
fn symbol_diff_ids(
    from: &RevisionIndex,
    to: &RevisionIndex,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let key_of = |symbol: &Symbol| -> (String, String, String) {
        (
            path_of(&symbol.definition).to_string(),
            symbol.qualified_name.clone(),
            format!("{:?}", symbol.kind),
        )
    };
    let from_map: BTreeMap<(String, String, String), &Symbol> =
        from.symbols.values().map(|s| (key_of(s), s)).collect();
    let to_map: BTreeMap<(String, String, String), &Symbol> =
        to.symbols.values().map(|s| (key_of(s), s)).collect();
    let mut changed: Vec<String> = Vec::new();
    let mut signature_changes: Vec<String> = Vec::new();
    let mut body_changes: Vec<String> = Vec::new();
    for (key, old) in &from_map {
        match to_map.get(key) {
            None => changed.push(old.id.clone()),
            Some(new) => {
                if old.signature != new.signature || old.definition != new.definition {
                    changed.push(new.id.clone());
                    if old.signature != new.signature {
                        signature_changes.push(new.id.clone());
                    } else {
                        body_changes.push(new.id.clone());
                    }
                }
            }
        }
    }
    for (key, new) in &to_map {
        if !from_map.contains_key(key) {
            changed.push(new.id.clone());
        }
    }
    changed.sort();
    changed.dedup();
    signature_changes.sort();
    signature_changes.dedup();
    body_changes.sort();
    body_changes.dedup();
    (changed, signature_changes, body_changes)
}

/// Ready-to-post markdown for `pr_context`.
fn render_pr_markdown(
    base: &str,
    head: &str,
    changed: &[String],
    impacted: &[String],
    test_gaps: &[String],
    signature_changes: &[String],
) -> String {
    let mut markdown = String::new();
    markdown.push_str("# PR Context (UKMA)\n\n");
    markdown.push_str(&format!("- Base: `{base}`\n"));
    markdown.push_str(&format!("- Head: `{head}`\n"));
    markdown.push_str(&format!("- Changed symbols: {}\n", changed.len()));
    markdown.push_str(&format!("- Impacted symbols: {}\n\n", impacted.len()));
    if !signature_changes.is_empty() {
        markdown.push_str(&format!(
            "## Signature changes\n\n{}\n\n",
            signature_changes
                .iter()
                .map(|id| format!("- `{id}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    markdown.push_str("## Changed\n\n");
    for id in changed.iter().take(25) {
        markdown.push_str(&format!("- `{id}`\n"));
    }
    if changed.len() > 25 {
        markdown.push_str(&format!("- … and {} more\n", changed.len() - 25));
    }
    markdown.push_str("\n## Impacted (blast radius)\n\n");
    for id in impacted.iter().take(25) {
        markdown.push_str(&format!("- `{id}`\n"));
    }
    if impacted.len() > 25 {
        markdown.push_str(&format!("- … and {} more\n", impacted.len() - 25));
    }
    if test_gaps.is_empty() {
        markdown
            .push_str("\n## Test coverage\n\nAll changed symbols have a related test candidate.\n");
    } else {
        markdown.push_str("\n## Test gaps\n\n");
        for id in test_gaps {
            markdown.push_str(&format!("- `{id}`\n"));
        }
    }
    markdown
}

/// Validate a repo-relative file path (no `..`, no leading `/`).
fn validate_file_path(path: &str) -> CodeResult<()> {
    if path.trim().is_empty() {
        return Err(CodeError::InvalidArgument {
            field: "files.path",
            reason: "must be non-empty".into(),
        });
    }
    if path.starts_with('/') {
        return Err(CodeError::InvalidArgument {
            field: "files.path",
            reason: "must be repo-relative".into(),
        });
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err(CodeError::InvalidArgument {
            field: "files.path",
            reason: "must not contain `..` segments".into(),
        });
    }
    Ok(())
}

/// DFS cycle detection: every back edge found on the search stack is
/// recorded as a canonicalised cycle (lexicographically smallest first).
fn visit_cycles(
    root: &str,
    graph: &BTreeMap<String, Vec<String>>,
    stack: &mut Vec<String>,
    visited: &mut std::collections::BTreeSet<String>,
    found: &mut Vec<Vec<String>>,
) {
    let mut on_stack: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    visited.insert(root.to_string());
    on_stack.insert(root.to_string());
    stack.push(root.to_string());
    loop {
        let node = stack.last().expect("stack is non-empty").clone();
        let neighbours = graph.get(&node).cloned().unwrap_or_default();
        let mut descended = false;
        for neighbour in neighbours {
            if on_stack.contains(&neighbour) {
                let position = stack
                    .iter()
                    .position(|item| item == &neighbour)
                    .expect("back-edge target is on the stack");
                let cycle: Vec<String> = stack[position..].to_vec();
                let smallest = cycle.iter().min().expect("cycle is non-empty").clone();
                let offset = cycle
                    .iter()
                    .position(|item| item == &smallest)
                    .expect("smallest is in the cycle");
                let mut rotated: Vec<String> = cycle[offset..].to_vec();
                rotated.extend(cycle[..offset].iter().cloned());
                if !found.contains(&rotated) {
                    found.push(rotated);
                }
                continue;
            }
            if !visited.contains(&neighbour) {
                visited.insert(neighbour.clone());
                on_stack.insert(neighbour.clone());
                stack.push(neighbour.clone());
                descended = true;
                break;
            }
        }
        if !descended {
            on_stack.remove(&node);
            stack.pop();
            if stack.is_empty() {
                return;
            }
        }
    }
}

/// §32.3 version-scoped symbol identity.
fn version_scoped_id(
    revision: &RepoRevision,
    location: &CodeLocation,
    qualified_name: &str,
    kind: SymbolKind,
    signature: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        revision.repository_id.as_str(),
        revision.commit_sha.as_str(),
        path_of(location),
        qualified_name,
        &format!("{kind:?}"),
        signature.unwrap_or(""),
        &location.line_start.to_string(),
        &location.line_end.to_string(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    let digest = hasher.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sym:v1:{hex}")
}

/// Fold one parsed file into the revision index, remapping parser-local ids to
/// §32.3 version-scoped ids.
fn index_parsed(
    revision: &RepoRevision,
    parsed: &ParsedSource,
    symbols: &mut BTreeMap<String, Symbol>,
    edges: &mut Vec<CodeEdge>,
) -> BTreeMap<String, String> {
    let mut remap: BTreeMap<String, String> = BTreeMap::new();
    for symbol in &parsed.symbols {
        let id = version_scoped_id(
            revision,
            &symbol.definition,
            &symbol.qualified_name,
            symbol.kind,
            symbol.signature.as_deref(),
        );
        remap.insert(symbol.id.clone(), id.clone());
        let mut stored = symbol.clone();
        stored.id = id;
        stored.evidence_ids = symbol
            .evidence_ids
            .iter()
            .map(|evidence| format!("{}|{}", parsed.content_hash, evidence))
            .collect();
        symbols.insert(stored.id.clone(), stored);
    }
    for edge in &parsed.edges {
        let Some(source) = remap.get(&edge.source).cloned() else {
            continue;
        };
        let target = edge
            .target
            .as_ref()
            .and_then(|target| remap.get(target).cloned())
            .filter(|target| target != &source);
        let resolution = if target.is_some() {
            edge.resolution
        } else {
            EdgeResolution::Unresolved
        };
        edges.push(CodeEdge {
            source,
            target,
            kind: edge.kind,
            resolution,
            confidence: edge.confidence,
            import_path: edge.import_path.clone(),
            evidence_ids: edge
                .evidence_ids
                .iter()
                .map(|evidence| format!("{}|{}", parsed.content_hash, evidence))
                .collect(),
        });
    }
    remap
}

// ---------------------------------------------------------------------------
// §37 port surface
// ---------------------------------------------------------------------------

/// The §37 domain port (CAPI-01..13). The async signature is the normative one;
/// this implementation is non-blocking and delegates to the inherent methods so
/// a gateway can await it without a runtime-specific blocking adapter.
#[allow(async_fn_in_trait)]
pub trait CodeIntelligencePort: Send + Sync {
    /// CAPI-01.
    async fn analyze_repository(
        &self,
        ctx: &RequestContext,
        request: &AnalyzeRepository,
    ) -> CodeResult<JobHandle>;
    /// CAPI-02.
    async fn get_analysis_job(&self, ctx: &RequestContext, request: &GetJob)
        -> CodeResult<JobView>;
    /// CAPI-03.
    async fn cancel_analysis_job(
        &self,
        ctx: &RequestContext,
        request: &CancelJob,
    ) -> CodeResult<JobView>;
    /// CAPI-04.
    async fn get_symbol(&self, ctx: &RequestContext, request: &GetSymbol) -> CodeResult<Symbol>;
    /// CAPI-05.
    async fn find_symbols(
        &self,
        ctx: &RequestContext,
        request: &SymbolQuery,
    ) -> CodeResult<Page<Symbol>>;
    /// CAPI-06.
    async fn find_references(
        &self,
        ctx: &RequestContext,
        request: &ReferenceQuery,
    ) -> CodeResult<Page<ReferenceLocation>>;
    /// CAPI-07.
    async fn query_callers(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>>;
    /// CAPI-08.
    async fn query_callees(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>>;
    /// CAPI-09.
    async fn query_dependencies(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>>;
    /// CAPI-10.
    async fn analyze_impact(
        &self,
        ctx: &RequestContext,
        request: &ImpactQuery,
    ) -> CodeResult<ImpactReport>;
    /// CAPI-11.
    async fn get_edit_context(
        &self,
        ctx: &RequestContext,
        request: &EditContextQuery,
    ) -> CodeResult<EditContextResult>;
    /// CAPI-12.
    async fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &CodeRevisionDiff,
    ) -> CodeResult<Page<CodeChange>>;
    /// CAPI-13.
    async fn invalidate_revision(
        &self,
        ctx: &RequestContext,
        request: &InvalidateCodeRevision,
    ) -> CodeResult<ChangeReceipt>;
}

impl CodeIntelligencePort for CodeIntelligenceService {
    async fn analyze_repository(
        &self,
        ctx: &RequestContext,
        request: &AnalyzeRepository,
    ) -> CodeResult<JobHandle> {
        CodeIntelligenceService::analyze_repository(self, ctx, request)
    }

    async fn get_analysis_job(
        &self,
        ctx: &RequestContext,
        request: &GetJob,
    ) -> CodeResult<JobView> {
        CodeIntelligenceService::get_analysis_job(self, ctx, request)
    }

    async fn cancel_analysis_job(
        &self,
        ctx: &RequestContext,
        request: &CancelJob,
    ) -> CodeResult<JobView> {
        CodeIntelligenceService::cancel_analysis_job(self, ctx, request)
    }

    async fn get_symbol(&self, ctx: &RequestContext, request: &GetSymbol) -> CodeResult<Symbol> {
        CodeIntelligenceService::get_symbol(self, ctx, request)
    }

    async fn find_symbols(
        &self,
        ctx: &RequestContext,
        request: &SymbolQuery,
    ) -> CodeResult<Page<Symbol>> {
        CodeIntelligenceService::find_symbols(self, ctx, request)
    }

    async fn find_references(
        &self,
        ctx: &RequestContext,
        request: &ReferenceQuery,
    ) -> CodeResult<Page<ReferenceLocation>> {
        CodeIntelligenceService::find_references(self, ctx, request)
    }

    async fn query_callers(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>> {
        CodeIntelligenceService::query_callers(self, ctx, request)
    }

    async fn query_callees(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>> {
        CodeIntelligenceService::query_callees(self, ctx, request)
    }

    async fn query_dependencies(
        &self,
        ctx: &RequestContext,
        request: &TraversalQuery,
    ) -> CodeResult<Page<CodeEdge>> {
        CodeIntelligenceService::query_dependencies(self, ctx, request)
    }

    async fn analyze_impact(
        &self,
        ctx: &RequestContext,
        request: &ImpactQuery,
    ) -> CodeResult<ImpactReport> {
        CodeIntelligenceService::analyze_impact(self, ctx, request)
    }

    async fn get_edit_context(
        &self,
        ctx: &RequestContext,
        request: &EditContextQuery,
    ) -> CodeResult<EditContextResult> {
        CodeIntelligenceService::get_edit_context(self, ctx, request)
    }

    async fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &CodeRevisionDiff,
    ) -> CodeResult<Page<CodeChange>> {
        CodeIntelligenceService::compare_revisions(self, ctx, request)
    }

    async fn invalidate_revision(
        &self,
        ctx: &RequestContext,
        request: &InvalidateCodeRevision,
    ) -> CodeResult<ChangeReceipt> {
        CodeIntelligenceService::invalidate_revision(self, ctx, request)
    }
}
