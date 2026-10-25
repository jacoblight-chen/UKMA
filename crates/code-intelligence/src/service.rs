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

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use indexing_engine::{GenerationPointer, JobError, JobManager, Stage};
use sha2::{Digest, Sha256};
use ukma_contracts::{RequestContext, ScopeRef};

use crate::domain::{
    AnalyzeRepository, CancelJob, ChangeKind, ChangeReceipt, CodeChange, CodeRevisionDiff,
    ContextBlock, Direction, DirtyPolicy, EditContextQuery, EditContextResult, GetJob, GetSymbol,
    ImpactQuery, ImpactReport, InvalidateCodeRevision, JobHandle, JobView, Page, ReferenceLocation,
    ReferenceQuery, RepoRevision, SourceFile, SourceSnapshot, SymbolQuery, TraversalQuery,
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
}

/// In-memory source snapshots. Development/test only: it makes no durability or
/// cross-process claim.
#[derive(Debug, Default)]
pub struct InMemorySources {
    snapshots: BTreeMap<String, SourceSnapshot>,
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
}

impl SourceSnapshotPort for InMemorySources {
    fn snapshot(&self, revision: &RepoRevision) -> CodeResult<SourceSnapshot> {
        revision.validate()?;
        self.snapshots
            .get(&revision.commit_sha)
            .cloned()
            .ok_or_else(|| CodeError::SourceUnavailable {
                revision: revision.commit_sha.clone(),
            })
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
        }
    }

    /// Replace the authorizer.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn CodeAuthorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    fn lock(&self) -> MutexGuard<'_, ServiceState> {
        self.state.lock().expect("code service state poisoned")
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
        request.revision.validate()?;
        if !request.asynchronous {
            return Err(CodeError::InvalidArgument {
                field: "async",
                reason: "code.analyze_repository is asynchronous; pass `async: true` (?32.2)"
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
        self.require_parseable(&snapshot, request)?;

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

        // ?32.1: a graph containing dynamic/possible calls must never be
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
        let key = RevisionKey::new(&ctx.scope, repository_id, revision).label();
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
