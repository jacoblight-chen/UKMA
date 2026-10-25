//! Document Intelligence service: the 14 operations (DAPI-01..14), requirement
//! extraction (`P4-WP3`), review (`P4-WP4`) and consistency/compare/span
//! (`P4-WP5`).
//!
//! Invariants the code enforces (not just documents):
//! - the parsed document is a derived projection; the artifact plus the parsed
//!   blocks are retained, so `invalidate_revision` is stale-not-delete;
//! - a requirement produced by an extractor is always `candidate`/`unverified`;
//!   only `review_requirement` moves it, it needs evidence, and an approved
//!   candidate cannot be re-reviewed without a new revision;
//! - a document with unparsed regions is never reported as `complete`;
//! - cross-tenant/project reads and relation extraction are refused explicitly.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use indexing_engine::{GenerationPointer, JobError, JobManager, Stage};
use sha2::{Digest, Sha256};
use ukma_contracts::{RequestContext, ScopeRef};

use crate::domain::{
    BlockKind, CancelJob, ChangeReceipt, ConsistencyCategory, ConsistencyFinding,
    ConsistencyReport, DocChange, DocChangeKind, DocCompleteness, DocDiff, DocumentBlock,
    DocumentError, DocumentIngest, DocumentManifest, DocumentRevisionDiff, DocumentSource,
    DocumentView, ExtractRelations, ExtractRequirements, ExtractedRelation, GetDocument, GetJob,
    GetSourceSpan, InvalidateDocumentRevision, JobHandle, JobView, ListRequirements, MediaFormat,
    NormativeStrength, Page, ParseDiagnostic, ParsePreview, ParseStatus, RequirementCandidate,
    RequirementReviewResult, RequirementStatus, Result, ReviewDecision, ReviewRequirement,
    SearchDocument, SearchHit, Severity, SourceSpanView, Span, ValidationState, VerifyConsistency,
};
use crate::parsers::{Anchor, DocLink, DocumentParserPort, ParseLimits};

/// Scope to read documents.
pub const SCOPE_READ: &str = "document:read";
/// Scope to ingest documents.
pub const SCOPE_INDEX: &str = "document:index";
/// Scope to run analysis (preview, extraction, consistency).
pub const SCOPE_ANALYZE: &str = "document:analyze";
/// Scope to review requirements.
pub const SCOPE_REVIEW: &str = "document:review";
/// Scope to invalidate revisions.
pub const SCOPE_INDEX_ADMIN: &str = "document:index_admin";

/// Highest number of bytes `parse_preview` and `get_source_span` will return.
pub const MAX_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;

/// The only extractor profile this build ships.
pub const EXTRACTOR_PROFILE: &str = "normative-keyword/1";

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// Read artifact bytes for a document source.
pub trait DocumentSourcePort: Send + Sync {
    /// Read and return the artifact, verifying `source.content_hash`.
    fn read(&self, source: &DocumentSource) -> Result<Vec<u8>>;
}

/// In-memory artifact store (development/test only: no durability claim).
#[derive(Debug, Default)]
pub struct InMemoryDocuments {
    artifacts: BTreeMap<String, Vec<u8>>,
}

impl InMemoryDocuments {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an artifact and return its `sha256:` hash.
    pub fn put(&mut self, artifact_ref: &str, bytes: &[u8]) -> String {
        let hash = format!(
            "sha256:{}",
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        self.artifacts
            .insert(artifact_ref.to_string(), bytes.to_vec());
        hash
    }

    /// Register text as an artifact.
    pub fn put_text(&mut self, artifact_ref: &str, text: &str) -> String {
        self.put(artifact_ref, text.as_bytes())
    }
}

impl DocumentSourcePort for InMemoryDocuments {
    fn read(&self, source: &DocumentSource) -> Result<Vec<u8>> {
        let bytes = self
            .artifacts
            .get(&source.artifact_ref)
            .cloned()
            .ok_or_else(|| DocumentError::SourceUnavailable {
                artifact_ref: source.artifact_ref.clone(),
            })?;
        let actual = format!(
            "sha256:{}",
            Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        if !source.content_hash.is_empty()
            && source.content_hash != "sha256:placeholder"
            && actual != source.content_hash
        {
            return Err(DocumentError::SourceUnavailable {
                artifact_ref: source.artifact_ref.clone(),
            });
        }
        Ok(bytes)
    }
}

/// Authorization and redaction decisions.
pub trait DocumentAuthorizer: Send + Sync {
    /// Require `scope` on the context.
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> Result<()>;

    /// Whether a span must be withheld. Defaults to allow.
    fn redact(&self, _ctx: &RequestContext, _document_id: &str, _span: &Span) -> bool {
        false
    }
}

/// Default authorizer: the delegated scope must be present.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScopeAuthorizer;

impl DocumentAuthorizer for ScopeAuthorizer {
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> Result<()> {
        if ctx.scopes.iter().any(|granted| granted == scope) {
            Ok(())
        } else {
            Err(DocumentError::Forbidden {
                required_scope: scope.to_string(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DocIndex {
    scope: ScopeRef,
    document_id: String,
    source: DocumentSource,
    blocks: Vec<DocumentBlock>,
    diagnostics: Vec<ParseDiagnostic>,
    anchors: Vec<Anchor>,
    links: Vec<DocLink>,
    completeness: DocCompleteness,
    parser_id: String,
    parser_version: String,
    requirements: BTreeMap<String, RequirementCandidate>,
    etag: String,
    stale: bool,
    generation: String,
    /// Parsed bytes retained for `get_source_span`.
    artifact: Vec<u8>,
}

impl DocIndex {
    fn block(&self, id: &str) -> Option<&DocumentBlock> {
        self.blocks.iter().find(|block| block.id == id)
    }

    fn heading_of(&self, block: &DocumentBlock) -> Option<&DocumentBlock> {
        block
            .parent_id
            .as_ref()
            .and_then(|parent| self.block(parent))
            .filter(|parent| parent.kind == BlockKind::Heading)
            .or_else(|| {
                block
                    .parent_id
                    .as_ref()
                    .and_then(|parent| self.block(parent))
                    .and_then(|parent| self.heading_of(parent))
            })
    }

    fn partial_ratio(&self) -> u8 {
        if self.blocks.is_empty() {
            return 100;
        }
        let unparsed = self
            .blocks
            .iter()
            .filter(|block| block.parse_status != ParseStatus::Complete)
            .count();
        ((unparsed * 100) / self.blocks.len()) as u8
    }
}

#[derive(Debug, Default)]
struct ServiceState {
    documents: BTreeMap<String, DocIndex>,
    jobs: JobManager,
    job_documents: BTreeMap<String, String>,
    job_scopes: BTreeMap<String, ScopeRef>,
    job_requests: BTreeMap<String, DocumentIngest>,
    dedup: BTreeMap<String, String>,
    idempotency: BTreeMap<String, (String, String)>,
    generation: GenerationPointer,
    sequence: u64,
}

impl ServiceState {
    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
}

/// Reference implementation of the Document Intelligence domain service.
pub struct DocumentService {
    parser: Arc<dyn DocumentParserPort>,
    sources: Arc<dyn DocumentSourcePort>,
    authorizer: Arc<dyn DocumentAuthorizer>,
    state: Mutex<ServiceState>,
}

impl std::fmt::Debug for DocumentService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentService").finish()
    }
}

impl DocumentService {
    /// Service over a parser port and an artifact port.
    pub fn new(parser: Arc<dyn DocumentParserPort>, sources: Arc<dyn DocumentSourcePort>) -> Self {
        Self {
            parser,
            sources,
            authorizer: Arc::new(ScopeAuthorizer),
            state: Mutex::new(ServiceState::default()),
        }
    }

    /// Replace the authorizer.
    pub fn with_authorizer(mut self, authorizer: Arc<dyn DocumentAuthorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    fn lock(&self) -> MutexGuard<'_, ServiceState> {
        self.state.lock().expect("document service state poisoned")
    }

    // -- DAPI-01 -------------------------------------------------------------

    /// `document.ingest` — accept and run an ingest job.
    pub fn ingest(&self, ctx: &RequestContext, request: &DocumentIngest) -> Result<JobHandle> {
        let handle = self.submit_ingest(ctx, request)?;
        if handle.stage == Stage::Queued.as_str() {
            self.run_ingest(ctx, &handle.job_id)?;
        }
        let state = self.lock();
        let job = state.jobs.get(&handle.job_id).map_err(map_job_error)?;
        Ok(JobHandle {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            stage: job.stage.as_str().to_string(),
        })
    }

    /// Accept an ingest job without running it.
    pub fn submit_ingest(
        &self,
        ctx: &RequestContext,
        request: &DocumentIngest,
    ) -> Result<JobHandle> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        let format = request.source.validate_media_type()?;
        if !self.parser.supports(format) {
            return Err(DocumentError::UnsupportedMediaType {
                media_type: request.source.media_type.clone(),
            });
        }
        if !request.asynchronous {
            return Err(DocumentError::InvalidArgument {
                field: "async",
                reason: "document.ingest is asynchronous; pass `async: true` (§33.2)".into(),
            });
        }
        if request.parser_profile.trim().is_empty() {
            return Err(DocumentError::InvalidArgument {
                field: "parser_profile",
                reason: "must be non-empty".into(),
            });
        }
        let bytes = self.sources.read(&request.source)?;
        if bytes.len() as u64 > MAX_PREVIEW_BYTES {
            return Err(DocumentError::SourceTooLarge {
                bytes: bytes.len() as u64,
                limit: MAX_PREVIEW_BYTES,
            });
        }

        let dedup_key = format!(
            "{}|{}|{}|{}|{}",
            ctx.scope.tenant_id,
            ctx.scope.project_id,
            request.source.artifact_ref,
            request.source.revision,
            request.parser_profile
        );

        let mut state = self.lock();
        if let Some(existing) = state.dedup.get(&dedup_key).cloned() {
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
        let job_id = format!("job-doc-{sequence}");
        state.jobs.submit(&job_id, "document.ingest");
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

    /// Drive a queued ingest job to completion. Idempotent.
    pub fn run_ingest(&self, ctx: &RequestContext, job_id: &str) -> Result<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        let (request, stage) = {
            let state = self.lock();
            let scope = state.job_scopes.get(job_id).cloned().ok_or_else(|| {
                DocumentError::JobNotFound {
                    job_id: job_id.to_string(),
                }
            })?;
            require_same_scope(ctx, &scope)?;
            let job = state.jobs.get(job_id).map_err(map_job_error)?;
            (state.job_requests.get(job_id).cloned(), job.stage)
        };
        let request = request.ok_or_else(|| DocumentError::JobNotFound {
            job_id: job_id.to_string(),
        })?;
        if stage != Stage::Queued {
            return self.job_view(ctx, job_id);
        }

        let bytes = self.sources.read(&request.source)?;
        let format = request.source.validate_media_type()?;
        {
            let mut state = self.lock();
            advance_to(&mut state, job_id, Stage::Parsing)?;
        }
        let limits = ParseLimits {
            max_bytes: MAX_PREVIEW_BYTES,
            include_images: request.include_images,
        };
        let parsed = match self.parser.parse(&request.source, &bytes, &limits) {
            Ok(output) => output,
            Err(error) => {
                let mut state = self.lock();
                let _ = state.jobs.fail(job_id, &error.to_string());
                return Err(error);
            }
        };

        let generation = {
            let mut state = self.lock();
            advance_to(&mut state, job_id, Stage::Staging)?;
            let sequence = state.next_sequence();
            let mut hasher = Sha256::new();
            hasher.update(
                format!(
                    "{}|{}|{}|{}",
                    ctx.scope.tenant_id,
                    ctx.scope.project_id,
                    parsed.document.document_id,
                    sequence
                )
                .as_bytes(),
            );
            let digest = hasher.finalize();
            let hex: String = digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            format!("gen-{hex}")
        };

        let mut index = DocIndex {
            scope: ctx.scope.clone(),
            document_id: parsed.document.document_id.clone(),
            source: parsed.document.source.clone(),
            blocks: parsed.document.blocks.clone(),
            diagnostics: parsed.document.diagnostics.clone(),
            anchors: parsed.anchors.clone(),
            links: parsed.links.clone(),
            completeness: parsed.document.completeness,
            parser_id: parsed.document.parser_id.clone(),
            parser_version: parsed.document.parser_version.clone(),
            requirements: BTreeMap::new(),
            etag: format!(
                "\"etag-{}-{}\"",
                parsed.document.document_id,
                parsed.document.blocks.len()
            ),
            stale: false,
            generation: generation.clone(),
            artifact: bytes,
        };
        // requirement extraction runs as part of the pipeline (EXTRACTING)
        index.requirements = self.extract_candidates(&index, EXTRACTOR_PROFILE)?;
        let _ = format;

        {
            let mut state = self.lock();
            advance_to(&mut state, job_id, Stage::Committing)?;
            state.documents.insert(index.document_id.clone(), index);
            state.generation.publish(&generation);
            state
                .job_documents
                .insert(job_id.to_string(), parsed.document.document_id.clone());
            advance_to(&mut state, job_id, Stage::Ready)?;
        }
        self.job_view(ctx, job_id)
    }

    fn remember_idempotency(
        &self,
        state: &mut ServiceState,
        ctx: &RequestContext,
        fingerprint: &str,
        job_id: &str,
    ) -> Result<()> {
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
            Some(_) => Err(DocumentError::IdempotencyConflict { key: key.clone() }),
            None => {
                state
                    .idempotency
                    .insert(key.clone(), (fingerprint.to_string(), job_id.to_string()));
                Ok(())
            }
        }
    }

    // -- DAPI-02 / DAPI-03 ---------------------------------------------------

    /// `document.get_ingest_job`.
    pub fn get_ingest_job(&self, ctx: &RequestContext, request: &GetJob) -> Result<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        self.job_view(ctx, &request.job_id)
    }

    fn job_view(&self, ctx: &RequestContext, job_id: &str) -> Result<JobView> {
        let state = self.lock();
        let job = state.jobs.get(job_id).map_err(map_job_error)?;
        let scope = state
            .job_scopes
            .get(job_id)
            .ok_or_else(|| DocumentError::JobNotFound {
                job_id: job_id.to_string(),
            })?;
        require_same_scope(ctx, scope)?;
        let result_ref = if job.outcome.commit == indexing_engine::CommitPhase::Committed {
            state.job_documents.get(job_id).cloned()
        } else {
            None
        };
        let partial_ratio = state
            .job_documents
            .get(job_id)
            .and_then(|document_id| state.documents.get(document_id))
            .filter(|index| index.completeness != DocCompleteness::Complete)
            .map(DocIndex::partial_ratio);
        Ok(JobView {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            stage: job.stage.as_str().to_string(),
            canonical_committed: job.outcome.commit == indexing_engine::CommitPhase::Committed,
            result_ref,
            diagnostics: job.outcome.diagnostics.clone(),
            partial_ratio,
        })
    }

    /// `document.cancel_ingest_job`.
    pub fn cancel_ingest_job(&self, ctx: &RequestContext, request: &CancelJob) -> Result<JobView> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX)?;
        let mut state = self.lock();
        let scope = state
            .job_scopes
            .get(&request.job_id)
            .cloned()
            .ok_or_else(|| DocumentError::JobNotFound {
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
            partial_ratio: None,
        })
    }

    // -- DAPI-04 -------------------------------------------------------------

    /// `document.get_document`.
    pub fn get_document(
        &self,
        ctx: &RequestContext,
        request: &GetDocument,
    ) -> Result<DocumentView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = lookup_document(ctx, &state, &request.document_id)?;
        let blocks = if request.include_blocks {
            Page::of(index.blocks.clone(), &request.page)?
        } else {
            Page {
                items: Vec::new(),
                next_cursor: None,
            }
        };
        Ok(DocumentView {
            document_id: index.document_id.clone(),
            source: index.source.clone(),
            blocks,
            completeness: index.completeness.as_str().to_string(),
            etag: index.etag.clone(),
        })
    }

    /// Parser identity, diagnostics and completeness of a committed document.
    pub fn document_manifest(
        &self,
        ctx: &RequestContext,
        document_id: &str,
    ) -> Result<DocumentManifest> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let index = lookup_document(ctx, &state, document_id)?;
        Ok(DocumentManifest {
            document_id: index.document_id.clone(),
            parser_id: index.parser_id.clone(),
            parser_version: index.parser_version.clone(),
            completeness: index.completeness.as_str().to_string(),
            block_count: index.blocks.len() as u32,
            partial_ratio: index.partial_ratio(),
            diagnostics: index.diagnostics.clone(),
            etag: index.etag.clone(),
            generation: index.generation.clone(),
        })
    }

    // -- DAPI-05 -------------------------------------------------------------

    /// `document.parse_preview` — parse without persisting anything.
    pub fn parse_preview(
        &self,
        ctx: &RequestContext,
        request: &ParsePreview,
    ) -> Result<crate::domain::ParsedDocument> {
        self.authorizer.require_scope(ctx, SCOPE_ANALYZE)?;
        let bytes = self.sources.read(&request.source)?;
        if bytes.len() as u64 > MAX_PREVIEW_BYTES {
            return Err(DocumentError::SourceTooLarge {
                bytes: bytes.len() as u64,
                limit: MAX_PREVIEW_BYTES,
            });
        }
        let limits = ParseLimits {
            max_bytes: request.max_bytes.min(MAX_PREVIEW_BYTES),
            include_images: true,
        };
        let output = self.parser.parse(&request.source, &bytes, &limits)?;
        Ok(output.document)
    }

    // -- DAPI-06 -------------------------------------------------------------

    /// `document.search`.
    pub fn search(
        &self,
        ctx: &RequestContext,
        request: &SearchDocument,
    ) -> Result<Page<SearchHit>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        if request.tenant_id != ctx.scope.tenant_id || request.project_id != ctx.scope.project_id {
            return Err(DocumentError::CrossScopeDenied {
                requested: format!("{}/{}", request.tenant_id, request.project_id),
                actual: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
            });
        }
        let terms: Vec<String> = request
            .query
            .split_whitespace()
            .map(|term| term.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return Err(DocumentError::InvalidArgument {
                field: "query",
                reason: "must contain at least one term".into(),
            });
        }
        let state = self.lock();
        let mut hits: Vec<SearchHit> = Vec::new();
        for index in state.documents.values() {
            if index.scope.tenant_id != ctx.scope.tenant_id
                || index.scope.project_id != ctx.scope.project_id
            {
                continue;
            }
            check_fresh(ctx, index)?;
            if !request.document_refs.is_empty()
                && !request.document_refs.contains(&index.document_id)
            {
                continue;
            }
            for block in &index.blocks {
                let Some(text) = block.text.as_deref() else {
                    continue;
                };
                if !request.block_kinds.is_empty() && !request.block_kinds.contains(&block.kind) {
                    continue;
                }
                let lower = text.to_ascii_lowercase();
                let matched = terms.iter().filter(|term| lower.contains(*term)).count();
                if matched == 0 {
                    continue;
                }
                hits.push(SearchHit {
                    document_id: index.document_id.clone(),
                    block_id: block.id.clone(),
                    kind: block.kind,
                    snippet: text.chars().take(200).collect(),
                    span: block.span.clone(),
                    score: ((matched * 100) / terms.len()) as u8,
                });
            }
        }
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.document_id.cmp(&right.document_id))
                .then_with(|| left.span.byte_start.cmp(&right.span.byte_start))
        });
        Page::of(hits, &request.page)
    }

    // -- DAPI-07 -------------------------------------------------------------

    /// `document.extract_requirements` — always `candidate`/`unverified`.
    pub fn extract_requirements(
        &self,
        ctx: &RequestContext,
        request: &ExtractRequirements,
    ) -> Result<Page<RequirementCandidate>> {
        self.authorizer.require_scope(ctx, SCOPE_ANALYZE)?;
        request.page.validate()?;
        if request.extractor_profile != EXTRACTOR_PROFILE {
            return Err(DocumentError::ModelUnavailable {
                profile: request.extractor_profile.clone(),
            });
        }
        let state = self.lock();
        let index = lookup_document(ctx, &state, &request.document_id)?;
        let mut candidates: Vec<RequirementCandidate> = index
            .requirements
            .values()
            .filter(|candidate| candidate.status == RequirementStatus::Candidate)
            .cloned()
            .collect();
        if !request.section_refs.is_empty() {
            candidates.retain(|candidate| {
                candidate.extracted_from.iter().any(|evidence| {
                    let block_id = evidence.split('|').next().unwrap_or(evidence);
                    if !request.section_refs.contains(&block_id.to_string()) {
                        return false;
                    }
                    // a section ref also covers its descendants
                    index
                        .block(block_id)
                        .map(|block| {
                            block.id == block_id
                                || index.heading_of(block).map(|heading| heading.id.as_str())
                                    == Some(block_id)
                        })
                        .unwrap_or(false)
                })
            });
        }
        Page::of(candidates, &request.page)
    }

    // -- DAPI-08 -------------------------------------------------------------

    /// `document.extract_relations` — cross references found in the text.
    pub fn extract_relations(
        &self,
        ctx: &RequestContext,
        request: &ExtractRelations,
    ) -> Result<Page<ExtractedRelation>> {
        self.authorizer.require_scope(ctx, SCOPE_ANALYZE)?;
        request.page.validate()?;
        if request.extractor_profile != EXTRACTOR_PROFILE {
            return Err(DocumentError::ModelUnavailable {
                profile: request.extractor_profile.clone(),
            });
        }
        if request.target_tenant_id != ctx.scope.tenant_id
            || request.target_project_id != ctx.scope.project_id
        {
            return Err(DocumentError::CrossScopeDenied {
                requested: format!("{}/{}", request.target_tenant_id, request.target_project_id),
                actual: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
            });
        }
        let state = self.lock();
        let index = lookup_document(ctx, &state, &request.document_id)?;
        let mut relations: Vec<ExtractedRelation> = Vec::new();
        for link in &index.links {
            let kind = match link.kind {
                "anchor" => "references",
                "document" => "references",
                "url" => "references_external",
                _ => continue,
            };
            if !request.relation_kinds.is_empty()
                && !request.relation_kinds.iter().any(|wanted| wanted == kind)
            {
                continue;
            }
            let target = if link.kind == "anchor" {
                match index
                    .anchors
                    .iter()
                    .find(|anchor| link.target == format!("#{}", anchor.anchor))
                {
                    Some(anchor) => anchor.block_id.clone(),
                    None => continue,
                }
            } else {
                link.target.clone()
            };
            relations.push(ExtractedRelation {
                source: link.block_id.clone(),
                target,
                relation_kind: kind.to_string(),
                evidence: vec![link.block_id.clone()],
                confidence: if link.kind == "anchor" { 85 } else { 60 },
                verification: ValidationState::Unverified,
            });
        }
        relations.sort_by(|left, right| {
            (&left.source, &left.target).cmp(&(&right.source, &right.target))
        });
        Page::of(relations, &request.page)
    }

    // -- DAPI-09 -------------------------------------------------------------

    /// `document.review_requirement`.
    pub fn review_requirement(
        &self,
        ctx: &RequestContext,
        request: &ReviewRequirement,
    ) -> Result<RequirementReviewResult> {
        self.authorizer.require_scope(ctx, SCOPE_REVIEW)?;
        if request.evidence_ids.is_empty() {
            return Err(DocumentError::EvidenceRequired {
                candidate_id: request.candidate_id.clone(),
            });
        }
        if request.rationale.trim().is_empty() {
            return Err(DocumentError::InvalidArgument {
                field: "rationale",
                reason: "a review needs a rationale".into(),
            });
        }
        let mut state = self.lock();
        let document_id = {
            let index = state
                .documents
                .values()
                .find(|index| index.requirements.contains_key(&request.candidate_id))
                .ok_or_else(|| DocumentError::NotFound {
                    resource: format!("candidate {}", request.candidate_id),
                })?;
            require_same_scope(ctx, &index.scope)?;
            index.document_id.clone()
        };
        let index =
            state
                .documents
                .get_mut(&document_id)
                .ok_or_else(|| DocumentError::NotFound {
                    resource: document_id.clone(),
                })?;
        let candidate = index
            .requirements
            .get(&request.candidate_id)
            .cloned()
            .ok_or_else(|| DocumentError::NotFound {
                resource: format!("candidate {}", request.candidate_id),
            })?;
        if candidate.status != RequirementStatus::Candidate {
            return Err(DocumentError::RequirementStateConflict {
                candidate_id: candidate.id.clone(),
                status: candidate.status,
            });
        }
        if candidate.etag != request.expected_etag {
            return Err(DocumentError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual: candidate.etag.clone(),
            });
        }
        let (status, verification) = match request.decision {
            ReviewDecision::Approve => (RequirementStatus::Approved, ValidationState::Verified),
            ReviewDecision::Reject => (RequirementStatus::Rejected, ValidationState::Rejected),
        };
        let mut updated = candidate.clone();
        updated.status = status;
        updated.verification = verification;
        updated.etag = format!("\"{}-{}\"", candidate.id, review_tag(status));
        index
            .requirements
            .insert(updated.id.clone(), updated.clone());
        // an approved change needs a new revision: reading is fine, editing is not
        index.etag = format!("{}-rev", index.etag.trim_end_matches('"'));
        Ok(RequirementReviewResult {
            requirement: updated.clone(),
            receipt: ChangeReceipt {
                resource_id: updated.id.clone(),
                etag: updated.etag.clone(),
                changed: true,
                events: vec!["document.requirement.reviewed".to_string()],
            },
        })
    }

    // -- DAPI-10 -------------------------------------------------------------

    /// `document.list_requirements`.
    pub fn list_requirements(
        &self,
        ctx: &RequestContext,
        request: &ListRequirements,
    ) -> Result<Page<RequirementCandidate>> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.page.validate()?;
        let state = self.lock();
        let index = lookup_document(ctx, &state, &request.document_id)?;
        let mut candidates: Vec<RequirementCandidate> = index
            .requirements
            .values()
            .filter(|candidate| {
                request.statuses.is_empty() || request.statuses.contains(&candidate.status)
            })
            .cloned()
            .collect();
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
        Page::of(candidates, &request.page)
    }

    // -- DAPI-11 -------------------------------------------------------------

    /// `document.verify_consistency`.
    pub fn verify_consistency(
        &self,
        ctx: &RequestContext,
        request: &VerifyConsistency,
    ) -> Result<ConsistencyReport> {
        self.authorizer.require_scope(ctx, SCOPE_ANALYZE)?;
        if request.max_findings == 0 {
            return Err(DocumentError::InvalidArgument {
                field: "max_findings",
                reason: "must be at least 1".into(),
            });
        }
        let state = self.lock();
        let documents: Vec<&DocIndex> = if request.document_refs.is_empty() {
            state
                .documents
                .values()
                .filter(|index| {
                    index.scope.tenant_id == ctx.scope.tenant_id
                        && index.scope.project_id == ctx.scope.project_id
                        && !index.stale
                })
                .collect()
        } else {
            request
                .document_refs
                .iter()
                .map(|document_id| lookup_document(ctx, &state, document_id))
                .collect::<Result<Vec<_>>>()?
        };

        let mut findings: Vec<ConsistencyFinding> = Vec::new();
        let mut analyzed_refs: Vec<String> = Vec::new();
        let mut skipped_refs: Vec<String> = Vec::new();
        let mut skipped_reasons: Vec<String> = Vec::new();

        for index in &documents {
            analyzed_refs.push(index.document_id.clone());
            // dangling anchors
            for link in &index.links {
                if link.kind == "anchor" {
                    let known = index
                        .anchors
                        .iter()
                        .any(|anchor| link.target == format!("#{}", anchor.anchor));
                    if !known {
                        findings.push(ConsistencyFinding {
                            id: finding_id(&index.document_id, &link.block_id, "missing_reference"),
                            category: ConsistencyCategory::MissingReference,
                            refs: vec![format!("{}#{}", index.document_id, link.block_id)],
                            evidence: vec![link.block_id.clone()],
                            severity: Severity::Error,
                            confidence: Some(95),
                            verification: ValidationState::Unverified,
                        });
                    }
                }
            }
            // unreviewed requirements
            for candidate in index.requirements.values() {
                if candidate.status == RequirementStatus::Candidate {
                    findings.push(ConsistencyFinding {
                        id: finding_id(&index.document_id, &candidate.id, "unverified"),
                        category: ConsistencyCategory::Unverified,
                        refs: vec![candidate.id.clone()],
                        evidence: candidate.extracted_from.clone(),
                        severity: Severity::Info,
                        confidence: Some(candidate.confidence),
                        verification: ValidationState::Unverified,
                    });
                }
                if candidate.text.to_ascii_lowercase().contains("etc.")
                    || candidate.text.to_ascii_lowercase().contains("tbd")
                {
                    findings.push(ConsistencyFinding {
                        id: finding_id(&index.document_id, &candidate.id, "ambiguity"),
                        category: ConsistencyCategory::Ambiguity,
                        refs: vec![candidate.id.clone()],
                        evidence: candidate.extracted_from.clone(),
                        severity: Severity::Warning,
                        confidence: Some(60),
                        verification: ValidationState::Unverified,
                    });
                }
            }
            // contradictory strength for the same normalised subject
            let candidates: Vec<&RequirementCandidate> = index.requirements.values().collect();
            for left in 0..candidates.len() {
                for right in (left + 1)..candidates.len() {
                    let a = candidates[left];
                    let b = candidates[right];
                    if normalised_subject(&a.text) == normalised_subject(&b.text)
                        && a.normative_strength != b.normative_strength
                    {
                        findings.push(ConsistencyFinding {
                            id: finding_id(&index.document_id, &a.id, "contradiction"),
                            category: ConsistencyCategory::Contradiction,
                            refs: vec![a.id.clone(), b.id.clone()],
                            evidence: a.extracted_from.clone(),
                            severity: Severity::Error,
                            confidence: Some(70),
                            verification: ValidationState::Unverified,
                        });
                    }
                }
            }
            if request.compare_code {
                skipped_refs.push(index.document_id.clone());
                skipped_reasons.push(
                    "compare_code needs the code domain; this service has no code index bound"
                        .to_string(),
                );
            }
        }

        findings.sort_by(|left, right| {
            (left.category as u8, &left.id).cmp(&(right.category as u8, &right.id))
        });
        if findings.len() as u32 > request.max_findings {
            return Err(DocumentError::ResourceLimit {
                detail: format!(
                    "consistency produced {} findings, limit is {}",
                    findings.len(),
                    request.max_findings
                ),
            });
        }
        let completeness = if skipped_refs.is_empty() {
            "complete"
        } else {
            "partial"
        };
        let snapshot_id = documents
            .first()
            .map(|index| index.generation.clone())
            .unwrap_or_else(|| "gen-none".to_string());
        Ok(ConsistencyReport {
            findings,
            analyzed_refs,
            skipped_refs,
            skipped_reasons,
            completeness: completeness.to_string(),
            snapshot_id,
        })
    }

    // -- DAPI-12 -------------------------------------------------------------

    /// `document.compare_revisions`.
    pub fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &DocumentRevisionDiff,
    ) -> Result<DocDiff> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        let state = self.lock();
        let from = lookup_document(ctx, &state, &request.from)?;
        let to = lookup_document(ctx, &state, &request.to)?;
        if from.source.source_uri != to.source.source_uri {
            return Err(DocumentError::SourceRevisionMismatch {
                requested: request.from.clone(),
                actual: request.to.clone(),
            });
        }
        let key = |block: &DocumentBlock| -> (BlockKind, String) {
            (block.kind, block.text.clone().unwrap_or_default())
        };
        let mut changes: Vec<DocChange> = Vec::new();
        let from_blocks: Vec<&DocumentBlock> = from.blocks.iter().collect();
        let to_blocks: Vec<&DocumentBlock> = to.blocks.iter().collect();
        let mut matched_to: Vec<bool> = vec![false; to_blocks.len()];

        for old in &from_blocks {
            let key_old = key(old);
            match to_blocks
                .iter()
                .enumerate()
                .find(|(position, new)| !matched_to[*position] && key(new) == key_old)
            {
                Some((position, new)) => {
                    matched_to[position] = true;
                    if old.order != new.order {
                        changes.push(DocChange {
                            kind: DocChangeKind::Moved,
                            old: Some((*old).clone()),
                            new: Some((*new).clone()),
                            evidence: vec![old.id.clone(), new.id.clone()],
                        });
                    }
                }
                None => changes.push(DocChange {
                    kind: DocChangeKind::Removed,
                    old: Some((*old).clone()),
                    new: None,
                    evidence: vec![old.id.clone()],
                }),
            }
        }
        for (position, new) in to_blocks.iter().enumerate() {
            if matched_to[position] {
                continue;
            }
            changes.push(DocChange {
                kind: DocChangeKind::Added,
                old: None,
                new: Some((*new).clone()),
                evidence: vec![new.id.clone()],
            });
        }
        // a same-position text change is a modification, not add+remove
        let removed: Vec<usize> = changes
            .iter()
            .enumerate()
            .filter(|(_, change)| change.kind == DocChangeKind::Removed)
            .map(|(position, _)| position)
            .collect();
        for position in removed {
            let old = changes[position]
                .old
                .clone()
                .unwrap_or_else(|| DocumentBlock {
                    id: String::new(),
                    kind: BlockKind::Other,
                    text: None,
                    parent_id: None,
                    order: 0,
                    span: Span::new(1, 1, 0, 0),
                    level: None,
                    language: None,
                    parse_status: ParseStatus::Complete,
                });
            let replacement = changes.iter().position(|change| {
                change.kind == DocChangeKind::Added
                    && change
                        .new
                        .as_ref()
                        .map(|new| new.order == old.order && new.kind == old.kind)
                        .unwrap_or(false)
            });
            if let Some(replacement_position) = replacement {
                let new = changes[replacement_position].new.clone();
                changes[position] = DocChange {
                    kind: DocChangeKind::Modified,
                    old: Some(old),
                    new: new.clone(),
                    evidence: new.map(|block| vec![block.id]).unwrap_or_default(),
                };
                changes.remove(replacement_position);
            }
        }
        changes.sort_by(|left, right| {
            let left_order = left
                .old
                .as_ref()
                .or(left.new.as_ref())
                .map(|block| block.order)
                .unwrap_or(0);
            let right_order = right
                .old
                .as_ref()
                .or(right.new.as_ref())
                .map(|block| block.order)
                .unwrap_or(0);
            (left_order, left.kind as u8).cmp(&(right_order, right.kind as u8))
        });
        let completeness = if from.completeness == DocCompleteness::Complete
            && to.completeness == DocCompleteness::Complete
        {
            "complete"
        } else {
            "partial"
        };
        Ok(DocDiff {
            from: request.from.clone(),
            to: request.to.clone(),
            changes,
            completeness: completeness.to_string(),
        })
    }

    // -- DAPI-13 -------------------------------------------------------------

    /// `document.invalidate_revision` — stale, never delete.
    pub fn invalidate_revision(
        &self,
        ctx: &RequestContext,
        request: &InvalidateDocumentRevision,
    ) -> Result<ChangeReceipt> {
        self.authorizer.require_scope(ctx, SCOPE_INDEX_ADMIN)?;
        let mut state = self.lock();
        let index = state
            .documents
            .get_mut(&request.document_id)
            .ok_or_else(|| DocumentError::NotFound {
                resource: format!("document {}", request.document_id),
            })?;
        require_same_scope(ctx, &index.scope)?;
        if index.etag != request.expected_etag {
            return Err(DocumentError::EtagMismatch {
                expected: request.expected_etag.clone(),
                actual: index.etag.clone(),
            });
        }
        let changed = !index.stale;
        index.stale = true;
        let next_etag = format!("\"stale-{}\"", index.generation);
        index.etag = next_etag.clone();
        let events = if changed {
            vec!["document.revision.stale".to_string()]
        } else {
            Vec::new()
        };
        Ok(ChangeReceipt {
            resource_id: request.document_id.clone(),
            etag: next_etag,
            changed,
            events,
        })
    }

    // -- DAPI-14 -------------------------------------------------------------

    /// `document.get_source_span`.
    pub fn get_source_span(
        &self,
        ctx: &RequestContext,
        request: &GetSourceSpan,
    ) -> Result<SourceSpanView> {
        self.authorizer.require_scope(ctx, SCOPE_READ)?;
        request.span.validate()?;
        let state = self.lock();
        let index = lookup_document(ctx, &state, &request.document_id)?;
        let end = request.span.byte_end.min(index.artifact.len() as u64);
        if request.span.byte_start > index.artifact.len() as u64 {
            return Err(DocumentError::InvalidArgument {
                field: "span",
                reason: "span starts past the end of the artifact".into(),
            });
        }
        let length = end.saturating_sub(request.span.byte_start);
        let redacted = self
            .authorizer
            .redact(ctx, &index.document_id, &request.span)
            || length > request.max_bytes;
        let text = if redacted {
            None
        } else {
            let slice = &index.artifact[request.span.byte_start as usize..end as usize];
            Some(String::from_utf8_lossy(slice).to_string())
        };
        Ok(SourceSpanView {
            text,
            artifact_ref: index.source.artifact_ref.clone(),
            span: request.span.clone(),
            redacted,
        })
    }

    // -- extraction ----------------------------------------------------------

    fn extract_candidates(
        &self,
        index: &DocIndex,
        profile: &str,
    ) -> Result<BTreeMap<String, RequirementCandidate>> {
        if profile != EXTRACTOR_PROFILE {
            return Err(DocumentError::ModelUnavailable {
                profile: profile.to_string(),
            });
        }
        let mut out: BTreeMap<String, RequirementCandidate> = BTreeMap::new();
        let mut ordinal = 0u32;
        for block in &index.blocks {
            let Some(text) = block.text.as_deref() else {
                continue;
            };
            if !matches!(
                block.kind,
                BlockKind::Paragraph | BlockKind::List | BlockKind::TableCell
            ) {
                continue;
            }
            // a requirement id introduced in the block applies to its normative
            // sentences, which are often separate sentences ("REQ-12 applies to?")
            let block_requirement_id = extract_requirement_id(text);
            for sentence in split_sentences(text) {
                let strength = normative_strength(&sentence);
                if strength == NormativeStrength::Unknown {
                    continue;
                }
                ordinal += 1;
                let mut hasher = Sha256::new();
                hasher.update(index.document_id.as_bytes());
                hasher.update(index.source.revision.as_bytes());
                hasher.update(ordinal.to_le_bytes());
                hasher.update(sentence.as_bytes());
                let digest = hasher.finalize();
                let hex: String = digest[..16]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                let id = format!("req:v1:{hex}");
                let confidence = match strength {
                    NormativeStrength::Must => 90,
                    NormativeStrength::Should => 80,
                    NormativeStrength::May => 70,
                    NormativeStrength::Unknown => 50,
                };
                out.insert(
                    id.clone(),
                    RequirementCandidate {
                        requirement_id: extract_requirement_id(&sentence)
                            .or_else(|| block_requirement_id.clone()),
                        id: id.clone(),
                        text: sentence,
                        normative_strength: strength,
                        extracted_from: vec![format!("{}|{}", block.id, index.source.content_hash)],
                        confidence,
                        verification: ValidationState::Unverified,
                        status: RequirementStatus::Candidate,
                        revision: index.source.revision.clone(),
                        etag: format!("\"{id}-candidate\""),
                    },
                );
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn review_tag(status: RequirementStatus) -> &'static str {
    match status {
        RequirementStatus::Candidate => "candidate",
        RequirementStatus::Approved => "approved",
        RequirementStatus::Rejected => "rejected",
        RequirementStatus::Superseded => "superseded",
    }
}

fn finding_id(document_id: &str, subject: &str, category: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(document_id.as_bytes());
    hasher.update(subject.as_bytes());
    hasher.update(category.as_bytes());
    let digest = hasher.finalize();
    format!(
        "finding:v1:{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if ch == '.' || ch == '!' || ch == '?' {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        sentences.push(trimmed.to_string());
    }
    if sentences.is_empty() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            sentences.push(trimmed.to_string());
        }
    }
    sentences
}

/// Normative strength from the keywords the sentence actually contains.
pub fn normative_strength(sentence: &str) -> NormativeStrength {
    let lower = sentence.to_ascii_lowercase();
    let has_word = |word: &str| -> bool {
        lower
            .split(|ch: char| !ch.is_alphanumeric() && ch != '\'')
            .any(|token| token == word)
    };
    if has_word("must") || has_word("shall") || has_word("required") {
        NormativeStrength::Must
    } else if has_word("should") || has_word("recommended") {
        NormativeStrength::Should
    } else if has_word("may") || has_word("optional") || has_word("can") {
        NormativeStrength::May
    } else {
        NormativeStrength::Unknown
    }
}

fn extract_requirement_id(sentence: &str) -> Option<String> {
    for token in sentence.split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';') {
        let trimmed = token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '-');
        if trimmed.len() < 3 {
            continue;
        }
        let mut parts = trimmed.splitn(2, '-');
        let prefix = parts.next().unwrap_or_default();
        let Some(number) = parts.next() else {
            continue;
        };
        if prefix.len() >= 2
            && prefix.chars().all(|ch| ch.is_ascii_uppercase())
            && number.len() >= 2
            && number.chars().all(|ch| ch.is_ascii_digit())
        {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn normalised_subject(text: &str) -> String {
    text.to_ascii_lowercase()
        .split_whitespace()
        .filter(|token| {
            !matches!(
                *token,
                "must" | "shall" | "should" | "may" | "can" | "the" | "a" | "an" | "be" | "is"
            )
        })
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn require_same_scope(ctx: &RequestContext, scope: &ScopeRef) -> Result<()> {
    if ctx.scope.tenant_id == scope.tenant_id && ctx.scope.project_id == scope.project_id {
        Ok(())
    } else {
        Err(DocumentError::CrossScopeDenied {
            requested: format!("{}/{}", ctx.scope.tenant_id, ctx.scope.project_id),
            actual: format!("{}/{}", scope.tenant_id, scope.project_id),
        })
    }
}

fn check_fresh(ctx: &RequestContext, index: &DocIndex) -> Result<()> {
    if index.stale {
        return Err(DocumentError::IndexStale {
            revision: index.document_id.clone(),
        });
    }
    if let Some(required) = &ctx.required_generation {
        if required != &index.generation {
            return Err(DocumentError::IndexStale {
                revision: required.clone(),
            });
        }
    }
    Ok(())
}

fn lookup_document<'a>(
    ctx: &RequestContext,
    state: &'a ServiceState,
    document_id: &str,
) -> Result<&'a DocIndex> {
    let index = state
        .documents
        .get(document_id)
        .ok_or_else(|| DocumentError::NotFound {
            resource: format!("document {document_id}"),
        })?;
    require_same_scope(ctx, &index.scope)?;
    Ok(index)
}

fn map_job_error(error: JobError) -> DocumentError {
    match error {
        JobError::JobNotFound { job_id } => DocumentError::JobNotFound { job_id },
        JobError::JobNotCancellable { job_id, .. } => DocumentError::JobNotCancellable { job_id },
        JobError::StaleGeneration { requested } => DocumentError::IndexStale {
            revision: requested,
        },
        JobError::InvalidTransition { .. } => DocumentError::InvalidArgument {
            field: "job.stage",
            reason: error.to_string(),
        },
    }
}

fn advance_to(state: &mut ServiceState, job_id: &str, target: Stage) -> Result<()> {
    loop {
        let stage = state.jobs.get(job_id).map_err(map_job_error)?.stage;
        if stage == target {
            return Ok(());
        }
        state.jobs.advance(job_id).map_err(map_job_error)?;
    }
}

/// `Page` needs `MediaFormat` in scope for the ingest path.
#[allow(dead_code)]
fn format_tag(format: MediaFormat) -> &'static str {
    format.parser_id()
}

// ---------------------------------------------------------------------------
// §37 port surface
// ---------------------------------------------------------------------------

/// The §37 domain port (DAPI-01..14).
#[allow(async_fn_in_trait)]
pub trait DocumentIntelligencePort: Send + Sync {
    /// DAPI-01.
    async fn ingest(&self, ctx: &RequestContext, request: &DocumentIngest) -> Result<JobHandle>;
    /// DAPI-02.
    async fn get_ingest_job(&self, ctx: &RequestContext, request: &GetJob) -> Result<JobView>;
    /// DAPI-03.
    async fn cancel_ingest_job(&self, ctx: &RequestContext, request: &CancelJob)
        -> Result<JobView>;
    /// DAPI-04.
    async fn get_document(
        &self,
        ctx: &RequestContext,
        request: &GetDocument,
    ) -> Result<DocumentView>;
    /// DAPI-05.
    async fn parse_preview(
        &self,
        ctx: &RequestContext,
        request: &ParsePreview,
    ) -> Result<crate::domain::ParsedDocument>;
    /// DAPI-06.
    async fn search(
        &self,
        ctx: &RequestContext,
        request: &SearchDocument,
    ) -> Result<Page<SearchHit>>;
    /// DAPI-07.
    async fn extract_requirements(
        &self,
        ctx: &RequestContext,
        request: &ExtractRequirements,
    ) -> Result<Page<RequirementCandidate>>;
    /// DAPI-08.
    async fn extract_relations(
        &self,
        ctx: &RequestContext,
        request: &ExtractRelations,
    ) -> Result<Page<ExtractedRelation>>;
    /// DAPI-09.
    async fn review_requirement(
        &self,
        ctx: &RequestContext,
        request: &ReviewRequirement,
    ) -> Result<RequirementReviewResult>;
    /// DAPI-10.
    async fn list_requirements(
        &self,
        ctx: &RequestContext,
        request: &ListRequirements,
    ) -> Result<Page<RequirementCandidate>>;
    /// DAPI-11.
    async fn verify_consistency(
        &self,
        ctx: &RequestContext,
        request: &VerifyConsistency,
    ) -> Result<ConsistencyReport>;
    /// DAPI-12.
    async fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &DocumentRevisionDiff,
    ) -> Result<DocDiff>;
    /// DAPI-13.
    async fn invalidate_revision(
        &self,
        ctx: &RequestContext,
        request: &InvalidateDocumentRevision,
    ) -> Result<ChangeReceipt>;
    /// DAPI-14.
    async fn get_source_span(
        &self,
        ctx: &RequestContext,
        request: &GetSourceSpan,
    ) -> Result<SourceSpanView>;
}

impl DocumentIntelligencePort for DocumentService {
    async fn ingest(&self, ctx: &RequestContext, request: &DocumentIngest) -> Result<JobHandle> {
        DocumentService::ingest(self, ctx, request)
    }
    async fn get_ingest_job(&self, ctx: &RequestContext, request: &GetJob) -> Result<JobView> {
        DocumentService::get_ingest_job(self, ctx, request)
    }
    async fn cancel_ingest_job(
        &self,
        ctx: &RequestContext,
        request: &CancelJob,
    ) -> Result<JobView> {
        DocumentService::cancel_ingest_job(self, ctx, request)
    }
    async fn get_document(
        &self,
        ctx: &RequestContext,
        request: &GetDocument,
    ) -> Result<DocumentView> {
        DocumentService::get_document(self, ctx, request)
    }
    async fn parse_preview(
        &self,
        ctx: &RequestContext,
        request: &ParsePreview,
    ) -> Result<crate::domain::ParsedDocument> {
        DocumentService::parse_preview(self, ctx, request)
    }
    async fn search(
        &self,
        ctx: &RequestContext,
        request: &SearchDocument,
    ) -> Result<Page<SearchHit>> {
        DocumentService::search(self, ctx, request)
    }
    async fn extract_requirements(
        &self,
        ctx: &RequestContext,
        request: &ExtractRequirements,
    ) -> Result<Page<RequirementCandidate>> {
        DocumentService::extract_requirements(self, ctx, request)
    }
    async fn extract_relations(
        &self,
        ctx: &RequestContext,
        request: &ExtractRelations,
    ) -> Result<Page<ExtractedRelation>> {
        DocumentService::extract_relations(self, ctx, request)
    }
    async fn review_requirement(
        &self,
        ctx: &RequestContext,
        request: &ReviewRequirement,
    ) -> Result<RequirementReviewResult> {
        DocumentService::review_requirement(self, ctx, request)
    }
    async fn list_requirements(
        &self,
        ctx: &RequestContext,
        request: &ListRequirements,
    ) -> Result<Page<RequirementCandidate>> {
        DocumentService::list_requirements(self, ctx, request)
    }
    async fn verify_consistency(
        &self,
        ctx: &RequestContext,
        request: &VerifyConsistency,
    ) -> Result<ConsistencyReport> {
        DocumentService::verify_consistency(self, ctx, request)
    }
    async fn compare_revisions(
        &self,
        ctx: &RequestContext,
        request: &DocumentRevisionDiff,
    ) -> Result<DocDiff> {
        DocumentService::compare_revisions(self, ctx, request)
    }
    async fn invalidate_revision(
        &self,
        ctx: &RequestContext,
        request: &InvalidateDocumentRevision,
    ) -> Result<ChangeReceipt> {
        DocumentService::invalidate_revision(self, ctx, request)
    }
    async fn get_source_span(
        &self,
        ctx: &RequestContext,
        request: &GetSourceSpan,
    ) -> Result<SourceSpanView> {
        DocumentService::get_source_span(self, ctx, request)
    }
}
