//! 5-path parity matrix for the two CodeGraph-parity document operations
//! (`document.list_doc_sources` = DAPI-15 and `document.generate_architecture_doc`
//! = DAPI-16), mirroring the code parity matrix: valid, invalid input,
//! authorization (FORBIDDEN / cross-scope), capability/version (missing
//! architecture port) and failure, plus tenant isolation.

use std::sync::Arc;

use document_intelligence::domain::Result as DocResult;
use document_intelligence::domain::{
    DocumentError, DocumentIngest, DocumentSource, GenerateArchitectureDoc, ListDocSources,
    PageRequest,
};
use document_intelligence::parsers::DocumentParsers;
use document_intelligence::service::{
    ArchitectureFacts, ArchitectureFactsPort, DocumentService, InMemoryDocuments, SCOPE_ANALYZE,
    SCOPE_INDEX, SCOPE_READ,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const TENANT: &str = "tenant-1";
const PROJECT: &str = "project-1";
const OTHER_TENANT: &str = "tenant-2";

const MARKDOWN: &str = "# Overview\n\nThe system MUST keep evidence.\n";

// ---------------------------------------------------------------------------
// Architecture facts port test doubles
// ---------------------------------------------------------------------------

/// A port that returns well-formed facts.
#[derive(Debug, Default, Clone)]
struct OkFactsPort;

impl ArchitectureFactsPort for OkFactsPort {
    fn facts(&self, _repository_id: &str, _revision: &str) -> DocResult<ArchitectureFacts> {
        Ok(ArchitectureFacts {
            section_titles: vec!["Overview".to_string(), "Modules".to_string()],
            sections: vec![
                "High level overview.".to_string(),
                "module-a, module-b".to_string(),
            ],
        })
    }
}

/// A port whose facts are malformed: the title/section counts do not pair.
#[derive(Debug, Default, Clone)]
struct MismatchedFactsPort;

impl ArchitectureFactsPort for MismatchedFactsPort {
    fn facts(&self, _repository_id: &str, _revision: &str) -> DocResult<ArchitectureFacts> {
        Ok(ArchitectureFacts {
            section_titles: vec!["Only one title".to_string()],
            sections: vec!["section a".to_string(), "section b".to_string()],
        })
    }
}

/// A port that fails to produce facts (e.g. the code graph is unavailable).
#[derive(Debug, Default, Clone)]
struct FailingFactsPort;

impl ArchitectureFactsPort for FailingFactsPort {
    fn facts(&self, _repository_id: &str, _revision: &str) -> DocResult<ArchitectureFacts> {
        Err(DocumentError::NotFound {
            resource: "code graph index".to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn shared_store() -> Arc<InMemoryDocuments> {
    let mut store = InMemoryDocuments::new();
    store.put_text("artifact:1", MARKDOWN);
    Arc::new(store)
}

fn service() -> DocumentService {
    DocumentService::new(Arc::new(DocumentParsers::new()), shared_store())
}

fn ctx(tenant: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-1".to_string(),
        trace_id: "trace-1".to_string(),
        scope: ScopeRef {
            tenant_id: tenant.to_string(),
            project_id: PROJECT.to_string(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "agent-1".to_string(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        deadline_at: None,
        consistency: Consistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

fn ingest(svc: &DocumentService, ctx: &RequestContext) {
    // The service's source port and this share identical content, so recomputing
    // the hash via a fresh store yields the same value the service will verify.
    let mut hash_store = InMemoryDocuments::new();
    let hash = hash_store.put_text("artifact:1", MARKDOWN);
    let source = DocumentSource {
        source_uri: "file:///spec.md".to_string(),
        media_type: "text/markdown".to_string(),
        content_hash: hash,
        revision: "rev-1".to_string(),
        language_hint: Some("en".to_string()),
        artifact_ref: "artifact:1".to_string(),
    };
    let _ = svc
        .ingest(
            ctx,
            &DocumentIngest {
                source,
                parser_profile: "markdown/1".to_string(),
                include_images: true,
                asynchronous: true,
            },
        )
        .expect("ingest");
}

fn is_invalid(error: &DocumentError) -> bool {
    matches!(error, DocumentError::InvalidArgument { .. })
}

fn is_forbidden(error: &DocumentError) -> bool {
    matches!(error, DocumentError::Forbidden { .. })
}

fn is_model_unavailable(error: &DocumentError) -> bool {
    matches!(error, DocumentError::ModelUnavailable { .. })
}

fn architecture_req(title: Option<&str>) -> GenerateArchitectureDoc {
    GenerateArchitectureDoc {
        repository_id: "repo-1".to_string(),
        revision: "abc123".to_string(),
        title: title.map(str::to_string),
    }
}

// ---------------------------------------------------------------------------
// document.list_doc_sources (DAPI-15)
// ---------------------------------------------------------------------------

#[test]
fn list_doc_sources_valid_lists_ingested_documents() {
    let svc = service();
    let ingest_ctx = ctx(TENANT, &[SCOPE_INDEX]);
    ingest(&svc, &ingest_ctx);
    let list_ctx = ctx(TENANT, &[SCOPE_READ]);
    let page = svc
        .list_doc_sources(
            &list_ctx,
            &ListDocSources {
                page: PageRequest::new(50),
            },
        )
        .expect("listing ingested documents succeeds");
    assert!(!page.items.is_empty());
    assert_eq!(page.items[0].media_type, "text/markdown");
}

#[test]
fn list_doc_sources_invalid_when_page_limit_is_zero() {
    let svc = service();
    let ctx = ctx(TENANT, &[SCOPE_READ]);
    let error = svc
        .list_doc_sources(
            &ctx,
            &ListDocSources {
                page: PageRequest::new(0),
            },
        )
        .expect_err("page.limit 0 is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn list_doc_sources_requires_the_read_scope() {
    let svc = service();
    let ctx = ctx(TENANT, &[]);
    let error = svc
        .list_doc_sources(
            &ctx,
            &ListDocSources {
                page: PageRequest::new(50),
            },
        )
        .expect_err("a context without document:read is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn list_doc_sources_is_tenant_isolated() {
    let svc = service();
    let ingest_ctx = ctx(TENANT, &[SCOPE_INDEX]);
    ingest(&svc, &ingest_ctx);
    // A different tenant must not see the document.
    let other_ctx = ctx(OTHER_TENANT, &[SCOPE_READ]);
    let page = svc
        .list_doc_sources(
            &other_ctx,
            &ListDocSources {
                page: PageRequest::new(50),
            },
        )
        .expect("the cross-tenant listing succeeds but is empty");
    assert!(page.items.is_empty());
}

#[test]
fn list_doc_sources_failure_on_stale_cursor() {
    let svc = service();
    let ctx = ctx(TENANT, &[SCOPE_READ]);
    let error = svc
        .list_doc_sources(
            &ctx,
            &ListDocSources {
                page: PageRequest {
                    cursor: Some("1".to_string()),
                    limit: 50,
                },
            },
        )
        .expect_err("an offset beyond the empty result set is a cursor error");
    assert!(matches!(error, DocumentError::CursorExpired { .. }));
}

// ---------------------------------------------------------------------------
// document.generate_architecture_doc (DAPI-16)
// ---------------------------------------------------------------------------

#[test]
fn generate_architecture_doc_valid_renders_markdown() {
    let svc = service().with_architecture_port(Arc::new(OkFactsPort));
    let ctx = ctx(TENANT, &[SCOPE_ANALYZE]);
    let doc = svc
        .generate_architecture_doc(&ctx, &architecture_req(None))
        .expect("a wired facts port renders the document");
    assert!(doc.markdown.starts_with("# Architecture: repo-1\n"));
    assert!(doc.markdown.contains("## Overview"));
    assert_eq!(
        doc.sections,
        vec!["Overview".to_string(), "Modules".to_string()]
    );
    assert_eq!(doc.source, "code_graph");
}

#[test]
fn generate_architecture_doc_invalid_when_repository_is_empty() {
    let svc = service();
    let ctx = ctx(TENANT, &[SCOPE_ANALYZE]);
    let error = svc
        .generate_architecture_doc(
            &ctx,
            &GenerateArchitectureDoc {
                repository_id: "   ".to_string(),
                revision: "abc123".to_string(),
                title: None,
            },
        )
        .expect_err("an empty repository id is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn generate_architecture_doc_requires_the_analyze_scope() {
    let svc = service();
    let ctx = ctx(TENANT, &[]);
    let error = svc
        .generate_architecture_doc(&ctx, &architecture_req(None))
        .expect_err("a context without document:analyze is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn generate_architecture_doc_fails_when_no_facts_port_is_wired() {
    // Capability/version path: without the architecture port the operation
    // degrades to MODEL_UNAVAILABLE rather than fabricating content.
    let svc = service();
    let ctx = ctx(TENANT, &[SCOPE_ANALYZE]);
    let error = svc
        .generate_architecture_doc(&ctx, &architecture_req(None))
        .expect_err("no facts port means no document");
    assert!(is_model_unavailable(&error));
}

#[test]
fn generate_architecture_doc_failure_when_facts_port_errors() {
    let svc = service().with_architecture_port(Arc::new(FailingFactsPort));
    let ctx = ctx(TENANT, &[SCOPE_ANALYZE]);
    let error = svc
        .generate_architecture_doc(&ctx, &architecture_req(None))
        .expect_err("a failing facts port propagates its error");
    assert!(matches!(error, DocumentError::NotFound { .. }));
}

#[test]
fn generate_architecture_doc_rejects_unpaired_facts() {
    let svc = service().with_architecture_port(Arc::new(MismatchedFactsPort));
    let ctx = ctx(TENANT, &[SCOPE_ANALYZE]);
    let error = svc
        .generate_architecture_doc(&ctx, &architecture_req(None))
        .expect_err("section titles must pair 1:1 with sections");
    assert!(is_invalid(&error));
}
