//! Handlers for the 120 domain operations and the 5 system operations.
//!
//! Each handler is a thin adapter: decode the input into the canonical request
//! type, call the domain service with the trusted context, encode the response.
//! No business rule lives here and no operation is dispatched by name matching.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use ukma_contracts::RequestContext;

use embedding_engine::{BackendMode, EmbeddingPort};

use code_intelligence::parsers::engine::UnifiedParser;
use code_intelligence::service::{CodeIntelligenceService, InMemorySources};
use document_intelligence::parsers::DocumentParsers;
use document_intelligence::service::{DocumentService, InMemoryDocuments};
use memory_domain::service::MemoryService;
use traceability_engine::service::TraceabilityService;

use crate::context::OpContext;
use crate::error::{from_domain, DomainFailure, GatewayError};
use crate::gateway::{encode, typed, Gateway, GatewayOutcome};
use crate::manifest;

// ---------------------------------------------------------------------------
// Domain failure adapters
// ---------------------------------------------------------------------------

/// Operation-agnostic retry hint for domain codes that are retryable (§15).
fn retryable_code(code: &str) -> bool {
    matches!(
        code,
        "SOURCE_REVISION_MISMATCH"
            | "INDEX_STALE"
            | "INDEX_NOT_READY"
            | "SOURCE_PARSE_FAILED"
            | "SOURCE_UNAVAILABLE"
            | "CONTEXT_BUDGET_EXCEEDED"
            | "CONFLICTING_EVIDENCE"
            | "STORAGE_COMMIT_UNKNOWN"
            | "STORAGE_UNAVAILABLE"
            | "INDEX_PROJECTION_FAILED"
            | "MODEL_UNAVAILABLE"
            | "TIMEOUT"
            | "INTERNAL"
            | "RESOURCE_EXHAUSTED"
    )
}

impl DomainFailure for code_intelligence::CodeError {
    fn code(&self) -> &'static str {
        code_intelligence::CodeError::code(self)
    }
    fn message(&self) -> String {
        self.to_string()
    }
    fn retryable(&self) -> bool {
        retryable_code(code_intelligence::CodeError::code(self))
    }
}

impl DomainFailure for document_intelligence::DocumentError {
    fn code(&self) -> &'static str {
        document_intelligence::DocumentError::code(self)
    }
    fn message(&self) -> String {
        self.to_string()
    }
    fn retryable(&self) -> bool {
        retryable_code(document_intelligence::DocumentError::code(self))
    }
}

impl DomainFailure for memory_domain::MemoryError {
    fn code(&self) -> &'static str {
        match self {
            // A resource outside the caller's sandbox is reported as absent so a
            // probe cannot tell "yours but off-limits" from "does not exist"
            // (§40.7). Operations that do not declare `NOT_FOUND` are normalised
            // back to `FORBIDDEN` by the gateway.
            memory_domain::MemoryError::CrossScopeDenied { .. } => crate::error::NOT_FOUND,
            other => memory_domain::MemoryError::code(other),
        }
    }
    fn message(&self) -> String {
        self.to_string()
    }
    fn retryable(&self) -> bool {
        memory_domain::MemoryError::retryable(self)
    }
}

impl DomainFailure for traceability_engine::TraceError {
    fn code(&self) -> &'static str {
        traceability_engine::TraceError::code(self)
    }
    fn message(&self) -> String {
        self.to_string()
    }
    fn retryable(&self) -> bool {
        retryable_code(traceability_engine::TraceError::code(self))
    }
}

// ---------------------------------------------------------------------------
// Job index (§36.4 `system.get_job` / `system.cancel_job`)
// ---------------------------------------------------------------------------

/// One job known to the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    /// Job id.
    pub job_id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning project.
    pub project_id: String,
    /// Job kind.
    pub kind: String,
    /// Current stage.
    pub stage: String,
    /// Whether canonical data was committed (a cancel never undoes it).
    pub canonical_committed: bool,
    /// Whether the job may still be cancelled.
    pub cancellable: bool,
    /// Whether a cancel was accepted.
    pub cancelled: bool,
    /// Diagnostics.
    pub diagnostics: Vec<String>,
}

impl JobRecord {
    /// Render the record as a job view.
    pub fn json(&self) -> Value {
        serde_json::json!({
            "job_id": self.job_id,
            "kind": self.kind,
            "stage": if self.cancelled { "cancelled" } else { self.stage.as_str() },
            "canonical_committed": self.canonical_committed,
            "cancellable": self.cancellable,
            "diagnostics": self.diagnostics,
        })
    }
}

/// Shared job index.
#[derive(Debug, Clone, Default)]
pub struct JobIndex {
    jobs: Arc<Mutex<Vec<JobRecord>>>,
    counter: Arc<AtomicU64>,
}

impl JobIndex {
    /// Empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a job handle returned by a domain service.
    pub fn record(&self, mut record: JobRecord) {
        if record.job_id.trim().is_empty() {
            let sequence = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
            record.job_id = format!("gateway-job-{sequence}");
        }
        let mut jobs = self.jobs.lock().expect("job index poisoned");
        if let Some(existing) = jobs.iter_mut().find(|job| job.job_id == record.job_id) {
            *existing = record;
        } else {
            jobs.push(record);
        }
    }

    /// Number of tracked jobs.
    pub fn len(&self) -> usize {
        self.jobs.lock().expect("job index poisoned").len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Look up a job inside a scope.
    pub fn get(&self, tenant_id: &str, project_id: &str, job_id: &str) -> Option<JobRecord> {
        self.jobs
            .lock()
            .expect("job index poisoned")
            .iter()
            .find(|job| {
                job.job_id == job_id && job.tenant_id == tenant_id && job.project_id == project_id
            })
            .cloned()
    }

    /// Cancel a job, reporting the real state when it is too late.
    pub fn cancel(
        &self,
        tenant_id: &str,
        project_id: &str,
        job_id: &str,
    ) -> Result<JobRecord, GatewayError> {
        let mut jobs = self.jobs.lock().expect("job index poisoned");
        let job = jobs
            .iter_mut()
            .find(|job| {
                job.job_id == job_id && job.tenant_id == tenant_id && job.project_id == project_id
            })
            .ok_or_else(|| GatewayError::new("JOB_NOT_FOUND", "job not found", false))?;
        if job.cancelled {
            // Cancelling twice is idempotent: the job is already stopped.
            return Ok(job.clone());
        }
        if !job.cancellable {
            return Err(GatewayError::new(
                "JOB_NOT_CANCELLABLE",
                "job passed its cancellable phase; committed work is kept",
                false,
            ));
        }
        job.cancelled = true;
        job.cancellable = false;
        job.stage = "cancelled".to_string();
        Ok(job.clone())
    }
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

/// The in-process domain services behind the gateway.
#[derive(Clone)]
pub struct Services {
    /// Code intelligence.
    pub code: Arc<CodeIntelligenceService>,
    /// Document intelligence.
    pub document: Arc<DocumentService>,
    /// Fractal memory.
    pub memory: Arc<MemoryService>,
    /// Traceability.
    pub trace: Arc<TraceabilityService>,
    /// Job index shared with the system operations.
    pub jobs: JobIndex,
    /// CodeGraph Pro (27 analysis/security operations).
    pub pro: Arc<code_intelligence::pro::ProService>,
}

impl Services {
    /// Default in-memory wiring (development profile, no persistence).
    pub fn in_memory() -> Self {
        Self {
            code: Arc::new(CodeIntelligenceService::new(
                Arc::new(UnifiedParser::new()),
                Arc::new(InMemorySources::new()),
            )),
            document: Arc::new(DocumentService::new(
                Arc::new(DocumentParsers::new()),
                Arc::new(InMemoryDocuments::new()),
            )),
            memory: Arc::new(MemoryService::new()),
            trace: Arc::new(TraceabilityService::new()),
            jobs: JobIndex::new(),
            pro: Arc::new(code_intelligence::pro::ProService::new()),
        }
    }

    /// Bind the Pro service (e.g. populated from the indexed working tree).
    pub fn with_pro(mut self, pro: Arc<code_intelligence::pro::ProService>) -> Self {
        self.pro = pro;
        self
    }

    /// Populate the Pro corpus from a source snapshot's files for `revision`.
    /// The reference deployment indexes the corpus with the file contents; the
    /// 17 security analyzers are purely line-based, so they operate fully.
    /// Symbol-based code analyzers run over whatever symbols are in the corpus.
    /// `vuln` is the optional OSV online source (`--online`); `None` keeps the
    /// offline default.
    pub fn with_pro_from(
        mut self,
        sources: Arc<dyn code_intelligence::service::SourceSnapshotPort>,
        revision: &str,
        vuln: Option<Arc<dyn code_intelligence::pro::VulnerabilitySource>>,
    ) -> Self {
        let files = sources.files_for(revision).unwrap_or_default();
        let mut pro = code_intelligence::pro::ProService::new();
        if let Some(source) = vuln {
            pro = pro.with_vulnerability_source(source);
        }
        let pro = Arc::new(pro);
        pro.index(revision, files, Vec::new())
            .expect("pro corpus indexing cannot fail for an empty symbol set");
        self.pro = pro;
        self
    }

    /// Like `in_memory`, but the code domain is bound to the given source
    /// snapshots (e.g. an indexed working directory). The document domain is
    /// wired with the code architecture facts port, so
    /// `document.generate_architecture_doc` renders from the same index.
    pub fn with_code_sources(
        sources: Arc<dyn code_intelligence::service::SourceSnapshotPort>,
    ) -> Self {
        let mut services = Self::in_memory();
        let code = Arc::new(CodeIntelligenceService::new(
            Arc::new(UnifiedParser::new()),
            sources,
        ));
        services.code = Arc::clone(&code);
        services.document = Arc::new(
            DocumentService::new(
                Arc::new(DocumentParsers::new()),
                Arc::new(InMemoryDocuments::new()),
            )
            .with_architecture_port(Arc::new(ArchitectureFactsAdapter { code })),
        );
        services
    }

    /// Like `with_code_sources`, but the code domain is built with an
    /// explicit search / embedding policy (CodeGraph `--split-identifiers`,
    /// `--full-body-embedding`, `--graph-only`).
    pub fn with_code_sources_policy(
        sources: Arc<dyn code_intelligence::service::SourceSnapshotPort>,
        policy: code_intelligence::search::SearchPolicy,
    ) -> Self {
        let code = Arc::new(
            CodeIntelligenceService::new(Arc::new(UnifiedParser::new()), sources)
                .with_search_policy(policy),
        );
        let mut services = Self::in_memory();
        services.code = Arc::clone(&code);
        services.document = Arc::new(
            DocumentService::new(
                Arc::new(DocumentParsers::new()),
                Arc::new(InMemoryDocuments::new()),
            )
            .with_architecture_port(Arc::new(ArchitectureFactsAdapter { code })),
        );
        services
    }

    /// Like `with_code_sources_policy`, but the code domain is also bound to
    /// an embedding port + selected profile and the effective mode after the
    /// RAM gate (CodeGraph `--embedding-model` +
    /// `CODEGRAPH_SKIP_MEMORY_CHECK` parity).
    pub fn with_embedding_backend(
        sources: Arc<dyn code_intelligence::service::SourceSnapshotPort>,
        policy: code_intelligence::search::SearchPolicy,
        embedder: Option<(Arc<dyn EmbeddingPort>, String)>,
        backend_mode: BackendMode,
    ) -> Self {
        Self::with_embedding_backend_and_parser(sources, policy, embedder, backend_mode, None)
    }

    /// Like [`Self::with_embedding_backend`], but the parser port is injected
    /// (for example a `CachingParser` over the project's parse cache). `None`
    /// keeps the built-in unified parser.
    pub fn with_embedding_backend_and_parser(
        sources: Arc<dyn code_intelligence::service::SourceSnapshotPort>,
        policy: code_intelligence::search::SearchPolicy,
        embedder: Option<(Arc<dyn EmbeddingPort>, String)>,
        backend_mode: BackendMode,
        parser: Option<Arc<dyn code_intelligence::ParserPort>>,
    ) -> Self {
        let parser: Arc<dyn code_intelligence::ParserPort> =
            parser.unwrap_or_else(|| Arc::new(UnifiedParser::new()));
        let code = Arc::new(
            CodeIntelligenceService::new(parser, sources)
                .with_search_policy(policy)
                .with_embedding_backend(embedder, backend_mode),
        );
        let mut services = Self::in_memory();
        services.code = Arc::clone(&code);
        services.document = Arc::new(
            DocumentService::new(
                Arc::new(DocumentParsers::new()),
                Arc::new(InMemoryDocuments::new()),
            )
            .with_architecture_port(Arc::new(ArchitectureFactsAdapter { code })),
        );
        services
    }
}

/// Adapter: fetch architecture facts from the code service.
struct ArchitectureFactsAdapter {
    code: Arc<CodeIntelligenceService>,
}

impl document_intelligence::service::ArchitectureFactsPort for ArchitectureFactsAdapter {
    fn facts(
        &self,
        repository_id: &str,
        revision: &str,
    ) -> document_intelligence::domain::Result<document_intelligence::service::ArchitectureFacts>
    {
        use code_intelligence::domain::{EntryPointsQuery, HotPathsQuery, ModuleSummaryQuery};
        // Build the facts by calling the code service with a neutral context.
        // The document service checks its own scope; the adapter only renders.
        let ctx_code = ukma_contracts::RequestContext {
            request_id: "architecture-facts".to_string(),
            trace_id: "architecture-facts".to_string(),
            scope: ukma_contracts::ScopeRef {
                tenant_id: "default".to_string(),
                project_id: "default".to_string(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: "architecture-facts".to_string(),
            scopes: vec!["code:read".to_string()],
            deadline_at: None,
            consistency: ukma_contracts::Consistency::ReadYourWrites,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: None,
            expected_etag: None,
            delegation_token: None,
        };
        let modules = self
            .code
            .get_module_summary(
                &ctx_code,
                &ModuleSummaryQuery {
                    repository_id: repository_id.to_string(),
                    revision: revision.to_string(),
                    path_prefix: None,
                    page: code_intelligence::domain::PageRequest::default(),
                },
            )
            .map_err(
                |error| document_intelligence::domain::DocumentError::ModelUnavailable {
                    profile: error.to_string(),
                },
            )?;
        let entry_points = self
            .code
            .find_entry_points(
                &ctx_code,
                &EntryPointsQuery {
                    repository_id: repository_id.to_string(),
                    revision: revision.to_string(),
                    page: code_intelligence::domain::PageRequest::default(),
                },
            )
            .map_err(
                |error| document_intelligence::domain::DocumentError::ModelUnavailable {
                    profile: error.to_string(),
                },
            )?;
        let hot_paths = self
            .code
            .find_hot_paths(
                &ctx_code,
                &HotPathsQuery {
                    repository_id: repository_id.to_string(),
                    revision: revision.to_string(),
                    max_results: 10,
                },
            )
            .map_err(
                |error| document_intelligence::domain::DocumentError::ModelUnavailable {
                    profile: error.to_string(),
                },
            )?;

        let mut section_titles = Vec::new();
        let mut sections = Vec::new();

        // Module summaries.
        let mut module_md = String::new();
        for module in modules.items.iter().take(20) {
            module_md.push_str(&format!(
                "- `{}`: {} files, {} symbols, {} functions\n",
                module.module, module.file_count, module.symbol_count, module.function_count
            ));
            for language in module.languages.iter().take(5) {
                module_md.push_str(&format!("  - {}: {}\n", language.language, language.count));
            }
        }
        if module_md.is_empty() {
            module_md = "No module summaries available.\n".to_string();
        }
        section_titles.push("Modules".to_string());
        sections.push(module_md);

        // Entry points.
        let mut ep_md = String::new();
        for entry in entry_points.items.iter().take(20) {
            ep_md.push_str(&format!(
                "- `{}` ({}) — {}\n",
                entry.name, entry.kind, entry.language
            ));
        }
        if ep_md.is_empty() {
            ep_md = "No entry points detected.\n".to_string();
        }
        section_titles.push("Entry points".to_string());
        sections.push(ep_md);

        // Hot paths.
        let mut hp_md = String::new();
        for hot in hot_paths.iter().take(10) {
            hp_md.push_str(&format!(
                "- `{}`: {} transitive callers ({} direct)\n",
                hot.name, hot.transitive_caller_count, hot.direct_caller_count
            ));
        }
        if hp_md.is_empty() {
            hp_md = "No hot paths detected.\n".to_string();
        }
        section_titles.push("Hot paths".to_string());
        sections.push(hp_md);

        Ok(document_intelligence::service::ArchitectureFacts {
            section_titles,
            sections,
        })
    }
}

/// Register every domain operation plus the system operations.
pub fn build(services: &Services) -> Result<Gateway, String> {
    let mut gateway = Gateway::new();
    register_code(&mut gateway, &services.code, &services.jobs)?;
    register_document(&mut gateway, &services.document, &services.jobs)?;
    register_memory(&mut gateway, &services.memory, &services.jobs)?;
    register_trace(&mut gateway, &services.trace, &services.jobs)?;
    register_system(&mut gateway, &services.jobs)?;
    register_pro(&mut gateway, &services.pro)?;
    Ok(gateway)
}

/// Build a gateway with the default in-memory services.
pub fn default_gateway() -> Gateway {
    build(&Services::in_memory()).expect("the default handler set registers cleanly")
}

macro_rules! bind {
    ($gateway:expr, $op:literal, $service:expr, $method:ident, $request:ty) => {{
        let service = Arc::clone($service);
        $gateway.register(
            $op,
            Arc::new(move |ctx: &OpContext, input: &Value| {
                typed(ctx, input, |request_context, request: $request| {
                    service
                        .$method(request_context, &request)
                        .map_err(from_domain)
                })
            }),
        )?;
    }};
}

/// Bind a Pro operation. Each Pro method takes `(&RequestContext, &str)` for
/// the operation key (`revision`, `file`, or `query`) and returns a JSON
/// payload. The key field is read from the request input.
macro_rules! bind_pro {
    ($gateway:expr, $op:literal, $service:expr, $method:ident, $field:literal) => {{
        let service = Arc::clone($service);
        $gateway.register(
            $op,
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let key = input
                    .get($field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        GatewayError::invalid_schema(concat!($field, " is required"))
                    })?;
                service
                    .$method(&request_context, key)
                    .map_err(from_domain)
                    .map(|payload| GatewayOutcome::data(serde_json::json!({ "report": payload })))
            }),
        )?;
    }};
}

fn job_record(
    request_context: &RequestContext,
    handle: &Value,
    kind: &str,
    cancellable: bool,
) -> JobRecord {
    JobRecord {
        job_id: handle
            .get("job_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tenant_id: request_context.scope.tenant_id.clone(),
        project_id: request_context.scope.project_id.clone(),
        kind: kind.to_string(),
        stage: handle
            .get("stage")
            .and_then(Value::as_str)
            .unwrap_or("queued")
            .to_string(),
        canonical_committed: false,
        cancellable,
        cancelled: false,
        diagnostics: Vec::new(),
    }
}

// -- code -------------------------------------------------------------------

fn register_code(
    gateway: &mut Gateway,
    service: &Arc<CodeIntelligenceService>,
    jobs: &JobIndex,
) -> Result<(), String> {
    use code_intelligence::domain::{
        AiContextQuery, AnalyzeRepository, CancelJob, CircularDepsQuery, CodeRevisionDiff,
        ComplexityQuery, CuratedContextQuery, DeadImportsQuery, EditContextQuery, EntryPointsQuery,
        ErrorSearchQuery, ExportGraphQuery, FindImplementorsQuery, FindImportersQuery,
        GetDetailedSymbolQuery, GetJob, GetSymbol, HotPathsQuery, ImpactQuery, IndexDirectory,
        IndexFiles, InvalidateCodeRevision, ModuleSummaryQuery, PatternSearchQuery, PrContextQuery,
        ReferenceQuery, RelatedTestsQuery, SignatureQuery, SymbolQuery, TraversalQuery,
        TraverseQuery,
    };

    {
        let service = Arc::clone(service);
        let jobs = jobs.clone();
        gateway.register(
            "code.analyze_repository",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let request: AnalyzeRepository = crate::gateway::decode(input)?;
                let handle = service
                    .analyze_repository(&request_context, &request)
                    .map_err(from_domain)?;
                let data = encode(&handle)?;
                jobs.record(job_record(&request_context, &data, "code.analysis", true));
                Ok(GatewayOutcome::data(data))
            }),
        )?;
    }
    bind!(
        gateway,
        "code.get_analysis_job",
        service,
        get_analysis_job,
        GetJob
    );
    bind!(
        gateway,
        "code.cancel_analysis_job",
        service,
        cancel_analysis_job,
        CancelJob
    );
    bind!(gateway, "code.get_symbol", service, get_symbol, GetSymbol);
    bind!(
        gateway,
        "code.find_symbols",
        service,
        find_symbols,
        SymbolQuery
    );
    bind!(
        gateway,
        "code.find_references",
        service,
        find_references,
        ReferenceQuery
    );
    bind!(
        gateway,
        "code.query_callers",
        service,
        query_callers,
        TraversalQuery
    );
    bind!(
        gateway,
        "code.query_callees",
        service,
        query_callees,
        TraversalQuery
    );
    bind!(
        gateway,
        "code.query_dependencies",
        service,
        query_dependencies,
        TraversalQuery
    );
    bind!(
        gateway,
        "code.analyze_impact",
        service,
        analyze_impact,
        ImpactQuery
    );
    bind!(
        gateway,
        "code.get_edit_context",
        service,
        get_edit_context,
        EditContextQuery
    );
    bind!(
        gateway,
        "code.compare_revisions",
        service,
        compare_revisions,
        CodeRevisionDiff
    );
    bind!(
        gateway,
        "code.invalidate_revision",
        service,
        invalidate_revision,
        InvalidateCodeRevision
    );
    // Parity operations (CodeGraph gap closure).
    bind!(
        gateway,
        "code.get_ai_context",
        service,
        get_ai_context,
        AiContextQuery
    );
    bind!(
        gateway,
        "code.get_curated_context",
        service,
        get_curated_context,
        CuratedContextQuery
    );
    bind!(
        gateway,
        "code.analyze_complexity",
        service,
        analyze_complexity,
        ComplexityQuery
    );
    bind!(
        gateway,
        "code.find_circular_deps",
        service,
        find_circular_deps,
        CircularDepsQuery
    );
    bind!(
        gateway,
        "code.find_hot_paths",
        service,
        find_hot_paths,
        HotPathsQuery
    );
    bind!(
        gateway,
        "code.find_dead_imports",
        service,
        find_dead_imports,
        DeadImportsQuery
    );
    bind!(
        gateway,
        "code.get_module_summary",
        service,
        get_module_summary,
        ModuleSummaryQuery
    );
    bind!(
        gateway,
        "code.search_by_pattern",
        service,
        search_by_pattern,
        PatternSearchQuery
    );
    bind!(
        gateway,
        "code.search_by_error",
        service,
        search_by_error,
        ErrorSearchQuery
    );
    bind!(
        gateway,
        "code.find_by_signature",
        service,
        find_by_signature,
        SignatureQuery
    );
    bind!(
        gateway,
        "code.find_entry_points",
        service,
        find_entry_points,
        EntryPointsQuery
    );
    bind!(
        gateway,
        "code.find_related_tests",
        service,
        find_related_tests,
        RelatedTestsQuery
    );
    bind!(
        gateway,
        "code.index_files",
        service,
        index_files,
        IndexFiles
    );
    bind!(
        gateway,
        "code.pr_context",
        service,
        pr_context,
        PrContextQuery
    );
    bind!(
        gateway,
        "code.find_implementors",
        service,
        find_implementors,
        FindImplementorsQuery
    );
    bind!(
        gateway,
        "code.traverse",
        service,
        traverse_graph,
        TraverseQuery
    );
    bind!(
        gateway,
        "code.get_detailed_symbol",
        service,
        get_detailed_symbol,
        GetDetailedSymbolQuery
    );
    bind!(
        gateway,
        "code.index_directory",
        service,
        index_directory,
        IndexDirectory
    );
    bind!(
        gateway,
        "code.export_graph",
        service,
        export_graph,
        ExportGraphQuery
    );
    bind!(
        gateway,
        "code.find_importers",
        service,
        find_importers,
        FindImportersQuery
    );
    Ok(())
}

// -- document ---------------------------------------------------------------

fn register_document(
    gateway: &mut Gateway,
    service: &Arc<DocumentService>,
    jobs: &JobIndex,
) -> Result<(), String> {
    use document_intelligence::domain::{
        CancelJob, DocumentIngest, DocumentRevisionDiff, ExtractRelations, ExtractRequirements,
        GenerateArchitectureDoc, GetDocument, GetJob, GetSourceSpan, InvalidateDocumentRevision,
        ListDocSources, ListRequirements, ParsePreview, RemoveDocSource, ReviewRequirement,
        SearchDocument, VerifyConsistency,
    };

    {
        let service = Arc::clone(service);
        let jobs = jobs.clone();
        gateway.register(
            "document.ingest",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let request: DocumentIngest = crate::gateway::decode(input)?;
                let handle = service
                    .ingest(&request_context, &request)
                    .map_err(from_domain)?;
                let data = encode(&handle)?;
                jobs.record(job_record(&request_context, &data, "document.ingest", true));
                Ok(GatewayOutcome::data(data))
            }),
        )?;
    }
    bind!(
        gateway,
        "document.get_ingest_job",
        service,
        get_ingest_job,
        GetJob
    );
    bind!(
        gateway,
        "document.cancel_ingest_job",
        service,
        cancel_ingest_job,
        CancelJob
    );
    bind!(
        gateway,
        "document.get_document",
        service,
        get_document,
        GetDocument
    );
    bind!(
        gateway,
        "document.parse_preview",
        service,
        parse_preview,
        ParsePreview
    );
    bind!(gateway, "document.search", service, search, SearchDocument);
    bind!(
        gateway,
        "document.extract_requirements",
        service,
        extract_requirements,
        ExtractRequirements
    );
    bind!(
        gateway,
        "document.extract_relations",
        service,
        extract_relations,
        ExtractRelations
    );
    bind!(
        gateway,
        "document.review_requirement",
        service,
        review_requirement,
        ReviewRequirement
    );
    bind!(
        gateway,
        "document.list_requirements",
        service,
        list_requirements,
        ListRequirements
    );
    bind!(
        gateway,
        "document.verify_consistency",
        service,
        verify_consistency,
        VerifyConsistency
    );
    bind!(
        gateway,
        "document.compare_revisions",
        service,
        compare_revisions,
        DocumentRevisionDiff
    );
    bind!(
        gateway,
        "document.invalidate_revision",
        service,
        invalidate_revision,
        InvalidateDocumentRevision
    );
    bind!(
        gateway,
        "document.get_source_span",
        service,
        get_source_span,
        GetSourceSpan
    );
    // Parity operations (CodeGraph gap closure).
    bind!(
        gateway,
        "document.list_doc_sources",
        service,
        list_doc_sources,
        ListDocSources
    );
    bind!(
        gateway,
        "document.generate_architecture_doc",
        service,
        generate_architecture_doc,
        GenerateArchitectureDoc
    );
    bind!(
        gateway,
        "document.remove_source",
        service,
        remove_source,
        RemoveDocSource
    );
    Ok(())
}

// -- memory -----------------------------------------------------------------

fn register_memory(
    gateway: &mut Gateway,
    service: &Arc<MemoryService>,
    jobs: &JobIndex,
) -> Result<(), String> {
    use memory_domain::domain::{
        BeginReconsolidation, CloseAgentScope, CommitReconsolidation, Consolidate,
        EvaluateEviction, FindConflicts, FoldMemory, ForgetMemory, GetMemoryBudget, GetMemoryJob,
        GetMemoryStats, ListMemoryQuery, ListWorkingBlocks, MemoryContextQuery, OpenAgentScope,
        PageIn, PageOut, ProposeFact, PutWorkingBlock, RecordProcedureOutcome, RegisterProcedure,
        RemoveWorkingBlock, RetrieveMemory, ReviewProcedure, SearchMemory, StoreEpisode,
        UpdateMemory, VerifyFact,
    };

    bind!(
        gateway,
        "memory.open_agent_scope",
        service,
        open_agent_scope,
        OpenAgentScope
    );
    bind!(
        gateway,
        "memory.close_agent_scope",
        service,
        close_agent_scope,
        CloseAgentScope
    );
    bind!(
        gateway,
        "memory.put_working_block",
        service,
        put_working_block,
        PutWorkingBlock
    );
    bind!(
        gateway,
        "memory.list_working_blocks",
        service,
        list_working_blocks,
        ListWorkingBlocks
    );
    bind!(
        gateway,
        "memory.remove_working_block",
        service,
        remove_working_block,
        RemoveWorkingBlock
    );
    bind!(
        gateway,
        "memory.store_episode",
        service,
        store_episode,
        StoreEpisode
    );
    bind!(gateway, "memory.search", service, search, SearchMemory);
    bind!(
        gateway,
        "memory.retrieve",
        service,
        retrieve,
        RetrieveMemory
    );
    bind!(
        gateway,
        "memory.update_revision",
        service,
        update_revision,
        UpdateMemory
    );
    bind!(
        gateway,
        "memory.propose_fact",
        service,
        propose_fact,
        ProposeFact
    );
    bind!(
        gateway,
        "memory.verify_fact",
        service,
        verify_fact,
        VerifyFact
    );
    bind!(
        gateway,
        "memory.find_conflicts",
        service,
        find_conflicts,
        FindConflicts
    );
    bind!(gateway, "memory.fold", service, fold, FoldMemory);
    bind!(gateway, "memory.page_out", service, page_out, PageOut);
    bind!(gateway, "memory.page_in", service, page_in, PageIn);
    bind!(
        gateway,
        "memory.begin_reconsolidation",
        service,
        begin_reconsolidation,
        BeginReconsolidation
    );
    bind!(
        gateway,
        "memory.commit_reconsolidation",
        service,
        commit_reconsolidation,
        CommitReconsolidation
    );
    {
        let service = Arc::clone(service);
        let jobs = jobs.clone();
        gateway.register(
            "memory.consolidate",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let request: Consolidate = crate::gateway::decode(input)?;
                let handle = service
                    .consolidate(&request_context, &request)
                    .map_err(from_domain)?;
                let data = encode(&handle)?;
                jobs.record(job_record(
                    &request_context,
                    &data,
                    "memory.consolidation",
                    false,
                ));
                Ok(GatewayOutcome::data(data))
            }),
        )?;
    }
    bind!(
        gateway,
        "memory.get_consolidation_job",
        service,
        get_consolidation_job,
        GetMemoryJob
    );
    bind!(
        gateway,
        "memory.register_procedure_candidate",
        service,
        register_procedure_candidate,
        RegisterProcedure
    );
    bind!(
        gateway,
        "memory.review_procedure",
        service,
        review_procedure,
        ReviewProcedure
    );
    bind!(
        gateway,
        "memory.record_procedure_outcome",
        service,
        record_procedure_outcome,
        RecordProcedureOutcome
    );
    bind!(gateway, "memory.forget", service, forget, ForgetMemory);
    bind!(
        gateway,
        "memory.evaluate_eviction",
        service,
        evaluate_eviction,
        EvaluateEviction
    );
    bind!(
        gateway,
        "memory.get_budget",
        service,
        get_budget,
        GetMemoryBudget
    );
    // Parity operations (CodeGraph gap closure).
    bind!(
        gateway,
        "memory.context",
        service,
        memory_context,
        MemoryContextQuery
    );
    bind!(
        gateway,
        "memory.list",
        service,
        list_memory,
        ListMemoryQuery
    );
    bind!(
        gateway,
        "memory.stats",
        service,
        memory_stats,
        GetMemoryStats
    );
    Ok(())
}

// -- traceability -----------------------------------------------------------

fn register_trace(
    gateway: &mut Gateway,
    service: &Arc<TraceabilityService>,
    jobs: &JobIndex,
) -> Result<(), String> {
    use traceability_engine::domain::{
        AcceptLink, CoverageQuery, ExplainEvidence, GapQuery, GetLink, GetTraceJob, ListLinks,
        ProposeLink, RevalidateStale, RevokeLink, TraceImpactQuery, TraceRevisionDiff,
        ValidateLink,
    };

    bind!(
        gateway,
        "traceability.propose_link",
        service,
        propose_link,
        ProposeLink
    );
    bind!(
        gateway,
        "traceability.validate_link",
        service,
        validate_link,
        ValidateLink
    );
    bind!(
        gateway,
        "traceability.accept_link",
        service,
        accept_link,
        AcceptLink
    );
    bind!(
        gateway,
        "traceability.revoke_link",
        service,
        revoke_link,
        RevokeLink
    );
    bind!(gateway, "traceability.get_link", service, get_link, GetLink);
    bind!(
        gateway,
        "traceability.list_links",
        service,
        list_links,
        ListLinks
    );
    bind!(
        gateway,
        "traceability.query_coverage",
        service,
        query_coverage,
        CoverageQuery
    );
    bind!(
        gateway,
        "traceability.analyze_change_impact",
        service,
        analyze_change_impact,
        TraceImpactQuery
    );
    bind!(
        gateway,
        "traceability.find_design_gaps",
        service,
        find_design_gaps,
        GapQuery
    );
    bind!(
        gateway,
        "traceability.compare_revisions",
        service,
        compare_revisions,
        TraceRevisionDiff
    );
    bind!(
        gateway,
        "traceability.explain_with_evidence",
        service,
        explain_with_evidence,
        ExplainEvidence
    );
    {
        let service = Arc::clone(service);
        let jobs = jobs.clone();
        gateway.register(
            "traceability.revalidate_stale",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let request: RevalidateStale = crate::gateway::decode(input)?;
                let handle = service
                    .revalidate_stale(&request_context, &request)
                    .map_err(from_domain)?;
                let data = encode(&handle)?;
                jobs.record(job_record(
                    &request_context,
                    &data,
                    "traceability.revalidation",
                    false,
                ));
                Ok(GatewayOutcome::data(data))
            }),
        )?;
    }
    bind!(
        gateway,
        "traceability.get_revalidation_job",
        service,
        get_revalidation_job,
        GetTraceJob
    );
    Ok(())
}

// -- system -----------------------------------------------------------------

fn register_system(gateway: &mut Gateway, jobs: &JobIndex) -> Result<(), String> {
    {
        let metrics = gateway.metrics().clone();
        gateway.register(
            "system.metrics",
            Arc::new(move |_ctx: &OpContext, _input: &Value| {
                let per_operation = metrics.snapshot();
                let total_success: u64 = per_operation.iter().map(|entry| entry.success).sum();
                let total_failure: u64 = per_operation.iter().map(|entry| entry.failure).sum();
                Ok(GatewayOutcome::data(serde_json::json!({
                    "total_success": total_success,
                    "total_failure": total_failure,
                    "per_operation": per_operation,
                })))
            }),
        )?;
    }
    gateway.register(
        "system.capabilities",
        Arc::new(move |_ctx: &OpContext, _input: &Value| {
            let tools = manifest::tools();
            let domain_tools = tools.iter().filter(|tool| !tool.system).count();
            let system_tools = tools.iter().filter(|tool| tool.system).count();
            Ok(GatewayOutcome::data(serde_json::json!({
                "spec_version": "1.2.3",
                "api_major": 1,
                "domains": {
                    "code": tools.iter().filter(|tool| tool.domain == "code").count(),
                    "document": tools.iter().filter(|tool| tool.domain == "document").count(),
                    "memory": tools.iter().filter(|tool| tool.domain == "memory").count(),
                    "traceability": tools.iter().filter(|tool| tool.domain == "traceability").count(),
                    "pro": tools.iter().filter(|tool| tool.domain == "pro").count(),
                },
                "domain_operations": domain_tools,
                "system_operations": system_tools,
                "schemas": "preliminary (JSON Schemas are not claimed closed)",
            })))
        }),
    )?;

    gateway.register(
        "system.get_operation",
        Arc::new(move |_ctx: &OpContext, input: &Value| {
            let operation_id = input
                .get("operation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| GatewayError::invalid_schema("operation_id is required"))?;
            let spec = ukma_contracts::generated::OPERATIONS
                .iter()
                .find(|spec| spec.operation_id == operation_id)
                .ok_or_else(|| GatewayError::not_found("operation"))?;
            Ok(GatewayOutcome::data(serde_json::json!({
                "operation_id": spec.operation_id,
                "api_major": spec.api_major,
                "operation_uri": spec.operation_uri,
                "request_schema": spec.request_schema,
                "response_schema": spec.response_schema,
                "required_scopes": spec.required_scopes,
                "error_codes": spec.error_codes,
                "events": spec.events,
                "mcp_tool": spec.mcp_tool,
                "rest": spec.server_method,
                "tests": spec.tests,
            })))
        }),
    )?;

    {
        let jobs = jobs.clone();
        gateway.register(
            "system.get_job",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let job_id = input
                    .get("job_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::invalid_schema("job_id is required"))?;
                let record = jobs
                    .get(&ctx.scope.tenant_id, &ctx.scope.project_id, job_id)
                    .ok_or_else(|| GatewayError::new("JOB_NOT_FOUND", "job not found", false))?;
                Ok(GatewayOutcome::data(record.json()))
            }),
        )?;
    }

    {
        let jobs = jobs.clone();
        gateway.register(
            "system.cancel_job",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let job_id = input
                    .get("job_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::invalid_schema("job_id is required"))?;
                let record = jobs.cancel(&ctx.scope.tenant_id, &ctx.scope.project_id, job_id)?;
                Ok(GatewayOutcome::data(record.json()).with_warning(
                    "cancellation stops further work; committed canonical data is kept",
                ))
            }),
        )?;
    }
    Ok(())
}

// -- pro --------------------------------------------------------------------

/// Register the 27 CodeGraph Pro operations (10 code analysis + 17 security).
/// Each operation reads its key from the request input (`revision`, `file`, or
/// `query`) and returns the detector's JSON payload.
fn register_pro(
    gateway: &mut Gateway,
    service: &Arc<code_intelligence::pro::ProService>,
) -> Result<(), String> {
    // 10 code analysis operations.
    bind_pro!(
        gateway,
        "pro.analyze_coupling",
        service,
        analyze_coupling,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.find_unused_code",
        service,
        find_unused_code,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.find_duplicates",
        service,
        find_duplicates,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.find_similar",
        service,
        find_similar,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.cluster_symbols",
        service,
        cluster_symbols,
        "revision"
    );
    {
        let service = Arc::clone(service);
        gateway.register(
            "pro.compare_symbols",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let revision = input
                    .get("revision")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::invalid_schema("revision is required"))?;
                let a = input
                    .get("a")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::invalid_schema("a is required"))?;
                let b = input
                    .get("b")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::invalid_schema("b is required"))?;
                service
                    .compare_symbols(&request_context, revision, a, b)
                    .map_err(from_domain)
                    .map(|payload| GatewayOutcome::data(serde_json::json!({ "report": payload })))
            }),
        )?;
    }
    bind_pro!(
        gateway,
        "pro.cross_project_search",
        service,
        cross_project_search,
        "query"
    );
    bind_pro!(
        gateway,
        "pro.mine_git_history",
        service,
        mine_git_history,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.mine_git_history_for_file",
        service,
        mine_git_history_for_file,
        "file"
    );
    bind_pro!(
        gateway,
        "pro.search_git_history",
        service,
        search_git_history,
        "query"
    );

    // 17 security operations.
    bind_pro!(
        gateway,
        "pro.scan_security",
        service,
        scan_security,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_control_flow",
        service,
        security_control_flow,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_trace_data_flow",
        service,
        security_trace_data_flow,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_generate_sbom",
        service,
        security_generate_sbom,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_audit_deps",
        service,
        security_audit_deps,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_unchecked_returns",
        service,
        security_check_unchecked_returns,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_resource_leaks",
        service,
        security_check_resource_leaks,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_misconfig",
        service,
        security_check_misconfig,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_input_validation",
        service,
        security_check_input_validation,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_error_exposure",
        service,
        security_check_error_exposure,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_scan_iac",
        service,
        security_scan_iac,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_licenses",
        service,
        security_check_licenses,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_secrets_entropy",
        service,
        security_check_secrets_entropy,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_detect_injection",
        service,
        security_detect_injection,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_search_path",
        service,
        security_check_search_path,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_check_crypto",
        service,
        security_check_crypto,
        "revision"
    );
    bind_pro!(
        gateway,
        "pro.security_export_sarif",
        service,
        security_export_sarif,
        "revision"
    );
    // `pro.security_orchestrated_scan` — multi-field (manual binding).
    {
        let service = Arc::clone(service);
        gateway.register(
            "pro.security_orchestrated_scan",
            Arc::new(move |ctx: &OpContext, input: &Value| {
                let request_context = ctx.to_request_context();
                let revision = input
                    .get("revision")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::invalid_schema("revision is required"))?;
                let engines: Vec<String> = input
                    .get("engines")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let severity = input.get("severity").and_then(Value::as_str);
                let path_filter = input.get("path_filter").and_then(Value::as_str);
                service
                    .security_orchestrated_scan(
                        &request_context,
                        revision,
                        &engines,
                        severity,
                        path_filter,
                    )
                    .map_err(from_domain)
                    .map(|payload| GatewayOutcome::data(serde_json::json!({ "report": payload })))
            }),
        )?;
    }
    bind_pro!(
        gateway,
        "pro.security_codeql_warmup",
        service,
        security_codeql_warmup,
        "revision"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_operation_has_a_handler() {
        let gateway = default_gateway();
        assert_eq!(gateway.unhandled(), Vec::<&str>::new());
        assert_eq!(
            gateway.registered().len(),
            manifest::DOMAIN_OPERATION_COUNT + manifest::SYSTEM_OPERATION_COUNT
        );
    }

    #[test]
    fn the_job_index_is_scoped_and_cancellable_only_once() {
        let index = JobIndex::new();
        index.record(JobRecord {
            job_id: "job-1".into(),
            tenant_id: "tenant-a".into(),
            project_id: "project-b".into(),
            kind: "document.ingest".into(),
            stage: "ready".into(),
            canonical_committed: true,
            cancellable: true,
            cancelled: false,
            diagnostics: vec![],
        });
        assert_eq!(index.len(), 1);
        assert!(index.get("tenant-z", "project-b", "job-1").is_none());
        assert!(index.get("tenant-a", "project-b", "job-1").is_some());

        let cancelled = index.cancel("tenant-a", "project-b", "job-1").unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled.canonical_committed);
        assert_eq!(
            index
                .cancel("tenant-a", "project-b", "nope")
                .unwrap_err()
                .code,
            "JOB_NOT_FOUND"
        );

        let mut finished = JobRecord {
            job_id: "job-2".into(),
            tenant_id: "tenant-a".into(),
            project_id: "project-b".into(),
            kind: "memory.consolidation".into(),
            stage: "committed".into(),
            canonical_committed: true,
            cancellable: false,
            cancelled: false,
            diagnostics: vec![],
        };
        index.record(finished.clone());
        assert_eq!(
            index
                .cancel("tenant-a", "project-b", "job-2")
                .unwrap_err()
                .code,
            "JOB_NOT_CANCELLABLE"
        );
        finished.cancelled = false;
    }

    #[test]
    fn an_empty_job_id_is_replaced_with_a_generated_one() {
        let index = JobIndex::new();
        index.record(JobRecord {
            job_id: String::new(),
            tenant_id: "tenant-a".into(),
            project_id: "project-b".into(),
            kind: "k".into(),
            stage: "queued".into(),
            canonical_committed: false,
            cancellable: true,
            cancelled: false,
            diagnostics: vec![],
        });
        assert!(!index.is_empty());
    }

    #[test]
    fn retryable_codes_follow_the_error_registry() {
        assert!(retryable_code("INDEX_STALE"));
        assert!(retryable_code("STORAGE_COMMIT_UNKNOWN"));
        assert!(!retryable_code("FORBIDDEN"));
        assert!(!retryable_code("ETAG_MISMATCH"));
    }
}
