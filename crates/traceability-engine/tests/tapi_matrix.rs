//! TAPI-01..13 operation tests (§35.2) including the state machine, three-
//! dimensional validity, coverage semantics and the revalidation job.

use std::sync::Arc;

use traceability_engine::domain::{
    AcceptDecision, AcceptLink, CoverageQuery, EndpointKind, Evidence, ExplainEvidence, GapQuery,
    GetLink, GetTraceJob, IndexFreshness, ListLinks, PageRequest, ProposeLink, Ref,
    RevalidateStale, RevokeLink, ScopeRef, TraceImpactQuery, TraceKind, TraceLink, TraceLinkInput,
    TraceRevisionDiff, TraceState, ValidateLink,
};
use traceability_engine::service::{
    FixedClock, TraceabilityPort, TraceabilityService, SCOPE_APPROVE, SCOPE_READ, SCOPE_VERIFY,
    SCOPE_WRITE,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef as WireScope};

const ALL_SCOPES: [&str; 5] = [
    SCOPE_READ,
    SCOPE_WRITE,
    SCOPE_VERIFY,
    SCOPE_APPROVE,
    "trace:admin",
];

fn context(tenant: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-tapi".into(),
        trace_id: "trace-tapi".into(),
        scope: WireScope {
            tenant_id: tenant.to_string(),
            project_id: "project-b".into(),
            repo_id: None,
            workspace_id: Some("task-1".into()),
            agent_id: Some("agent-main".into()),
            session_id: Some("s1".into()),
        },
        principal_id: "principal".into(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        deadline_at: None,
        consistency: Consistency::ReadYourWrites,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

fn full() -> RequestContext {
    context("tenant-a", &ALL_SCOPES)
}

fn scope() -> ScopeRef {
    ScopeRef {
        tenant_id: "tenant-a".into(),
        project_id: "project-b".into(),
    }
}

fn evidence(id: &str) -> Evidence {
    Evidence {
        evidence_id: id.to_string(),
        source_ref: format!("artifact:sha256:{id}"),
        confidence: 0.9,
    }
}

fn kind_name(kind: EndpointKind) -> &'static str {
    match kind {
        EndpointKind::Requirement => "requirement",
        EndpointKind::Code => "code",
        EndpointKind::Test => "test",
        EndpointKind::Document => "document",
        EndpointKind::Other => "other",
    }
}

struct Fixture {
    service: TraceabilityService,
    clock: Arc<FixedClock>,
}

impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(FixedClock::new(1_000_000_000));
        let service = TraceabilityService::new().with_clock(clock.clone());
        let ctx = full();
        for (id, kind, authorised) in [
            ("req:1", EndpointKind::Requirement, true),
            ("req:2", EndpointKind::Requirement, true),
            ("code:a", EndpointKind::Code, true),
            ("test:1", EndpointKind::Test, true),
        ] {
            service
                .register_endpoint(
                    &ctx,
                    &Ref::new(id, kind_name(kind), "rev-1"),
                    kind,
                    authorised,
                    true,
                    IndexFreshness::Current,
                )
                .unwrap();
        }
        Self { service, clock }
    }

    fn propose(&self, source: &str, target: &str, kind: TraceKind) -> TraceLink {
        self.service
            .propose_link(
                &full(),
                &ProposeLink {
                    link: TraceLinkInput {
                        source: Ref::new(source, "code", "rev-1"),
                        target: Ref::new(target, "requirement", "rev-1"),
                        kind,
                        evidence: vec![evidence("e1")],
                        rationale: Some("because".into()),
                    },
                },
            )
            .unwrap()
    }

    fn accept(&self, link: &TraceLink) -> TraceLink {
        let validated = self
            .service
            .validate_link(
                &full(),
                &ValidateLink {
                    link: link.reference.clone(),
                    evidence_ids: vec!["e1".into()],
                    expected_etag: link.etag.clone(),
                },
            )
            .unwrap();
        self.service
            .accept_link(
                &full(),
                &AcceptLink {
                    link: validated.reference.clone(),
                    decision: AcceptDecision::Accept,
                    expected_etag: validated.etag.clone(),
                },
            )
            .unwrap()
    }
}

// -- TAPI-01 ----------------------------------------------------------------

#[test]
fn tapi01_propose_link_records_a_candidate() {
    let fixture = Fixture::new();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    assert_eq!(link.state, TraceState::Proposed);
    assert!(link.reference.id.starts_with("link:tenant-a:project-b:"));
    assert_eq!(link.evidence.len(), 1);
    assert_eq!(fixture.service.link_count(), 1);
}

#[test]
fn tapi01_propose_link_rejects_missing_evidence_self_links_and_unknowns() {
    let fixture = Fixture::new();
    let ctx = full();
    let mut request = ProposeLink {
        link: TraceLinkInput {
            source: Ref::new("code:a", "code", "rev-1"),
            target: Ref::new("req:1", "requirement", "rev-1"),
            kind: TraceKind::Implements,
            evidence: Vec::new(),
            rationale: None,
        },
    };
    assert_eq!(
        fixture
            .service
            .propose_link(&ctx, &request)
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );

    request.link.evidence = vec![evidence("e1")];
    request.link.target = request.link.source.clone();
    assert_eq!(
        fixture
            .service
            .propose_link(&ctx, &request)
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    request.link.target = Ref::new("req:none", "requirement", "rev-1");
    assert_eq!(
        fixture
            .service
            .propose_link(&ctx, &request)
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    request.link.target = Ref::new("req:1", "requirement", "rev-1");
    assert_eq!(
        fixture
            .service
            .propose_link(&reader, &request)
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

// -- TAPI-02 ----------------------------------------------------------------

#[test]
fn tapi02_validate_link_needs_evidence_and_a_fresh_etag() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);

    assert_eq!(
        fixture
            .service
            .validate_link(
                &ctx,
                &ValidateLink {
                    link: link.reference.clone(),
                    evidence_ids: Vec::new(),
                    expected_etag: link.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );
    assert_eq!(
        fixture
            .service
            .validate_link(
                &ctx,
                &ValidateLink {
                    link: link.reference.clone(),
                    evidence_ids: vec!["e1".into()],
                    expected_etag: "\"stale\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );
    assert_eq!(
        fixture
            .service
            .validate_link(
                &ctx,
                &ValidateLink {
                    link: link.reference.clone(),
                    evidence_ids: vec!["e9".into()],
                    expected_etag: link.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );

    let validated = fixture
        .service
        .validate_link(
            &ctx,
            &ValidateLink {
                link: link.reference.clone(),
                evidence_ids: vec!["e1".into()],
                expected_etag: link.etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(validated.state, TraceState::Validated);
    assert!(validated.verified_at.is_some());

    assert_eq!(
        fixture
            .service
            .validate_link(
                &ctx,
                &ValidateLink {
                    link: validated.reference.clone(),
                    evidence_ids: vec!["e1".into()],
                    expected_etag: validated.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        fixture
            .service
            .validate_link(
                &reader,
                &ValidateLink {
                    link: link.reference,
                    evidence_ids: vec!["e1".into()],
                    expected_etag: link.etag,
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

#[test]
fn tapi02_validate_link_refuses_a_moved_source_revision() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture
        .service
        .observe_endpoint(&ctx, &Ref::new("code:a", "code", "rev-2"))
        .unwrap();
    let error = fixture
        .service
        .validate_link(
            &ctx,
            &ValidateLink {
                link: link.reference,
                evidence_ids: vec!["e1".into()],
                expected_etag: link.etag,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "SOURCE_REVISION_MISMATCH");
}

// -- TAPI-03 / TAPI-04 ------------------------------------------------------

#[test]
fn tapi03_accept_requires_validation_and_rejection_is_terminal() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    assert_eq!(
        fixture
            .service
            .accept_link(
                &ctx,
                &AcceptLink {
                    link: link.reference.clone(),
                    decision: AcceptDecision::Accept,
                    expected_etag: link.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );

    let validated = fixture
        .service
        .validate_link(
            &ctx,
            &ValidateLink {
                link: link.reference.clone(),
                evidence_ids: vec!["e1".into()],
                expected_etag: link.etag.clone(),
            },
        )
        .unwrap();
    let rejected = fixture
        .service
        .accept_link(
            &ctx,
            &AcceptLink {
                link: validated.reference.clone(),
                decision: AcceptDecision::Reject,
                expected_etag: validated.etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(rejected.state, TraceState::Rejected);

    assert_eq!(
        fixture
            .service
            .validate_link(
                &ctx,
                &ValidateLink {
                    link: rejected.reference.clone(),
                    evidence_ids: vec!["e1".into()],
                    expected_etag: rejected.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );
}

#[test]
fn tapi03_and_04_accept_then_revoke_keeps_history() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    let accepted = fixture.accept(&link);
    assert_eq!(accepted.state, TraceState::Accepted);

    assert_eq!(
        fixture
            .service
            .revoke_link(
                &ctx,
                &RevokeLink {
                    link: accepted.reference.clone(),
                    reason: " ".into(),
                    expected_etag: accepted.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        fixture
            .service
            .revoke_link(
                &ctx,
                &RevokeLink {
                    link: accepted.reference.clone(),
                    reason: "withdrawn".into(),
                    expected_etag: "\"stale\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );

    let revoked = fixture
        .service
        .revoke_link(
            &ctx,
            &RevokeLink {
                link: accepted.reference.clone(),
                reason: "withdrawn".into(),
                expected_etag: accepted.etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(revoked.state, TraceState::Revoked);
    assert!(revoked.rationale.as_deref().unwrap().contains("withdrawn"));
    assert_eq!(
        revoked.validity.historical_validity,
        traceability_engine::HistoricalValidity::Valid,
        "a revoke does not erase history"
    );
}

// -- TAPI-05 / TAPI-06 ------------------------------------------------------

#[test]
fn tapi05_get_link_is_scoped_and_authorised() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    let read = fixture
        .service
        .get_link(
            &ctx,
            &GetLink {
                link: link.reference.clone(),
            },
        )
        .unwrap();
    assert_eq!(read.reference.id, link.reference.id);

    assert_eq!(
        fixture
            .service
            .get_link(
                &ctx,
                &GetLink {
                    link: Ref::new("link:none", "trace_link", "\"1\"")
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );

    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        fixture
            .service
            .get_link(
                &other,
                &GetLink {
                    link: link.reference.clone()
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert!(fixture
        .service
        .get_link(
            &reader,
            &GetLink {
                link: link.reference
            }
        )
        .is_ok());
}

#[test]
fn tapi06_list_links_filters_and_paginates() {
    let fixture = Fixture::new();
    let ctx = full();
    let first = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture.propose("code:a", "req:2", TraceKind::References);

    let all = fixture
        .service
        .list_links(
            &ctx,
            &ListLinks {
                endpoint: Ref::new("code:a", "code", "rev-1"),
                kinds: Vec::new(),
                states: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(all.items.len(), 2);

    let filtered = fixture
        .service
        .list_links(
            &ctx,
            &ListLinks {
                endpoint: Ref::new("code:a", "code", "rev-1"),
                kinds: vec![TraceKind::Implements],
                states: vec![TraceState::Proposed],
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].reference.id, first.reference.id);

    let page = fixture
        .service
        .list_links(
            &ctx,
            &ListLinks {
                endpoint: Ref::new("code:a", "code", "rev-1"),
                kinds: Vec::new(),
                states: Vec::new(),
                page: PageRequest::new(1),
            },
        )
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_some());

    assert_eq!(
        fixture
            .service
            .list_links(
                &ctx,
                &ListLinks {
                    endpoint: Ref::new("code:a", "code", "rev-1"),
                    kinds: Vec::new(),
                    states: Vec::new(),
                    page: PageRequest::new(0),
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

// -- TAPI-07 ----------------------------------------------------------------

#[test]
fn tapi07_coverage_counts_only_applicable_accepted_links() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture.accept(&link);

    let report = fixture
        .service
        .query_coverage(
            &ctx,
            &CoverageQuery {
                requirement_scope: scope(),
                requirement_refs: Vec::new(),
                implementation_kinds: vec![TraceKind::Implements],
                accepted_only: true,
                include_unindexed: false,
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(report.denominator, 2);
    assert_eq!(report.numerator, 1);
    assert_eq!(report.coverage_ratio, Some(0.5));
    assert_eq!(report.uncovered.len(), 1);
    assert!(!report.coverage_incomplete);
}

#[test]
fn tapi07_an_unauthorised_requirement_makes_coverage_incomplete() {
    let fixture = Fixture::new();
    let ctx = full();
    fixture
        .service
        .register_endpoint(
            &ctx,
            &Ref::new("req:hidden", "requirement", "rev-1"),
            EndpointKind::Requirement,
            false,
            true,
            IndexFreshness::Current,
        )
        .unwrap();
    let report = fixture
        .service
        .query_coverage(
            &ctx,
            &CoverageQuery {
                requirement_scope: scope(),
                requirement_refs: Vec::new(),
                implementation_kinds: vec![TraceKind::Implements],
                accepted_only: true,
                include_unindexed: false,
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(report.excluded_count, 1);
    assert!(report.coverage_incomplete);
    assert_eq!(report.denominator, 2);
    assert_ne!(report.coverage_ratio, Some(1.0), "no false 100%");

    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        fixture
            .service
            .query_coverage(
                &other,
                &CoverageQuery {
                    requirement_scope: scope(),
                    requirement_refs: Vec::new(),
                    implementation_kinds: Vec::new(),
                    accepted_only: true,
                    include_unindexed: false,
                    page: PageRequest::new(10),
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

#[test]
fn tapi07_a_stale_link_becomes_unknown_not_covered() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture.accept(&link);
    fixture
        .service
        .observe_endpoint(&ctx, &Ref::new("code:a", "code", "rev-2"))
        .unwrap();

    let report = fixture
        .service
        .query_coverage(
            &ctx,
            &CoverageQuery {
                requirement_scope: scope(),
                requirement_refs: Vec::new(),
                implementation_kinds: vec![TraceKind::Implements],
                accepted_only: false,
                include_unindexed: false,
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(report.covered.len(), 0);
    assert_eq!(report.unknown.len(), 1);
    assert!(report.coverage_incomplete);
}

// -- TAPI-08 ----------------------------------------------------------------

#[test]
fn tapi08_impact_traversal_is_bounded_and_reports_stale_frontiers() {
    let fixture = Fixture::new();
    let ctx = full();
    let implements = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture.accept(&implements);
    let verifies = fixture.propose("test:1", "req:1", TraceKind::Verifies);
    fixture.accept(&verifies);

    let result = fixture
        .service
        .analyze_change_impact(
            &ctx,
            &TraceImpactQuery {
                changed: vec![Ref::new("req:1", "requirement", "rev-1")],
                max_depth: 2,
                kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert_eq!(result.impacted.len(), 2);
    assert_eq!(
        result.completeness,
        traceability_engine::Completeness::Complete
    );
    assert_eq!(result.paths.len(), 2);

    assert_eq!(
        fixture
            .service
            .analyze_change_impact(
                &ctx,
                &TraceImpactQuery {
                    changed: Vec::new(),
                    max_depth: 1,
                    kinds: Vec::new(),
                    page: PageRequest::new(10),
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        fixture
            .service
            .analyze_change_impact(
                &ctx,
                &TraceImpactQuery {
                    changed: vec![Ref::new("req:1", "requirement", "rev-1")],
                    max_depth: 0,
                    kinds: Vec::new(),
                    page: PageRequest::new(10),
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    fixture
        .service
        .observe_endpoint(&ctx, &Ref::new("code:a", "code", "rev-2"))
        .unwrap();
    let stale = fixture
        .service
        .analyze_change_impact(
            &ctx,
            &TraceImpactQuery {
                changed: vec![Ref::new("req:1", "requirement", "rev-1")],
                max_depth: 2,
                kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert!(!stale.unknown_due_to_stale.is_empty());
    assert_eq!(
        stale.completeness,
        traceability_engine::Completeness::Partial
    );
}

// -- TAPI-09 ----------------------------------------------------------------

#[test]
fn tapi09_gaps_report_missing_implementation_and_tests() {
    let fixture = Fixture::new();
    let ctx = full();
    let implements = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture.accept(&implements);

    let report = fixture
        .service
        .find_design_gaps(
            &ctx,
            &GapQuery {
                scope: scope(),
                requirement_refs: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.missing_implementation.len(), 1);
    assert_eq!(report.missing_implementation[0].id, "req:2");
    assert_eq!(report.missing_tests.len(), 1);
    assert_eq!(report.missing_tests[0].id, "code:a");
    assert!(report.stale_links.is_empty());
    assert!(!report.coverage_incomplete);
}

// -- TAPI-10 ----------------------------------------------------------------

#[test]
fn tapi10_compare_revisions_reports_added_changed_removed() {
    let fixture = Fixture::new();
    let ctx = full();
    let first = fixture.propose("code:a", "req:1", TraceKind::Implements);
    assert_eq!(fixture.service.record_snapshot(&ctx, "rev-a").unwrap(), 1);

    fixture.accept(&first);
    let second = fixture.propose("code:a", "req:2", TraceKind::References);
    fixture.service.record_snapshot(&ctx, "rev-b").unwrap();

    let diff = fixture
        .service
        .compare_revisions(
            &ctx,
            &TraceRevisionDiff {
                from_revision: "rev-a".into(),
                to_revision: "rev-b".into(),
                scope: scope(),
            },
        )
        .unwrap();
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.added[0].reference.id, second.reference.id);
    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].reference.id, first.reference.id);
    assert!(diff.removed.is_empty());

    assert_eq!(
        fixture
            .service
            .compare_revisions(
                &ctx,
                &TraceRevisionDiff {
                    from_revision: "rev-zzz".into(),
                    to_revision: "rev-b".into(),
                    scope: scope(),
                }
            )
            .unwrap_err()
            .code(),
        "SOURCE_UNAVAILABLE"
    );
    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        fixture
            .service
            .compare_revisions(
                &other,
                &TraceRevisionDiff {
                    from_revision: "rev-a".into(),
                    to_revision: "rev-b".into(),
                    scope: scope(),
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// -- TAPI-11 ----------------------------------------------------------------

#[test]
fn tapi11_explain_with_evidence_concludes_and_respects_access() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    fixture.accept(&link);

    let report = fixture
        .service
        .explain_with_evidence(
            &ctx,
            &ExplainEvidence {
                claim: "the requirement is implemented".into(),
                refs: vec![Ref::new("req:1", "requirement", "rev-1")],
                max_tokens: 10_000,
            },
        )
        .unwrap();
    assert_eq!(
        report.conclusion,
        traceability_engine::Conclusion::Supported
    );
    assert_eq!(report.omitted_due_to_access, 0);
    assert_eq!(report.evidence.len(), 1);

    let report = fixture
        .service
        .explain_with_evidence(
            &ctx,
            &ExplainEvidence {
                claim: "another claim".into(),
                refs: vec![Ref::new("req:unknown", "requirement", "rev-1")],
                max_tokens: 10_000,
            },
        )
        .unwrap();
    assert_eq!(report.omitted_due_to_access, 1);
    assert_eq!(
        report.completeness,
        traceability_engine::Completeness::Partial
    );

    assert_eq!(
        fixture
            .service
            .explain_with_evidence(
                &ctx,
                &ExplainEvidence {
                    claim: "the requirement is implemented".into(),
                    refs: vec![Ref::new("req:1", "requirement", "rev-1")],
                    max_tokens: 1,
                }
            )
            .unwrap_err()
            .code(),
        "CONTEXT_BUDGET_EXCEEDED"
    );
}

// -- TAPI-12 / TAPI-13 ------------------------------------------------------

#[test]
fn tapi12_revalidation_revalidates_a_reverted_source_and_keeps_true_staleness() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = fixture.propose("code:a", "req:1", TraceKind::Implements);
    let accepted = fixture.accept(&link);

    fixture
        .service
        .observe_endpoint(&ctx, &Ref::new("code:a", "code", "rev-2"))
        .unwrap();
    let stale = fixture
        .service
        .get_link(
            &ctx,
            &GetLink {
                link: accepted.reference.clone(),
            },
        )
        .unwrap();
    assert_eq!(stale.state, TraceState::Stale);

    let error = fixture
        .service
        .revalidate_stale(
            &ctx,
            &RevalidateStale {
                link_refs: vec![accepted.reference.clone()],
                policy_id: "policy:default".into(),
                asynchronous: false,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "INVALID_ARGUMENT");
    // The wire field is `asynchronous`; the hint must name it exactly.
    let message = error.to_string();
    assert!(message.contains("`asynchronous: true`"), "{message}");
    assert!(!message.contains("`async: true`"), "{message}");

    let partial = fixture
        .service
        .revalidate_stale(
            &ctx,
            &RevalidateStale {
                link_refs: vec![accepted.reference.clone()],
                policy_id: "policy:default".into(),
                asynchronous: true,
            },
        )
        .unwrap();
    assert_eq!(partial.stage, "partial");
    let job = fixture
        .service
        .get_revalidation_job(
            &ctx,
            &GetTraceJob {
                job_id: partial.job_id.clone(),
            },
        )
        .unwrap();
    assert!(!job.canonical_committed);
    assert!(job
        .diagnostics
        .iter()
        .any(|line| line.contains("still stale")));
    let still_stale = fixture
        .service
        .get_link(
            &ctx,
            &GetLink {
                link: accepted.reference.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        still_stale.state,
        TraceState::Stale,
        "a revalidation never auto-accepts"
    );

    fixture
        .service
        .observe_endpoint(&ctx, &Ref::new("code:a", "code", "rev-1"))
        .unwrap();
    let done = fixture
        .service
        .revalidate_stale(
            &ctx,
            &RevalidateStale {
                link_refs: vec![accepted.reference.clone()],
                policy_id: "policy:default".into(),
                asynchronous: true,
            },
        )
        .unwrap();
    assert_eq!(done.stage, "committed");
    let revalidated = fixture
        .service
        .get_link(
            &ctx,
            &GetLink {
                link: accepted.reference,
            },
        )
        .unwrap();
    assert_eq!(revalidated.state, TraceState::Validated);
}

#[test]
fn tapi12_and_13_jobs_are_validated_and_scoped() {
    let fixture = Fixture::new();
    let ctx = full();
    assert_eq!(
        fixture
            .service
            .revalidate_stale(
                &ctx,
                &RevalidateStale {
                    link_refs: Vec::new(),
                    policy_id: "policy:default".into(),
                    asynchronous: true,
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        fixture
            .service
            .revalidate_stale(
                &ctx,
                &RevalidateStale {
                    link_refs: vec![Ref::new("link:none", "trace_link", "\"1\"")],
                    policy_id: " ".into(),
                    asynchronous: true,
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    let handle = fixture
        .service
        .revalidate_stale(
            &ctx,
            &RevalidateStale {
                link_refs: vec![Ref::new("link:none", "trace_link", "\"1\"")],
                policy_id: "policy:default".into(),
                asynchronous: true,
            },
        )
        .unwrap();
    assert!(handle
        .job_id
        .starts_with("job:trace-revalidation:tenant-a:"));
    assert_eq!(
        fixture
            .service
            .get_revalidation_job(
                &ctx,
                &GetTraceJob {
                    job_id: "job:trace-revalidation:tenant-z:1".into()
                }
            )
            .unwrap_err()
            .code(),
        "JOB_NOT_FOUND"
    );
    assert_eq!(
        fixture
            .service
            .get_revalidation_job(
                &ctx,
                &GetTraceJob {
                    job_id: handle.job_id
                }
            )
            .map(|job| job.kind),
        Ok("traceability.revalidate_stale".to_string())
    );
}

// -- §37 port ---------------------------------------------------------------

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
fn port_exposes_all_thirteen_operations() {
    let fixture = Fixture::new();
    let ctx = full();
    let link = block_on(TraceabilityPort::propose_link(
        &fixture.service,
        &ctx,
        &ProposeLink {
            link: TraceLinkInput {
                source: Ref::new("code:a", "code", "rev-1"),
                target: Ref::new("req:1", "requirement", "rev-1"),
                kind: TraceKind::Implements,
                evidence: vec![evidence("e1")],
                rationale: None,
            },
        },
    ))
    .unwrap();
    let validated = block_on(TraceabilityPort::validate_link(
        &fixture.service,
        &ctx,
        &ValidateLink {
            link: link.reference.clone(),
            evidence_ids: vec!["e1".into()],
            expected_etag: link.etag.clone(),
        },
    ))
    .unwrap();
    assert_eq!(validated.state, TraceState::Validated);
    let accepted = block_on(TraceabilityPort::accept_link(
        &fixture.service,
        &ctx,
        &AcceptLink {
            link: validated.reference.clone(),
            decision: AcceptDecision::Accept,
            expected_etag: validated.etag.clone(),
        },
    ))
    .unwrap();
    assert_eq!(accepted.state, TraceState::Accepted);
    let coverage = block_on(TraceabilityPort::query_coverage(
        &fixture.service,
        &ctx,
        &CoverageQuery {
            requirement_scope: scope(),
            requirement_refs: Vec::new(),
            implementation_kinds: vec![TraceKind::Implements],
            accepted_only: true,
            include_unindexed: false,
            page: PageRequest::new(10),
        },
    ))
    .unwrap();
    assert_eq!(coverage.numerator, 1);
    assert!(traceability_engine::service::TraceClock::now(&*fixture.clock).ends_with('Z'));
}
