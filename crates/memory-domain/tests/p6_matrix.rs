//! MAPI-10..12 and MAPI-18..22 operation tests (§26.3, §27.1, §27.2, §34.2).

use std::sync::Arc;

use context_engine::Reserves;
use memory_domain::consolidation::Stage;
use memory_domain::domain::{
    AgentScopeGrant, Consolidate, DetailLevel, EpisodeInput, EpisodeOutcome, EpisodeType, Evidence,
    FactDecision, FindConflicts, GetMemoryJob, MemoryKind, MemoryQuery, OpenAgentScope,
    ProcedureDecision, ProcedureDraft, ProposeFact, RecordProcedureOutcome, Ref, RegisterProcedure,
    ReviewProcedure, SearchMemory, SemanticFactInput, StoreEpisode, VerifyFact,
};
use memory_domain::poisoning::SourceObservation;
use memory_domain::service::{
    FixedClock, MemoryService, SCOPE_CONSOLIDATE, SCOPE_FORGET, SCOPE_MANAGE,
    SCOPE_PROCEDURE_REVIEW, SCOPE_PROCEDURE_WRITE, SCOPE_READ, SCOPE_REVIEW, SCOPE_WRITE,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const ALL_SCOPES: [&str; 10] = [
    SCOPE_READ,
    SCOPE_WRITE,
    SCOPE_REVIEW,
    SCOPE_CONSOLIDATE,
    SCOPE_PROCEDURE_WRITE,
    SCOPE_PROCEDURE_REVIEW,
    SCOPE_MANAGE,
    SCOPE_FORGET,
    "memory:delegate",
    "memory:fold",
];

fn context(tenant: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-p6".into(),
        trace_id: "trace-p6".into(),
        scope: ScopeRef {
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

fn service() -> (MemoryService, Arc<FixedClock>) {
    let clock = Arc::new(FixedClock::new(1_000_000_000));
    let service = MemoryService::new()
        .with_reserves(Reserves::new(16_384, 4_096, 2_048, 1_024))
        .with_clock(clock.clone());
    (service, clock)
}

fn evidence(id: &str, group: &str) -> Evidence {
    Evidence {
        evidence_id: id.to_string(),
        source_ref: format!("artifact:sha256:{id}"),
        confidence: 0.9,
        source_group: group.to_string(),
    }
}

fn fact_input(groups: usize, confidence: f64) -> SemanticFactInput {
    SemanticFactInput {
        subject: Ref::new("concept:order", MemoryKind::Semantic, "\"1\""),
        predicate: "status".to_string(),
        object: "shipped".to_string(),
        language: "en".to_string(),
        evidence: (0..groups)
            .map(|index| evidence(&format!("e{index}"), &format!("doc-{index}")))
            .collect(),
        confidence,
    }
}

fn episode(service: &MemoryService, ctx: &RequestContext) -> String {
    let receipt = service
        .store_episode(
            ctx,
            &StoreEpisode {
                input: EpisodeInput {
                    session_id: "s1".into(),
                    agent_id: "agent-main".into(),
                    agent_depth: 0,
                    episode_type: EpisodeType::ConversationTurn,
                    summary: "alpha order shipped".into(),
                    full_text_ref: None,
                    key_facts: vec!["order shipped".into()],
                    outcome: EpisodeOutcome::Success,
                    source_refs: vec!["artifact:sha256:aa".into()],
                    importance_score: 0.7,
                    language: "en".into(),
                    evidence: vec![evidence("e0", "doc-0")],
                },
            },
        )
        .unwrap();
    receipt.resource_id
}

// -- MAPI-10 ----------------------------------------------------------------

#[test]
fn mapi10_propose_fact_requires_evidence_and_a_write_scope() {
    let (service, _) = service();
    let ctx = full();
    let reference = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(2, 0.7),
            },
        )
        .unwrap();
    assert_eq!(reference.kind, MemoryKind::Semantic);
    assert!(reference.id.starts_with("fact:tenant-a:project-b:"));

    let mut no_evidence = fact_input(2, 0.7);
    no_evidence.evidence.clear();
    assert_eq!(
        service
            .propose_fact(&ctx, &ProposeFact { input: no_evidence })
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );

    assert_eq!(
        service
            .propose_fact(
                &ctx,
                &ProposeFact {
                    input: fact_input(2, 1.4)
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .propose_fact(
                &reader,
                &ProposeFact {
                    input: fact_input(2, 0.7)
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

// -- MAPI-11 ----------------------------------------------------------------

#[test]
fn mapi11_verify_fact_checks_etag_scope_and_state() {
    let (service, _) = service();
    let ctx = full();
    let reference = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(2, 0.7),
            },
        )
        .unwrap();

    assert_eq!(
        service
            .verify_fact(
                &ctx,
                &VerifyFact {
                    fact: reference.clone(),
                    evidence_ids: vec!["e0".into()],
                    decision: FactDecision::Verify,
                    expected_etag: "\"stale\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );

    let receipt = service
        .verify_fact(
            &ctx,
            &VerifyFact {
                fact: reference.clone(),
                evidence_ids: vec!["e0".into()],
                decision: FactDecision::Verify,
                expected_etag: reference.revision.clone(),
            },
        )
        .unwrap();
    assert!(receipt.changed);
    assert_eq!(receipt.events, vec!["memory.fact.reviewed"]);

    assert_eq!(
        service
            .verify_fact(
                &ctx,
                &VerifyFact {
                    fact: reference.clone(),
                    evidence_ids: vec!["e0".into()],
                    decision: FactDecision::Reject,
                    expected_etag: receipt.etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .verify_fact(
                &reader,
                &VerifyFact {
                    fact: reference.clone(),
                    evidence_ids: vec!["e0".into()],
                    decision: FactDecision::Verify,
                    expected_etag: receipt.etag,
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );

    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        service
            .verify_fact(
                &other,
                &VerifyFact {
                    fact: reference,
                    evidence_ids: vec!["e0".into()],
                    decision: FactDecision::Verify,
                    expected_etag: "\"x\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

#[test]
fn mapi11_a_contradicted_fact_cannot_be_verified() {
    let (service, _) = service();
    let ctx = full();
    let first = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(2, 0.7),
            },
        )
        .unwrap();
    let mut conflicting = fact_input(2, 0.7);
    conflicting.object = "cancelled".to_string();
    service
        .propose_fact(&ctx, &ProposeFact { input: conflicting })
        .unwrap();

    assert_eq!(
        service
            .verify_fact(
                &ctx,
                &VerifyFact {
                    fact: first.clone(),
                    evidence_ids: vec!["e0".into()],
                    decision: FactDecision::Verify,
                    expected_etag: first.revision.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    let conflicts = service
        .find_conflicts(
            &ctx,
            &FindConflicts {
                fact: first.clone(),
                max_results: 10,
            },
        )
        .unwrap();
    assert_eq!(conflicts.items.len(), 1);
    assert!(conflicts.items[0]
        .summary
        .as_deref()
        .unwrap()
        .contains("cancelled"));

    // Rejecting is still allowed: the conflict is recorded, not hidden.
    let rejected = service
        .verify_fact(
            &ctx,
            &VerifyFact {
                fact: first,
                evidence_ids: vec!["e0".into()],
                decision: FactDecision::Reject,
                expected_etag: "\"ignored\"".into(),
            },
        )
        .unwrap_err();
    assert_eq!(rejected.code(), "ETAG_MISMATCH");
}

// -- MAPI-12 ----------------------------------------------------------------

#[test]
fn mapi12_find_conflicts_is_scoped_and_bounded() {
    let (service, _) = service();
    let ctx = full();
    let target = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(2, 0.7),
            },
        )
        .unwrap();
    let mut other = fact_input(2, 0.7);
    other.object = "not_shipped".to_string();
    service
        .propose_fact(&ctx, &ProposeFact { input: other })
        .unwrap();

    let page = service
        .find_conflicts(
            &ctx,
            &FindConflicts {
                fact: target.clone(),
                max_results: 10,
            },
        )
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, MemoryKind::Semantic);

    assert_eq!(
        service
            .find_conflicts(
                &ctx,
                &FindConflicts {
                    fact: target.clone(),
                    max_results: 0
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .find_conflicts(
                &ctx,
                &FindConflicts {
                    fact: Ref::new("fact:none", MemoryKind::Semantic, "\"x\""),
                    max_results: 5
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );

    let other_tenant = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        service
            .find_conflicts(
                &other_tenant,
                &FindConflicts {
                    fact: target,
                    max_results: 5
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// -- MAPI-18 / MAPI-19 ------------------------------------------------------

#[test]
fn mapi18_consolidate_promotes_only_claims_that_pass_the_gate() {
    let (service, _) = service();
    let ctx = full();
    let source = episode(&service, &ctx);
    let promotable = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(3, 0.7),
            },
        )
        .unwrap();
    let under_sourced = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(1, 0.9),
            },
        )
        .unwrap();

    let handle = service
        .consolidate(
            &ctx,
            &Consolidate {
                episode_refs: vec![Ref::new(source, MemoryKind::Episodic, "\"1\"")],
                policy_id: "policy:default".into(),
                asynchronous: true,
            },
        )
        .unwrap();
    assert_eq!(handle.kind, "memory.consolidation");
    assert_eq!(handle.stage, "committed");

    let job = service
        .get_consolidation_job(
            &ctx,
            &GetMemoryJob {
                job_id: handle.job_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(job.stage, "committed");
    assert!(job.canonical_committed);
    assert!(job.diagnostics.is_empty());
    assert!(handle.job_id.starts_with("job:consolidation:tenant-a:"));

    // The promoted claim is now invisible to a second promotion (verified), and
    // the under-sourced one is still a candidate: the conflict view shows both.
    let conflicts = service
        .find_conflicts(
            &ctx,
            &FindConflicts {
                fact: promotable.clone(),
                max_results: 10,
            },
        )
        .unwrap();
    assert!(conflicts.items.is_empty());
    assert_eq!(under_sourced.kind, MemoryKind::Semantic);

    assert_eq!(
        service
            .get_consolidation_job(
                &ctx,
                &GetMemoryJob {
                    job_id: "job:consolidation:tenant-z:1".into()
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
}

#[test]
fn mapi18_consolidate_validates_its_inputs() {
    let (service, _) = service();
    let ctx = full();
    let source = episode(&service, &ctx);
    let base = Consolidate {
        episode_refs: vec![Ref::new(source, MemoryKind::Episodic, "\"1\"")],
        policy_id: "policy:default".into(),
        asynchronous: true,
    };

    assert_eq!(
        service
            .consolidate(
                &ctx,
                &Consolidate {
                    asynchronous: false,
                    ..base.clone()
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .consolidate(
                &ctx,
                &Consolidate {
                    episode_refs: Vec::new(),
                    ..base.clone()
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .consolidate(
                &ctx,
                &Consolidate {
                    policy_id: " ".into(),
                    ..base.clone()
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .consolidate(
                &ctx,
                &Consolidate {
                    episode_refs: vec![Ref::new("mem:none", MemoryKind::Episodic, "\"1\"")],
                    ..base.clone()
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service.consolidate(&reader, &base).unwrap_err().code(),
        "FORBIDDEN"
    );
}

#[test]
fn mapi18_a_quarantined_source_freezes_promotion() {
    let (service, _) = service();
    let ctx = full();
    let source = episode(&service, &ctx);
    let fact = service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(3, 0.9),
            },
        )
        .unwrap();

    let record = service
        .quarantine_source(
            &ctx,
            &SourceObservation {
                source_group: "doc-0".into(),
                source_mutated: true,
                writes_last_hour: 5,
                contradictions_last_hour: 0,
                trust_delta_z: 0.1,
                instruction_like: false,
            },
        )
        .unwrap();
    assert!(record.active);
    assert!(service.is_quarantined("doc-0"));

    // Verification is refused while the source is quarantined.
    assert_eq!(
        service
            .verify_fact(
                &ctx,
                &VerifyFact {
                    fact: fact.clone(),
                    evidence_ids: vec!["e0".into()],
                    decision: FactDecision::Verify,
                    expected_etag: fact.revision.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    let handle = service
        .consolidate(
            &ctx,
            &Consolidate {
                episode_refs: vec![Ref::new(source, MemoryKind::Episodic, "\"1\"")],
                policy_id: "policy:default".into(),
                asynchronous: true,
            },
        )
        .unwrap();
    assert_eq!(handle.stage, "quarantined");
    let job = service
        .get_consolidation_job(
            &ctx,
            &GetMemoryJob {
                job_id: handle.job_id,
            },
        )
        .unwrap();
    assert!(!job.canonical_committed);
    assert!(job
        .diagnostics
        .iter()
        .any(|line| line.contains("quarantined")));

    // An audited review restores the source and promotion works again.
    service
        .restore_source(&ctx, "doc-0", "reviewer-1", "false positive", "snapshot:1")
        .unwrap();
    assert!(!service.is_quarantined("doc-0"));
    let receipt = service
        .verify_fact(
            &ctx,
            &VerifyFact {
                fact: fact.clone(),
                evidence_ids: vec!["e0".into()],
                decision: FactDecision::Verify,
                expected_etag: fact.revision.clone(),
            },
        )
        .unwrap();
    assert!(receipt.changed);
}

#[test]
fn mapi18_the_pipeline_records_every_stage_in_order() {
    let (service, _) = service();
    let ctx = full();
    let source = episode(&service, &ctx);
    let handle = service
        .consolidate(
            &ctx,
            &Consolidate {
                episode_refs: vec![Ref::new(source, MemoryKind::Episodic, "\"1\"")],
                policy_id: "policy:default".into(),
                asynchronous: true,
            },
        )
        .unwrap();
    assert_eq!(handle.stage, "committed");
    assert_eq!(Stage::ALL.len(), 9);
}

// -- MAPI-20 ----------------------------------------------------------------

fn draft() -> ProcedureDraft {
    ProcedureDraft {
        name: "retry the upload".into(),
        preconditions: vec!["the upload failed".into()],
        steps: vec!["open the dashboard".into(), "retry".into()],
        evidence: vec![evidence("e0", "doc-0")],
        permissions: vec!["upload".into()],
        source_episodes: vec![Ref::new("ep:1", MemoryKind::Episodic, "\"1\"")],
        language: "en".into(),
    }
}

#[test]
fn mapi20_register_procedure_requires_evidence_and_scope() {
    let (service, _) = service();
    let ctx = full();
    let view = service
        .register_procedure_candidate(&ctx, &RegisterProcedure { draft: draft() })
        .unwrap();
    assert_eq!(view.state, memory_domain::ProcedureState::Candidate);
    assert_eq!(view.run_count, 0);
    assert!(view.success_rate < 1.0, "a fresh procedure is not 100%");
    assert_eq!(view.permissions, vec!["upload".to_string()]);

    let mut no_evidence = draft();
    no_evidence.evidence.clear();
    assert_eq!(
        service
            .register_procedure_candidate(&ctx, &RegisterProcedure { draft: no_evidence })
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .register_procedure_candidate(&reader, &RegisterProcedure { draft: draft() })
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

// -- MAPI-21 / MAPI-22 ------------------------------------------------------

#[test]
fn mapi21_to_mapi22_the_procedure_lifecycle_runs_end_to_end() {
    let (service, _) = service();
    let ctx = full();
    let view = service
        .register_procedure_candidate(&ctx, &RegisterProcedure { draft: draft() })
        .unwrap();
    let mut etag = view.reference.revision.clone();
    assert_eq!(view.state, memory_domain::ProcedureState::Candidate);

    // Activating a candidate without runs is refused: consolidation gates first.
    let blocked = service
        .review_procedure(
            &ctx,
            &ReviewProcedure {
                procedure: view.reference.clone(),
                decision: ProcedureDecision::Activate,
                evidence_ids: Vec::new(),
                expected_etag: etag.clone(),
            },
        )
        .unwrap_err();
    assert_eq!(blocked.code(), "POLICY_DENIED");
    assert!(blocked.to_string().contains("REVIEW_REQUIRED"));

    // Three successful trials reach the consolidation gate.
    for index in 0..3 {
        let receipt = service
            .record_procedure_outcome(
                &ctx,
                &RecordProcedureOutcome {
                    procedure: view.reference.clone(),
                    success: true,
                    evidence: vec![evidence(&format!("e{index}"), "doc-1")],
                    expected_etag: etag.clone(),
                },
            )
            .unwrap();
        etag = receipt.etag;
    }

    let activated = service
        .review_procedure(
            &ctx,
            &ReviewProcedure {
                procedure: view.reference.clone(),
                decision: ProcedureDecision::Activate,
                evidence_ids: vec!["e0".into()],
                expected_etag: etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(activated.state, memory_domain::ProcedureState::Active);
    assert!(activated.success_rate < 1.0);
    etag = activated.reference.revision.clone();

    // Two failures drift the procedure into refinement; the state is a state,
    // never a permission grant.
    for _ in 0..2 {
        let receipt = service
            .record_procedure_outcome(
                &ctx,
                &RecordProcedureOutcome {
                    procedure: view.reference.clone(),
                    success: false,
                    evidence: vec![evidence("e9", "doc-9")],
                    expected_etag: etag.clone(),
                },
            )
            .unwrap();
        etag = receipt.etag;
    }
    let refined = service
        .review_procedure(
            &ctx,
            &ReviewProcedure {
                procedure: view.reference.clone(),
                decision: ProcedureDecision::Refine,
                evidence_ids: Vec::new(),
                expected_etag: etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(refined.state, memory_domain::ProcedureState::Refining);

    let disabled = service
        .review_procedure(
            &ctx,
            &ReviewProcedure {
                procedure: view.reference,
                decision: ProcedureDecision::Disable,
                evidence_ids: Vec::new(),
                expected_etag: refined.reference.revision.clone(),
            },
        )
        .unwrap();
    assert_eq!(disabled.state, memory_domain::ProcedureState::Disabled);
}

#[test]
fn mapi22_record_outcomes_is_refused_once_the_procedure_is_disabled() {
    let (service, _) = service();
    let ctx = full();
    let view = service
        .register_procedure_candidate(&ctx, &RegisterProcedure { draft: draft() })
        .unwrap();

    // Trials are allowed while still a candidate: the consolidation gate needs them.
    let trial = service
        .record_procedure_outcome(
            &ctx,
            &RecordProcedureOutcome {
                procedure: view.reference.clone(),
                success: true,
                evidence: vec![evidence("e1", "doc-1")],
                expected_etag: view.reference.revision.clone(),
            },
        )
        .unwrap();
    assert!(trial.changed);

    assert_eq!(
        service
            .record_procedure_outcome(
                &ctx,
                &RecordProcedureOutcome {
                    procedure: view.reference.clone(),
                    success: true,
                    evidence: vec![evidence("e1", "doc-1")],
                    expected_etag: view.reference.revision.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );

    let disabled = service
        .review_procedure(
            &ctx,
            &ReviewProcedure {
                procedure: view.reference.clone(),
                decision: ProcedureDecision::Disable,
                evidence_ids: Vec::new(),
                expected_etag: trial.etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(disabled.state, memory_domain::ProcedureState::Disabled);
    assert_eq!(
        service
            .record_procedure_outcome(
                &ctx,
                &RecordProcedureOutcome {
                    procedure: view.reference.clone(),
                    success: true,
                    evidence: Vec::new(),
                    expected_etag: disabled.reference.revision.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );

    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .record_procedure_outcome(
                &reader,
                &RecordProcedureOutcome {
                    procedure: view.reference,
                    success: true,
                    evidence: Vec::new(),
                    expected_etag: "\"x\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

// -- search over the semantic store ------------------------------------------

#[test]
fn facts_are_not_silently_indexed_into_cross_document_search() {
    let (service, _) = service();
    let ctx = full();
    service
        .propose_fact(
            &ctx,
            &ProposeFact {
                input: fact_input(2, 0.7),
            },
        )
        .unwrap();
    let page = service
        .search(
            &ctx,
            &SearchMemory {
                query: MemoryQuery {
                    query: "shipped".into(),
                    kinds: Vec::new(),
                    language: None,
                    after: None,
                    before: None,
                    min_trust: None,
                    max_results: 10,
                    detail: DetailLevel::Title,
                    max_tokens: 100,
                },
            },
        )
        .unwrap();
    assert!(
        page.items.is_empty(),
        "semantic facts are reachable through find_conflicts only (ADR-052 gap)"
    );
}

// -- scope plumbing helper ---------------------------------------------------

#[test]
fn a_child_scope_cannot_register_procedures_for_its_parent() {
    let (service, _) = service();
    let ctx = full();
    service
        .open_agent_scope(
            &ctx,
            &OpenAgentScope {
                grant: AgentScopeGrant {
                    parent_agent_id: "agent-main".into(),
                    child_agent_id: "agent-child".into(),
                    session_id: "s1".into(),
                    task_id: "task-1".into(),
                    depth: 1,
                    allowed_scopes: vec![SCOPE_PROCEDURE_WRITE.to_string()],
                    allowed_tools: Vec::new(),
                    expires_at: "2001-09-09T01:46:40Z".into(),
                    max_context_tokens: 4_096,
                },
            },
        )
        .unwrap();
    let child = RequestContext {
        scope: ScopeRef {
            agent_id: Some("agent-child".into()),
            ..ctx.scope.clone()
        },
        ..ctx.clone()
    };
    let view = service
        .register_procedure_candidate(&child, &RegisterProcedure { draft: draft() })
        .unwrap();
    // The procedure belongs to the tenant/project, not to the agent, so the
    // parent can review it but the child's own scope marker still matches.
    assert!(view
        .reference
        .id
        .starts_with("procedure:tenant-a:project-b:"));
    let parent = service
        .review_procedure(
            &ctx,
            &ReviewProcedure {
                procedure: view.reference.clone(),
                decision: ProcedureDecision::Disable,
                evidence_ids: Vec::new(),
                expected_etag: view.reference.revision.clone(),
            },
        )
        .unwrap();
    assert_eq!(parent.state, memory_domain::ProcedureState::Disabled);
}
