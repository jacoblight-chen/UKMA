//! DAPI-01..14 contract matrix (`P4-WP3`..`P4-WP6`).
//!
//! Every operation is exercised with the sub-cases §41.4 asks for where they
//! apply: valid, invalid input, cross-scope, FORBIDDEN, stale index, missing
//! evidence, idempotent retry, capability missing and pagination.

use std::sync::Arc;

use document_intelligence::domain::{
    BlockKind, CancelJob, ConsistencyCategory, DocChangeKind, DocumentError, DocumentIngest,
    DocumentRevisionDiff, DocumentSource, ExtractRelations, ExtractRequirements, GetDocument,
    GetJob, GetSourceSpan, InvalidateDocumentRevision, ListRequirements, NormativeStrength,
    PageRequest, ParsePreview, ParseStatus, RequirementStatus, ReviewDecision, ReviewRequirement,
    SearchDocument, Span, ValidationState, VerifyConsistency,
};
use document_intelligence::parsers::DocumentParsers;
use document_intelligence::service::{
    DocumentAuthorizer, DocumentService, DocumentSourcePort, InMemoryDocuments, ScopeAuthorizer,
    EXTRACTOR_PROFILE, SCOPE_ANALYZE, SCOPE_INDEX, SCOPE_INDEX_ADMIN, SCOPE_READ, SCOPE_REVIEW,
};
use ukma_contracts::{Consistency as WireConsistency, RequestContext, ScopeRef};

const TENANT: &str = "tenant-1";
const PROJECT: &str = "project-1";

const REV_1: &str = "\
# Overview

The system MUST keep evidence for every claim. REQ-12 applies to all modules.

A backup SHOULD run nightly.

Operators MAY disable alerts.

See [the appendix](#appendix) and [a missing section](#nope).

![diagram](diagram.png)

Appendix
========

Final words.
";

const REV_2: &str = "\
# Overview

The system MUST keep evidence for every claim. REV-12 applies to all modules.

A backup SHOULD run nightly.

Operators MAY disable alerts, etc.

See [the appendix](#appendix).

Appendix
========

Final words, revised.
";

fn docs() -> InMemoryDocuments {
    let mut store = InMemoryDocuments::new();
    store.put_text("artifact:rev-1", REV_1);
    store.put_text("artifact:rev-2", REV_2);
    store.put_text(
        "artifact:other",
        "# Other\n\nThe service MUST log every request.\n",
    );
    store
}

fn service() -> (DocumentService, InMemoryDocuments) {
    let store = docs();
    let svc = DocumentService::new(Arc::new(DocumentParsers::new()), Arc::new(docs()));
    (svc, store)
}

fn base_ctx(scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-1".to_string(),
        trace_id: "trace-1".to_string(),
        scope: ScopeRef {
            tenant_id: TENANT.to_string(),
            project_id: PROJECT.to_string(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "agent-1".to_string(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        deadline_at: None,
        consistency: WireConsistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

fn full_ctx() -> RequestContext {
    base_ctx(&[
        SCOPE_READ,
        SCOPE_INDEX,
        SCOPE_ANALYZE,
        SCOPE_REVIEW,
        SCOPE_INDEX_ADMIN,
    ])
}

fn source(artifact_ref: &str, revision: &str) -> DocumentSource {
    let store = docs();
    let bytes = store
        .read(&DocumentSource {
            source_uri: "file:///spec.md".to_string(),
            media_type: "text/markdown".to_string(),
            content_hash: String::new(),
            revision: revision.to_string(),
            language_hint: None,
            artifact_ref: artifact_ref.to_string(),
        })
        .expect("artifact");
    let hash = format!("sha256:{}", {
        use sha2::{Digest, Sha256};
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    });
    DocumentSource {
        source_uri: "file:///spec.md".to_string(),
        media_type: "text/markdown".to_string(),
        content_hash: hash,
        revision: revision.to_string(),
        language_hint: Some("en".to_string()),
        artifact_ref: artifact_ref.to_string(),
    }
}

fn ingest_request(artifact_ref: &str, revision: &str) -> DocumentIngest {
    DocumentIngest {
        source: source(artifact_ref, revision),
        parser_profile: "markdown/1".to_string(),
        include_images: true,
        asynchronous: true,
    }
}

fn ingest(
    svc: &DocumentService,
    ctx: &RequestContext,
    artifact_ref: &str,
    revision: &str,
) -> String {
    let handle = svc
        .ingest(ctx, &ingest_request(artifact_ref, revision))
        .expect("ingest");
    handle.job_id
}

fn document_id(svc: &DocumentService, ctx: &RequestContext, job_id: &str) -> String {
    svc.get_ingest_job(
        ctx,
        &GetJob {
            job_id: job_id.to_string(),
        },
    )
    .expect("job")
    .result_ref
    .expect("committed job exposes the document")
}

// ---------------------------------------------------------------------------
// DAPI-01 document.ingest
// ---------------------------------------------------------------------------

#[test]
fn dapi01_ingest_commits_a_parsed_document() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let view = svc
        .get_ingest_job(
            &ctx,
            &GetJob {
                job_id: job_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(view.stage, "ready");
    assert!(view.canonical_committed);
    assert!(view.partial_ratio.unwrap_or(0) > 0, "the image is unparsed");

    let document = view.result_ref.unwrap();
    let manifest = svc.document_manifest(&ctx, &document).unwrap();
    assert_eq!(manifest.parser_id, "ukma-markdown");
    assert_eq!(manifest.completeness, "partial");
    assert!(manifest
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "OCR_NOT_IMPLEMENTED"));
}

#[test]
fn dapi01_rejects_unsupported_media_types_and_sync_requests() {
    let (svc, _) = service();
    let ctx = full_ctx();

    let mut request = ingest_request("artifact:rev-1", "rev-1");
    request.source.media_type = "application/msword".to_string();
    assert_eq!(
        svc.ingest(&ctx, &request).unwrap_err().code(),
        "UNSUPPORTED_MEDIA_TYPE"
    );

    let mut request = ingest_request("artifact:rev-1", "rev-1");
    request.asynchronous = false;
    let error = svc.ingest(&ctx, &request).unwrap_err();
    assert_eq!(error.code(), "INVALID_ARGUMENT");
    // The wire field is `asynchronous`; the hint must name it exactly.
    let message = error.to_string();
    assert!(message.contains("`asynchronous: true`"), "{message}");
    assert!(!message.contains("`async: true`"), "{message}");

    let mut request = ingest_request("artifact:rev-1", "rev-1");
    request.parser_profile = "  ".to_string();
    assert_eq!(
        svc.ingest(&ctx, &request).unwrap_err().code(),
        "INVALID_ARGUMENT"
    );
}

#[test]
fn dapi01_refuses_an_artifact_whose_hash_does_not_match() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let mut request = ingest_request("artifact:rev-1", "rev-1");
    request.source.content_hash = format!("sha256:{}", "0".repeat(64));
    assert_eq!(
        svc.ingest(&ctx, &request).unwrap_err().code(),
        "SOURCE_UNAVAILABLE"
    );

    let mut request = ingest_request("artifact:rev-1", "rev-1");
    request.source.artifact_ref = "artifact:missing".to_string();
    request.source.content_hash = String::new();
    assert_eq!(
        svc.ingest(&ctx, &request).unwrap_err().code(),
        "SOURCE_UNAVAILABLE"
    );
}

#[test]
fn dapi01_requires_the_index_scope_and_dedups_retries() {
    let (svc, _) = service();
    let read_only = base_ctx(&[SCOPE_READ]);
    assert_eq!(
        svc.ingest(&read_only, &ingest_request("artifact:rev-1", "rev-1"))
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );

    let ctx = full_ctx();
    let first = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let second = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    assert_eq!(first, second, "an identical request joins the existing job");

    let mut keyed = full_ctx();
    keyed.idempotency_key = Some("key-1".to_string());
    let a = ingest(&svc, &keyed, "artifact:other", "rev-1");
    let b = ingest(&svc, &keyed, "artifact:other", "rev-1");
    assert_eq!(a, b);
    let mut conflicting = ingest_request("artifact:other", "rev-2");
    conflicting.parser_profile = "markdown/2".to_string();
    assert_eq!(
        svc.ingest(&keyed, &conflicting).unwrap_err().code(),
        "IDEMPOTENCY_CONFLICT"
    );
}

// ---------------------------------------------------------------------------
// DAPI-02 / DAPI-03
// ---------------------------------------------------------------------------

#[test]
fn dapi02_unknown_job_and_missing_scope() {
    let (svc, _) = service();
    assert_eq!(
        svc.get_ingest_job(
            &full_ctx(),
            &GetJob {
                job_id: "job-doc-999".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "JOB_NOT_FOUND"
    );
    let job_id = ingest(&svc, &full_ctx(), "artifact:rev-1", "rev-1");
    assert_eq!(
        svc.get_ingest_job(&base_ctx(&[SCOPE_INDEX]), &GetJob { job_id })
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

#[test]
fn dapi03_cancel_before_commit_wins_after_commit_is_refused() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let handle = svc
        .submit_ingest(&ctx, &ingest_request("artifact:rev-1", "rev-1"))
        .unwrap();
    assert_eq!(handle.stage, "queued");
    let cancelled = svc
        .cancel_ingest_job(
            &ctx,
            &CancelJob {
                job_id: handle.job_id.clone(),
                reason: "user abort".to_string(),
            },
        )
        .unwrap();
    assert_eq!(cancelled.stage, "cancelled");
    let view = svc.run_ingest(&ctx, &handle.job_id).unwrap();
    assert_eq!(
        view.stage, "cancelled",
        "a cancelled job is not resurrected"
    );

    let done = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    assert_eq!(
        svc.cancel_ingest_job(
            &ctx,
            &CancelJob {
                job_id: done.clone(),
                reason: "too late".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "JOB_NOT_CANCELLABLE"
    );
    assert!(
        svc.get_ingest_job(&ctx, &GetJob { job_id: done })
            .unwrap()
            .canonical_committed
    );
}

// ---------------------------------------------------------------------------
// DAPI-04
// ---------------------------------------------------------------------------

#[test]
fn dapi04_get_document_pages_blocks_and_hides_them_on_request() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));

    let view = svc
        .get_document(
            &ctx,
            &GetDocument {
                document_id: document.clone(),
                include_blocks: true,
                page: PageRequest::new(3),
            },
        )
        .unwrap();
    assert_eq!(view.blocks.items.len(), 3);
    assert!(view.blocks.next_cursor.is_some());
    assert_eq!(view.completeness, "partial");

    let without = svc
        .get_document(
            &ctx,
            &GetDocument {
                document_id: document.clone(),
                include_blocks: false,
                page: PageRequest::new(3),
            },
        )
        .unwrap();
    assert!(without.blocks.items.is_empty());

    assert_eq!(
        svc.get_document(
            &ctx,
            &GetDocument {
                document_id: "doc:v1:missing".to_string(),
                include_blocks: true,
                page: PageRequest::default(),
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
    let mut other = full_ctx();
    other.scope.tenant_id = "tenant-2".to_string();
    assert_eq!(
        svc.get_document(
            &other,
            &GetDocument {
                document_id: document,
                include_blocks: true,
                page: PageRequest::default(),
            }
        )
        .unwrap_err()
        .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// ---------------------------------------------------------------------------
// DAPI-05
// ---------------------------------------------------------------------------

#[test]
fn dapi05_parse_preview_persists_nothing() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let parsed = svc
        .parse_preview(
            &ctx,
            &ParsePreview {
                source: source("artifact:rev-1", "rev-1"),
                parser_profile: "markdown/1".to_string(),
                max_bytes: 1024 * 1024,
            },
        )
        .unwrap();
    assert!(!parsed.blocks.is_empty());
    assert_eq!(parsed.completeness.as_str(), "partial");
    // nothing was committed
    assert_eq!(
        svc.get_document(
            &ctx,
            &GetDocument {
                document_id: parsed.document_id,
                include_blocks: true,
                page: PageRequest::default(),
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
    assert_eq!(
        svc.parse_preview(
            &base_ctx(&[SCOPE_READ]),
            &ParsePreview {
                source: source("artifact:rev-1", "rev-1"),
                parser_profile: "markdown/1".to_string(),
                max_bytes: 1024,
            }
        )
        .unwrap_err()
        .code(),
        "FORBIDDEN"
    );
}

// ---------------------------------------------------------------------------
// DAPI-06
// ---------------------------------------------------------------------------

#[test]
fn dapi06_search_matches_terms_and_respects_scope_and_kinds() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&svc, &ctx, &job);

    let hits = svc
        .search(
            &ctx,
            &SearchDocument {
                tenant_id: TENANT.to_string(),
                project_id: PROJECT.to_string(),
                query: "evidence".to_string(),
                document_refs: vec![document.clone()],
                languages: Vec::new(),
                block_kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(hits.items.len(), 1);
    assert!(hits.items[0].snippet.contains("evidence"));
    assert_eq!(hits.items[0].score, 100);

    let headings_only = svc
        .search(
            &ctx,
            &SearchDocument {
                tenant_id: TENANT.to_string(),
                project_id: PROJECT.to_string(),
                query: "overview".to_string(),
                document_refs: Vec::new(),
                languages: Vec::new(),
                block_kinds: vec![BlockKind::Heading],
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(headings_only.items.len(), 1);
    assert_eq!(headings_only.items[0].kind, BlockKind::Heading);

    let mut other = full_ctx();
    other.scope.project_id = "project-2".to_string();
    assert_eq!(
        svc.search(
            &other,
            &SearchDocument {
                tenant_id: TENANT.to_string(),
                project_id: PROJECT.to_string(),
                query: "evidence".to_string(),
                document_refs: Vec::new(),
                languages: Vec::new(),
                block_kinds: Vec::new(),
                page: PageRequest::new(10),
            }
        )
        .unwrap_err()
        .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// ---------------------------------------------------------------------------
// DAPI-07 / DAPI-08
// ---------------------------------------------------------------------------

#[test]
fn dapi07_requirements_are_candidates_with_a_strength_and_id() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));

    let page = svc
        .extract_requirements(
            &ctx,
            &ExtractRequirements {
                document_id: document.clone(),
                section_refs: Vec::new(),
                extractor_profile: EXTRACTOR_PROFILE.to_string(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert!(page.items.len() >= 3, "candidates: {:#?}", page.items);
    for candidate in &page.items {
        assert_eq!(candidate.status, RequirementStatus::Candidate);
        assert_eq!(candidate.verification, ValidationState::Unverified);
        assert!(!candidate.extracted_from.is_empty());
        assert_ne!(candidate.normative_strength, NormativeStrength::Unknown);
    }
    let strengths: Vec<NormativeStrength> = page
        .items
        .iter()
        .map(|candidate| candidate.normative_strength)
        .collect();
    assert!(strengths.contains(&NormativeStrength::Must));
    assert!(strengths.contains(&NormativeStrength::Should));
    assert!(strengths.contains(&NormativeStrength::May));
    assert!(page
        .items
        .iter()
        .any(|candidate| candidate.requirement_id.as_deref() == Some("REQ-12")));

    assert_eq!(
        svc.extract_requirements(
            &ctx,
            &ExtractRequirements {
                document_id: document,
                section_refs: Vec::new(),
                extractor_profile: "llm/9".to_string(),
                page: PageRequest::default(),
            }
        )
        .unwrap_err()
        .code(),
        "MODEL_UNAVAILABLE"
    );
}

#[test]
fn dapi08_relations_resolve_anchors_and_refuse_cross_scope_targets() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));

    let relations = svc
        .extract_relations(
            &ctx,
            &ExtractRelations {
                document_id: document.clone(),
                target_tenant_id: TENANT.to_string(),
                target_project_id: PROJECT.to_string(),
                relation_kinds: vec!["references".to_string()],
                extractor_profile: EXTRACTOR_PROFILE.to_string(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    // the fixture links to #appendix (resolvable) and #nope (not a relation)
    assert_eq!(relations.items.len(), 1, "{:#?}", relations.items);
    assert_eq!(relations.items[0].relation_kind, "references");
    assert_eq!(relations.items[0].verification, ValidationState::Unverified);

    assert_eq!(
        svc.extract_relations(
            &ctx,
            &ExtractRelations {
                document_id: document,
                target_tenant_id: "tenant-2".to_string(),
                target_project_id: PROJECT.to_string(),
                relation_kinds: Vec::new(),
                extractor_profile: EXTRACTOR_PROFILE.to_string(),
                page: PageRequest::default(),
            }
        )
        .unwrap_err()
        .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// ---------------------------------------------------------------------------
// DAPI-09 / DAPI-10
// ---------------------------------------------------------------------------

fn first_candidate(svc: &DocumentService, ctx: &RequestContext, document: &str) -> String {
    svc.extract_requirements(
        ctx,
        &ExtractRequirements {
            document_id: document.to_string(),
            section_refs: Vec::new(),
            extractor_profile: EXTRACTOR_PROFILE.to_string(),
            page: PageRequest::new(50),
        },
    )
    .unwrap()
    .items[0]
        .id
        .clone()
}

#[test]
fn dapi09_review_needs_evidence_and_a_matching_etag() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));
    let candidate = first_candidate(&svc, &ctx, &document);

    assert_eq!(
        svc.review_requirement(
            &ctx,
            &ReviewRequirement {
                candidate_id: candidate.clone(),
                decision: ReviewDecision::Approve,
                rationale: "matches the spec".to_string(),
                evidence_ids: Vec::new(),
                expected_etag: "\"x\"".to_string(),
            }
        )
        .unwrap_err()
        .code(),
        "EVIDENCE_REQUIRED"
    );

    assert_eq!(
        svc.review_requirement(
            &ctx,
            &ReviewRequirement {
                candidate_id: candidate.clone(),
                decision: ReviewDecision::Approve,
                rationale: "matches the spec".to_string(),
                evidence_ids: vec!["ev-1".to_string()],
                expected_etag: "\"wrong\"".to_string(),
            }
        )
        .unwrap_err()
        .code(),
        "ETAG_MISMATCH"
    );

    let etag = svc
        .list_requirements(
            &ctx,
            &ListRequirements {
                document_id: document.clone(),
                statuses: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.id == candidate)
        .unwrap()
        .etag;

    let result = svc
        .review_requirement(
            &ctx,
            &ReviewRequirement {
                candidate_id: candidate.clone(),
                decision: ReviewDecision::Approve,
                rationale: "matches the spec".to_string(),
                evidence_ids: vec!["ev-1".to_string()],
                expected_etag: etag,
            },
        )
        .unwrap();
    assert_eq!(result.requirement.status, RequirementStatus::Approved);
    assert_eq!(result.requirement.verification, ValidationState::Verified);
    assert_eq!(
        result.receipt.events,
        vec!["document.requirement.reviewed".to_string()]
    );

    assert_eq!(
        svc.review_requirement(
            &ctx,
            &ReviewRequirement {
                candidate_id: candidate,
                decision: ReviewDecision::Reject,
                rationale: "changed my mind".to_string(),
                evidence_ids: vec!["ev-2".to_string()],
                expected_etag: result.requirement.etag.clone(),
            }
        )
        .unwrap_err()
        .code(),
        "LINK_STATE_CONFLICT",
        "an approved requirement only changes with a new revision"
    );
}

#[test]
fn dapi10_list_requirements_filters_by_status() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));
    let all = svc
        .list_requirements(
            &ctx,
            &ListRequirements {
                document_id: document.clone(),
                statuses: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert!(!all.items.is_empty());
    let approved = svc
        .list_requirements(
            &ctx,
            &ListRequirements {
                document_id: document,
                statuses: vec![RequirementStatus::Approved],
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert!(approved.items.is_empty(), "nothing has been approved yet");
}

// ---------------------------------------------------------------------------
// DAPI-11
// ---------------------------------------------------------------------------

#[test]
fn dapi11_consistency_reports_dangling_anchors_unverified_and_ambiguity() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));

    let report = svc
        .verify_consistency(
            &ctx,
            &VerifyConsistency {
                document_refs: vec![document.clone()],
                compare_code: false,
                max_findings: 100,
            },
        )
        .unwrap();
    let categories: Vec<ConsistencyCategory> = report
        .findings
        .iter()
        .map(|finding| finding.category)
        .collect();
    assert!(categories.contains(&ConsistencyCategory::MissingReference));
    assert!(categories.contains(&ConsistencyCategory::Unverified));
    assert_eq!(report.completeness, "complete");
    assert!(report.skipped_refs.is_empty());

    // the second revision adds "etc." which is an ambiguity, and asks for code
    let other = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-2", "rev-2"));
    let with_code = svc
        .verify_consistency(
            &ctx,
            &VerifyConsistency {
                document_refs: vec![other],
                compare_code: true,
                max_findings: 100,
            },
        )
        .unwrap();
    assert!(with_code
        .findings
        .iter()
        .any(|finding| finding.category == ConsistencyCategory::Ambiguity));
    assert_eq!(with_code.completeness, "partial");
    assert_eq!(with_code.skipped_refs.len(), 1);
    assert!(with_code
        .skipped_reasons
        .iter()
        .any(|reason| reason.contains("code domain")));

    assert_eq!(
        svc.verify_consistency(
            &ctx,
            &VerifyConsistency {
                document_refs: vec![document],
                compare_code: false,
                max_findings: 1,
            }
        )
        .unwrap_err()
        .code(),
        "RESOURCE_LIMIT"
    );
}

// ---------------------------------------------------------------------------
// DAPI-12
// ---------------------------------------------------------------------------

#[test]
fn dapi12_compare_revisions_classifies_changes() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let from = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));
    let to = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-2", "rev-2"));

    let diff = svc
        .compare_revisions(
            &ctx,
            &DocumentRevisionDiff {
                from: from.clone(),
                to,
            },
        )
        .unwrap();
    assert!(!diff.changes.is_empty());
    assert_eq!(diff.completeness, "partial", "images stay unparsed");
    let kinds: Vec<DocChangeKind> = diff.changes.iter().map(|change| change.kind).collect();
    assert!(
        kinds.iter().any(|kind| matches!(
            kind,
            DocChangeKind::Modified | DocChangeKind::Added | DocChangeKind::Removed
        )),
        "kinds: {kinds:?}"
    );

    assert_eq!(
        svc.compare_revisions(
            &ctx,
            &DocumentRevisionDiff {
                from,
                to: "doc:v1:missing".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
}

// ---------------------------------------------------------------------------
// DAPI-13
// ---------------------------------------------------------------------------

#[test]
fn dapi13_invalidate_is_stale_not_delete() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));
    let etag = svc.document_manifest(&ctx, &document).unwrap().etag;

    assert_eq!(
        svc.invalidate_revision(
            &ctx,
            &InvalidateDocumentRevision {
                document_id: document.clone(),
                reason: "superseded".to_string(),
                expected_etag: "\"wrong\"".to_string(),
            }
        )
        .unwrap_err()
        .code(),
        "ETAG_MISMATCH"
    );

    let receipt = svc
        .invalidate_revision(
            &ctx,
            &InvalidateDocumentRevision {
                document_id: document.clone(),
                reason: "superseded".to_string(),
                expected_etag: etag,
            },
        )
        .unwrap();
    assert!(receipt.changed);
    assert_eq!(receipt.events, vec!["document.revision.stale".to_string()]);

    // search refuses a stale document, but the content is still readable
    assert_eq!(
        svc.search(
            &ctx,
            &SearchDocument {
                tenant_id: TENANT.to_string(),
                project_id: PROJECT.to_string(),
                query: "evidence".to_string(),
                document_refs: vec![document.clone()],
                languages: Vec::new(),
                block_kinds: Vec::new(),
                page: PageRequest::new(10),
            }
        )
        .unwrap_err()
        .code(),
        "INDEX_STALE"
    );
    let view = svc
        .get_document(
            &ctx,
            &GetDocument {
                document_id: document.clone(),
                include_blocks: true,
                page: PageRequest::new(2),
            },
        )
        .unwrap();
    assert!(!view.blocks.items.is_empty(), "stale-not-delete");

    let again = svc
        .invalidate_revision(
            &ctx,
            &InvalidateDocumentRevision {
                document_id: document,
                reason: "again".to_string(),
                expected_etag: receipt.etag,
            },
        )
        .unwrap();
    assert!(!again.changed);
}

// ---------------------------------------------------------------------------
// DAPI-14
// ---------------------------------------------------------------------------

struct SpanRedactor;

impl DocumentAuthorizer for SpanRedactor {
    fn require_scope(
        &self,
        ctx: &RequestContext,
        scope: &str,
    ) -> document_intelligence::domain::Result<()> {
        ScopeAuthorizer.require_scope(ctx, scope)
    }

    fn redact(&self, _ctx: &RequestContext, _document_id: &str, span: &Span) -> bool {
        span.byte_start == 0
    }
}

#[test]
fn dapi14_source_span_returns_text_or_redacts_it() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));

    let view = svc
        .get_source_span(
            &ctx,
            &GetSourceSpan {
                document_id: document.clone(),
                span: Span::new(1, 1, 0, 11),
                max_bytes: 1024,
            },
        )
        .unwrap();
    assert!(!view.redacted);
    assert_eq!(view.text.as_deref(), Some("# Overview\n"));

    // a byte cap smaller than the span withholds the text instead of truncating
    let capped = svc
        .get_source_span(
            &ctx,
            &GetSourceSpan {
                document_id: document.clone(),
                span: Span::new(1, 1, 0, 11),
                max_bytes: 4,
            },
        )
        .unwrap();
    assert!(capped.redacted);
    assert!(capped.text.is_none());

    // an authorization policy can withhold any span
    let redacting = DocumentService::new(Arc::new(DocumentParsers::new()), Arc::new(docs()))
        .with_authorizer(Arc::new(SpanRedactor));
    let job = ingest(&redacting, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&redacting, &ctx, &job);
    let redacted = redacting
        .get_source_span(
            &ctx,
            &GetSourceSpan {
                document_id: document,
                span: Span::new(1, 1, 0, 11),
                max_bytes: 1024,
            },
        )
        .unwrap();
    assert!(redacted.redacted);
    assert!(redacted.text.is_none());

    assert_eq!(
        svc.get_source_span(
            &ctx,
            &GetSourceSpan {
                document_id: "doc:v1:missing".to_string(),
                span: Span::new(1, 1, 0, 4),
                max_bytes: 1024,
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
}

// ---------------------------------------------------------------------------
// §37 port
// ---------------------------------------------------------------------------

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[test]
fn port_surface_matches_the_fourteen_operations() {
    use document_intelligence::service::DocumentIntelligencePort;

    let (svc, _) = service();
    let ctx = full_ctx();
    let handle = block_on(DocumentIntelligencePort::ingest(
        &svc,
        &ctx,
        &ingest_request("artifact:rev-1", "rev-1"),
    ))
    .unwrap();
    let view = block_on(DocumentIntelligencePort::get_ingest_job(
        &svc,
        &ctx,
        &GetJob {
            job_id: handle.job_id.clone(),
        },
    ))
    .unwrap();
    assert!(view.canonical_committed);
    let document = view.result_ref.unwrap();
    let requirements = block_on(DocumentIntelligencePort::extract_requirements(
        &svc,
        &ctx,
        &ExtractRequirements {
            document_id: document.clone(),
            section_refs: Vec::new(),
            extractor_profile: EXTRACTOR_PROFILE.to_string(),
            page: PageRequest::new(10),
        },
    ))
    .unwrap();
    assert!(!requirements.items.is_empty());
    let report = block_on(DocumentIntelligencePort::verify_consistency(
        &svc,
        &ctx,
        &VerifyConsistency {
            document_refs: vec![document],
            compare_code: false,
            max_findings: 50,
        },
    ))
    .unwrap();
    assert!(!report.analyzed_refs.is_empty());
}

#[test]
fn unparsed_blocks_are_never_reported_as_complete() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let document = document_id(&svc, &ctx, &ingest(&svc, &ctx, "artifact:rev-1", "rev-1"));
    let manifest = svc.document_manifest(&ctx, &document).unwrap();
    assert_eq!(manifest.completeness, "partial");
    assert!(manifest.partial_ratio > 0);
    assert!(matches!(
        svc.get_document(
            &ctx,
            &GetDocument {
                document_id: document,
                include_blocks: true,
                page: PageRequest::new(200),
            }
        )
        .unwrap()
        .blocks
        .items
        .iter()
        .find(|block| block.kind == BlockKind::Image)
        .map(|block| block.parse_status),
        Some(ParseStatus::Unparsed)
    ));
    assert!(matches!(
        DocumentError::SourceTooLarge { bytes: 1, limit: 0 }.code(),
        "SOURCE_TOO_LARGE"
    ));
}

// ---------------------------------------------------------------------------
// DAPI-17 document.remove_source (Phase 8 gap closure)
// ---------------------------------------------------------------------------

fn ctx_with_key(key: Option<&str>, scopes: &[&str]) -> RequestContext {
    let mut ctx = base_ctx(scopes);
    ctx.idempotency_key = key.map(|value| value.to_string());
    ctx
}

#[test]
fn dapi17_remove_source_positive_and_tombstone() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&svc, &ctx, &job_id);
    let receipt = svc
        .remove_source(
            &ctx,
            &document_intelligence::domain::RemoveDocSource {
                document_id: document.clone(),
            },
        )
        .expect("positive path");
    assert!(receipt.deleted, "first removal deletes");
    assert!(receipt.removed_blocks > 0);
    // The source no longer lists the document.
    let page = svc
        .list_doc_sources(
            &ctx,
            &document_intelligence::domain::ListDocSources {
                page: PageRequest::new(200),
            },
        )
        .expect("list");
    assert!(
        !page.items.iter().any(|item| item.document_id == document),
        "removed document is gone from the listing"
    );
    // A repeated removal without the key is ALREADY_DELETED.
    let err = svc
        .remove_source(
            &ctx,
            &document_intelligence::domain::RemoveDocSource {
                document_id: document,
            },
        )
        .expect_err("repeated removal must fail");
    assert!(matches!(err, DocumentError::AlreadyDeleted { .. }));
}

#[test]
fn dapi17_remove_source_idempotent_replay_and_conflict() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&svc, &ctx, &job_id);

    // First removal with a key.
    let first = svc
        .remove_source(
            &ctx_with_key(Some("key-remove-1"), &[SCOPE_INDEX_ADMIN]),
            &document_intelligence::domain::RemoveDocSource {
                document_id: document.clone(),
            },
        )
        .expect("first removal");
    assert!(first.deleted);

    // Replay with the same key + same document: the recorded receipt.
    let replay = svc
        .remove_source(
            &ctx_with_key(Some("key-remove-1"), &[SCOPE_INDEX_ADMIN]),
            &document_intelligence::domain::RemoveDocSource {
                document_id: document.clone(),
            },
        )
        .expect("replay");
    assert!(!replay.deleted, "replay is not a fresh deletion");
    assert_eq!(replay.removed_blocks, first.removed_blocks);

    // Same key, different document: conflict.
    let job_id2 = ingest(&svc, &ctx, "artifact:other", "other");
    let document2 = document_id(&svc, &ctx, &job_id2);
    let err = svc
        .remove_source(
            &ctx_with_key(Some("key-remove-1"), &[SCOPE_INDEX_ADMIN]),
            &document_intelligence::domain::RemoveDocSource {
                document_id: document2,
            },
        )
        .expect_err("key reuse with a different document must conflict");
    assert!(matches!(err, DocumentError::IdempotencyConflict { .. }));
}

#[test]
fn dapi17_remove_source_negative_paths() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let _ = document_id(&svc, &ctx, &job_id);

    // Unknown document.
    let err = svc
        .remove_source(
            &ctx,
            &document_intelligence::domain::RemoveDocSource {
                document_id: "doc:missing".to_string(),
            },
        )
        .expect_err("unknown document must fail");
    assert!(matches!(err, DocumentError::NotFound { .. }));

    // Empty id.
    let err = svc
        .remove_source(
            &ctx,
            &document_intelligence::domain::RemoveDocSource {
                document_id: " ".to_string(),
            },
        )
        .expect_err("empty id must fail");
    assert!(matches!(err, DocumentError::InvalidArgument { .. }));
}

#[test]
fn dapi17_remove_source_requires_the_admin_scope() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&svc, &ctx, &job_id);
    let read_only = base_ctx(&[SCOPE_READ]);
    let err = svc
        .remove_source(
            &read_only,
            &document_intelligence::domain::RemoveDocSource {
                document_id: document,
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, DocumentError::Forbidden { .. }));
}

#[test]
fn dapi17_remove_source_deletion_blocked_by_a_queued_reingest() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&svc, &ctx, &job_id);
    // A queued re-ingest of the same artifact (new revision).
    let handle = svc
        .submit_ingest(
            &ctx,
            &document_intelligence::domain::DocumentIngest {
                source: DocumentSource {
                    source_uri: "file:///spec.md".to_string(),
                    media_type: "text/markdown".to_string(),
                    content_hash: String::new(),
                    revision: "rev-3".to_string(),
                    language_hint: None,
                    artifact_ref: "artifact:rev-1".to_string(),
                },
                parser_profile: "markdown/1".to_string(),
                include_images: true,
                asynchronous: true,
            },
        )
        .expect("submit");
    assert_eq!(handle.stage, "queued");
    let err = svc
        .remove_source(
            &ctx,
            &document_intelligence::domain::RemoveDocSource {
                document_id: document,
            },
        )
        .expect_err("a queued re-ingest must block the removal");
    assert!(matches!(err, DocumentError::DeletionBlocked { .. }));
}

#[test]
fn dapi17_remove_source_cross_scope_is_not_found() {
    let (svc, _) = service();
    let ctx = full_ctx();
    let job_id = ingest(&svc, &ctx, "artifact:rev-1", "rev-1");
    let document = document_id(&svc, &ctx, &job_id);
    let mut other = base_ctx(&[SCOPE_INDEX_ADMIN]);
    other.scope.tenant_id = "tenant-2".to_string();
    let err = svc
        .remove_source(
            &other,
            &document_intelligence::domain::RemoveDocSource {
                document_id: document,
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, DocumentError::NotFound { .. }));
}
