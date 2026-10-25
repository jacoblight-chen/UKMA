//! Cross-cutting memory contracts: the §40.2 budget vectors, FSRS/TTL sweeps,
//! folding safety, eviction protection, paging integrity, reconsolidation CAS,
//! agent isolation and deletion invariants.

use std::sync::Arc;

use context_engine::{
    allocate, b_eff, compression_thresholds, next_compression_stage, CompressionStage, Reserves,
};
use memory_domain::domain::{
    AgentScopeGrant, BeginReconsolidation, BlockKind, ChangeReceipt, CommitReconsolidation,
    DetailLevel, EpisodeInput, EpisodeOutcome, EpisodeState, EpisodeType, EvaluateEviction,
    Evidence, FoldInput, FoldMemory, ForgetMemory, GetMemoryBudget, ListWorkingBlocks, MemoryKind,
    MemoryPatch, MemoryQuery, OpenAgentScope, PageIn, PageOut, PageRequest, PutWorkingBlock, Ref,
    RemoveWorkingBlock, RetrieveMemory, SearchMemory, StoreEpisode, UpdateMemory,
    WorkingBlockInput,
};
use memory_domain::fsrs::{
    active_ttl_hours, effective_active_until, folded_trust, on_failure, on_success, retention_gate,
    retrievability, retrievability_at, FsrsState, RetentionGate,
};
use memory_domain::scope::{ChildRetention, MAX_CONCURRENT, MAX_DEPTH, SUBAGENT_TIMEOUT_SECS};
use memory_domain::service::{
    FixedClock, MemoryService, POLICY_DETERMINISTIC, SCOPE_DELEGATE, SCOPE_FOLD, SCOPE_FORGET,
    SCOPE_MANAGE, SCOPE_READ, SCOPE_WRITE,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const ALL_SCOPES: [&str; 6] = [
    SCOPE_READ,
    SCOPE_WRITE,
    SCOPE_DELEGATE,
    SCOPE_FOLD,
    SCOPE_MANAGE,
    SCOPE_FORGET,
];

fn context(tenant: &str, agent: &str, session: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-1".into(),
        trace_id: "trace-1".into(),
        scope: ScopeRef {
            tenant_id: tenant.to_string(),
            project_id: "project-b".into(),
            repo_id: None,
            workspace_id: Some("task-1".into()),
            agent_id: Some(agent.to_string()),
            session_id: Some(session.to_string()),
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

fn root() -> RequestContext {
    context("tenant-a", "agent-main", "s1", &ALL_SCOPES)
}

fn fixed_clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::new(1_000_000_000))
}

fn service() -> (MemoryService, Arc<FixedClock>) {
    let clock = fixed_clock();
    let service = MemoryService::new()
        .with_reserves(Reserves::new(16_384, 4_096, 2_048, 1_024))
        .with_clock(clock.clone());
    (service, clock)
}

fn episode(summary: &str) -> EpisodeInput {
    EpisodeInput {
        session_id: "s1".into(),
        agent_id: "agent-main".into(),
        agent_depth: 0,
        episode_type: EpisodeType::ConversationTurn,
        summary: summary.to_string(),
        full_text_ref: None,
        key_facts: vec!["order shipped".to_string()],
        outcome: EpisodeOutcome::Success,
        source_refs: vec!["artifact:sha256:aa".to_string()],
        importance_score: 0.6,
        language: "en".into(),
        evidence: vec![Evidence {
            evidence_id: "e1".into(),
            source_ref: "artifact:sha256:aa".into(),
            confidence: 0.9,
            source_group: "doc-a".into(),
        }],
    }
}

fn block(content: &str) -> WorkingBlockInput {
    WorkingBlockInput {
        session_id: "s1".into(),
        agent_id: "agent-main".into(),
        block_type: BlockKind::ActiveContext,
        content: content.to_string(),
        language: "en".into(),
        priority: 0.5,
        protected: false,
        expires_after_turns: None,
    }
}

fn store(service: &MemoryService, ctx: &RequestContext, summary: &str) -> (String, String) {
    let receipt = service
        .store_episode(
            ctx,
            &StoreEpisode {
                input: episode(summary),
            },
        )
        .unwrap();
    (receipt.resource_id, receipt.etag)
}

fn title_query(text: &str) -> MemoryQuery {
    MemoryQuery {
        query: text.to_string(),
        kinds: Vec::new(),
        language: None,
        after: None,
        before: None,
        min_trust: None,
        max_results: 10,
        detail: DetailLevel::Title,
        max_tokens: 1_000,
    }
}

// -- §40.2 / §22.1 budget ---------------------------------------------------

#[test]
fn section_40_2_budget_vectors_hold() {
    assert_eq!(b_eff(65_536, 0).unwrap(), 65_536);
    assert_eq!(b_eff(65_536, 1).unwrap(), 16_384);
    assert_eq!(b_eff(65_536, 2).unwrap(), 4_096);
    assert_eq!(b_eff(131_072, 1).unwrap(), 32_768);
    assert_eq!(b_eff(32_768, 3).unwrap(), 4_096);
    assert!(b_eff(4_095, 0).is_err(), "4095 is below the floor");
}

#[test]
fn shares_always_sum_to_the_effective_budget() {
    for depth in 0..6u32 {
        for c_usable in [4_096u32, 8_192, 32_768, 65_536, 131_072] {
            let effective = b_eff(c_usable, depth).unwrap();
            let allocation = allocate(effective, depth);
            assert_eq!(
                allocation.wm + allocation.core + allocation.ring,
                effective,
                "depth {depth} c_usable {c_usable}"
            );
            assert!(allocation.wm > allocation.core);
            assert!(allocation.core > allocation.ring);
        }
    }
}

#[test]
fn the_five_compression_stages_appear_in_order() {
    let thresholds = compression_thresholds(0);
    assert_eq!(thresholds, &[80, 85, 90, 93, 95]);
    let mut seen: Vec<CompressionStage> = Vec::new();
    for utilisation in 0..=100u8 {
        if let Some(stage) = next_compression_stage(utilisation, 0) {
            if seen.last() != Some(&stage) {
                seen.push(stage);
            }
        }
    }
    assert_eq!(
        seen,
        vec![
            CompressionStage::RingReplacement,
            CompressionStage::EpisodeSummary,
            CompressionStage::Facts,
            CompressionStage::BlockFolding,
            CompressionStage::PolicyEviction,
        ]
    );
}

#[test]
fn child_scopes_get_the_depth_reduced_budget() {
    let (service, _) = service();
    let ctx = root();
    let view = service
        .open_agent_scope(
            &ctx,
            &OpenAgentScope {
                grant: AgentScopeGrant {
                    parent_agent_id: "agent-main".into(),
                    child_agent_id: "agent-child".into(),
                    session_id: "s1".into(),
                    task_id: "task-1".into(),
                    depth: 1,
                    allowed_scopes: vec![SCOPE_READ.to_string()],
                    allowed_tools: Vec::new(),
                    expires_at: "2001-09-09T01:46:40Z".into(),
                    max_context_tokens: 4_096,
                },
            },
        )
        .unwrap();
    assert!(view.scope_id.contains("agent-child"));

    let child = context("tenant-a", "agent-child", "s1", &ALL_SCOPES);
    let budget = service
        .get_budget(
            &child,
            &GetMemoryBudget {
                agent_id: "agent-child".into(),
                session_id: "s1".into(),
            },
        )
        .unwrap();
    assert_eq!(budget.agent_budget, b_eff(budget.available, 1).unwrap());
    assert!(budget.agent_budget < b_eff(budget.available, 0).unwrap());
}

// -- FSRS / TTL -------------------------------------------------------------

#[test]
fn fsrs_retrievability_refuses_skew_and_zero_stability() {
    let start = 1_000_000_000;
    assert_eq!(retrievability(0.0, 2.0).unwrap(), 1.0);
    assert!(retrievability_at(start, start + 86_400, 2.0).unwrap() < 1.0);
    assert!(retrievability_at(start, start - 1, 2.0).is_err(), "skew");
    assert!(retrievability(1.0, 0.0).is_err(), "zero stability");
}

#[test]
fn fsrs_success_grows_and_failure_shrinks_stability_within_bounds() {
    let state = FsrsState::default();
    let reviewed = on_success(state, 0.9).unwrap();
    assert!(reviewed.stability_days > state.stability_days);
    assert_eq!(reviewed.reviews, 1);
    assert_eq!(on_failure(state).unwrap().stability_days, 1.0);
    let mut capped = state;
    capped.stability_days = 3_650.0;
    assert_eq!(on_success(capped, 0.1).unwrap().stability_days, 3_650.0);
    let mut floored = state;
    floored.stability_days = 0.1;
    assert_eq!(on_failure(floored).unwrap().stability_days, 0.1);
}

#[test]
fn retention_gates_thresholds_and_active_ttl() {
    assert_eq!(
        retention_gate(0.29).unwrap(),
        RetentionGate::DeleteCandidate
    );
    assert_eq!(
        retention_gate(0.30).unwrap(),
        RetentionGate::CompressCandidate
    );
    assert_eq!(retention_gate(0.70).unwrap(), RetentionGate::Keep);
    assert_eq!(active_ttl_hours(0), 72.0);
    assert_eq!(active_ttl_hours(1), 36.0);
    assert_eq!(active_ttl_hours(2), 18.0);
    assert_eq!(active_ttl_hours(9), 1.0);
    let created = 1_000;
    assert_eq!(
        effective_active_until(created, None, 2),
        created + 18 * 3_600
    );
    assert_eq!(
        effective_active_until(created, Some(created + 600), 0),
        created + 600 + 3_600
    );
    let horizons = ChildRetention::after_completion(created);
    assert_eq!(horizons.archive_after, created + 24 * 3_600);
    assert_eq!(horizons.purge_eligible_after, created + 7 * 86_400);
    assert_eq!(MAX_DEPTH, 3);
    assert_eq!(MAX_CONCURRENT, 10);
    assert_eq!(SUBAGENT_TIMEOUT_SECS, 300);
}

#[test]
fn ttl_sweep_archives_then_purges_a_record() {
    let (service, clock) = service();
    let ctx = root();
    let (id, _) = store(&service, &ctx, "alpha order shipped");

    let fresh = service.sweep(&[], true).unwrap();
    assert_eq!(fresh[0].1.action, memory_domain::SweepAction::Keep);

    // Past the 72h active window: archived, not purged.
    clock.advance(73 * 3_600);
    let archived = service.sweep(&[], true).unwrap();
    assert_eq!(
        archived[0].1.action,
        memory_domain::SweepAction::Archive,
        "the active window closed but the purge horizon has not been reached"
    );

    // Past the 30-day purge horizon with every guard satisfied.
    clock.advance(31 * 86_400);
    let purged = service.sweep(&[], true).unwrap();
    assert_eq!(purged[0].1.action, memory_domain::SweepAction::Purge);

    // The record is no longer retrievable and a second sweep is a no-op.
    assert_eq!(
        service
            .retrieve(
                &ctx,
                &RetrieveMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, "\"x\"".to_string()),
                    detail: DetailLevel::Full,
                    max_tokens: 100,
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
    assert!(service.sweep(&[], true).unwrap().is_empty());
}

#[test]
fn ttl_sweep_respects_holds_and_regulation() {
    let (service, clock) = service();
    let ctx = root();
    let (held, _) = store(&service, &ctx, "alpha order shipped");
    service
        .set_episode_protection(&ctx, &held, false, true)
        .unwrap();
    clock.advance(31 * 86_400);

    let decisions = service.sweep(&[], true).unwrap();
    let held_decision = decisions
        .iter()
        .find(|(id, _)| id == &held)
        .map(|(_, decision)| decision.clone())
        .expect("the held record is reported");
    assert_ne!(held_decision.action, memory_domain::SweepAction::Purge);

    let denied = service.sweep(&[], false).unwrap();
    assert!(denied
        .iter()
        .all(|(_, decision)| decision.action != memory_domain::SweepAction::Purge));
}

#[test]
fn privacy_delete_overrides_retention_but_not_a_legal_hold() {
    let (service, clock) = service();
    let ctx = root();
    let (plain, _) = store(&service, &ctx, "alpha order shipped");
    let (held, _) = store(&service, &ctx, "beta order shipped");
    service
        .set_episode_protection(&ctx, &held, false, true)
        .unwrap();

    let decisions = service.sweep(&[plain.clone(), held.clone()], true).unwrap();
    let plain_decision = decisions.iter().find(|(id, _)| id == &plain).unwrap();
    assert_eq!(
        plain_decision.1.action,
        memory_domain::SweepAction::Purge,
        "a privacy delete does not wait for the retention horizon"
    );
    let held_decision = decisions.iter().find(|(id, _)| id == &held).unwrap();
    assert_ne!(
        held_decision.1.action,
        memory_domain::SweepAction::Purge,
        "a legal hold still wins"
    );
    clock.advance(1);
}

// -- folding ----------------------------------------------------------------

#[test]
fn folding_levels_and_trust_decay_follow_the_registered_boundaries() {
    use context_engine::{classify, decide, FoldLevel, PreserveRule};
    assert_eq!(classify(0.9), FoldLevel::Preserve);
    assert_eq!(classify(0.7), FoldLevel::Summarize);
    assert_eq!(classify(0.31), FoldLevel::Summarize);
    assert_eq!(classify(0.3), FoldLevel::Fold);
    assert_eq!(classify(0.0), FoldLevel::Fold);

    let forced = decide(0.1, &[PreserveRule::UserPin], Some(0.05));
    assert_eq!(forced.level, FoldLevel::Preserve);
    assert!(forced.forced_preserve);
    assert_eq!(forced.preserve_rule, Some(PreserveRule::UserPin));

    let modelled = decide(0.1, &[], Some(0.95));
    assert_eq!(modelled.level, FoldLevel::Preserve);
    assert!(!modelled.forced_preserve);

    let fallback = decide(0.85, &[], None);
    assert_eq!(fallback.level, FoldLevel::Preserve);
    assert_eq!(
        fallback.fallback,
        Some(context_engine::FallbackReason::ModelAbsent)
    );

    for level in 1..4u32 {
        assert!(folded_trust(1.0, level) > folded_trust(1.0, level + 1));
    }
    assert_eq!(folded_trust(1.0, 0), 1.0);
}

#[test]
fn fold_keeps_every_evidence_reference_across_a_chain() {
    let (service, _) = service();
    let ctx = root();
    let (first, _) = store(&service, &ctx, "alpha order shipped");
    let (second, _) = store(&service, &ctx, "beta order signed");
    let once = service
        .fold(
            &ctx,
            &FoldMemory {
                input: FoldInput {
                    episode_refs: vec![
                        Ref::new(first, MemoryKind::Episodic, "\"a\""),
                        Ref::new(second, MemoryKind::Episodic, "\"b\""),
                    ],
                    policy_id: "policy:default".into(),
                    max_tokens: 100,
                    target_agent_id: "agent-main".into(),
                },
                model_score: Some(0.5),
            },
        )
        .unwrap();
    assert!(!once.forced_preserve);
    assert_eq!(once.protected_evidence_refs, vec!["artifact:sha256:aa"]);
    assert!(once.trust_score > 0.0 && once.trust_score < 0.5);
}

// -- eviction ---------------------------------------------------------------

#[test]
fn eviction_never_touches_pinned_or_task_state_blocks() {
    let (service, _) = service();
    let ctx = root();
    let pinned = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    protected: true,
                    ..block("pinned")
                },
                automatic: false,
            },
        )
        .unwrap();
    let task = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    block_type: BlockKind::TaskState,
                    ..block("task")
                },
                automatic: false,
            },
        )
        .unwrap();
    let scratch = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block("scratch"),
                automatic: false,
            },
        )
        .unwrap();

    let plan = service
        .evaluate_eviction(
            &ctx,
            &EvaluateEviction {
                agent_id: "agent-main".into(),
                session_id: "s1".into(),
                policy_id: POLICY_DETERMINISTIC.into(),
                dry_run: true,
                required_tokens: 10,
            },
        )
        .unwrap();
    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.selected[0].id, scratch.id);
    let protected: Vec<&str> = plan
        .protected
        .iter()
        .map(|reference| reference.id.as_str())
        .collect();
    assert!(protected.contains(&pinned.id.as_str()));
    assert!(protected.contains(&task.id.as_str()));
}

// -- paging -----------------------------------------------------------------

#[test]
fn paging_round_trip_keeps_the_integrity_hash_and_survives_repeats() {
    let (service, _) = service();
    let ctx = root();
    let reference = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block("page me twice"),
                automatic: false,
            },
        )
        .unwrap();
    let receipt = service
        .page_out(
            &ctx,
            &PageOut {
                block: reference,
                policy_id: "page-policy".into(),
            },
        )
        .unwrap();
    let page = receipt.page_ref.clone().expect("a page");
    assert_eq!(receipt.integrity_hash, page.revision);

    for _ in 0..2 {
        let restored = service
            .page_in(
                &ctx,
                &PageIn {
                    page_ref: page.clone(),
                    expected_hash: receipt.integrity_hash.clone(),
                },
            )
            .unwrap();
        assert_eq!(restored.receipt.integrity_hash, receipt.integrity_hash);
        assert_eq!(restored.block.summary.as_deref(), Some("page me twice"));
    }
}

// -- reconsolidation --------------------------------------------------------

#[test]
fn a_concurrent_edit_makes_the_window_commit_fail_without_overwriting() {
    let (service, _) = service();
    let ctx = root();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let window = service
        .begin_reconsolidation(
            &ctx,
            &BeginReconsolidation {
                fact: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                expected_etag: etag.clone(),
                window_hours: None,
            },
        )
        .unwrap();

    // Someone else commits a revision while the window is open.
    let external: ChangeReceipt = service
        .update_revision(
            &ctx,
            &UpdateMemory {
                memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                patch: MemoryPatch {
                    summary: Some("external edit".into()),
                    ..MemoryPatch::default()
                },
                expected_etag: etag.clone(),
            },
        )
        .unwrap();
    assert!(external.changed);

    let conflict = service
        .commit_reconsolidation(
            &ctx,
            &CommitReconsolidation {
                window_id: window.window_id,
                patch: MemoryPatch {
                    summary: Some("window edit".into()),
                    ..MemoryPatch::default()
                },
                evidence: vec![Evidence {
                    evidence_id: "e2".into(),
                    source_ref: "artifact:sha256:bb".into(),
                    confidence: 0.8,
                    source_group: "doc-b".into(),
                }],
                expected_etag: etag,
            },
        )
        .unwrap_err();
    assert_eq!(conflict.code(), "ETAG_MISMATCH");

    let view = service
        .retrieve(
            &ctx,
            &RetrieveMemory {
                memory: Ref::new(id, MemoryKind::Episodic, external.etag),
                detail: DetailLevel::Summary,
                max_tokens: 100,
            },
        )
        .unwrap();
    assert_eq!(view.summary.as_deref(), Some("external edit"));
}

// -- agent isolation --------------------------------------------------------

#[test]
fn a_child_agent_cannot_read_its_parents_episodes() {
    let (service, _) = service();
    let parent = root();
    let (id, etag) = store(&service, &parent, "alpha parent secret");
    service
        .open_agent_scope(
            &parent,
            &OpenAgentScope {
                grant: AgentScopeGrant {
                    parent_agent_id: "agent-main".into(),
                    child_agent_id: "agent-child".into(),
                    session_id: "s1".into(),
                    task_id: "task-1".into(),
                    depth: 1,
                    allowed_scopes: vec![SCOPE_READ.to_string()],
                    allowed_tools: Vec::new(),
                    expires_at: "2001-09-09T01:46:40Z".into(),
                    max_context_tokens: 4_096,
                },
            },
        )
        .unwrap();
    let child = context("tenant-a", "agent-child", "s1", &ALL_SCOPES);

    assert_eq!(
        service
            .retrieve(
                &child,
                &RetrieveMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, etag),
                    detail: DetailLevel::Full,
                    max_tokens: 100,
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
    let page = service
        .search(
            &child,
            &SearchMemory {
                query: title_query("alpha"),
            },
        )
        .unwrap();
    assert!(page.items.is_empty(), "the parent episode is invisible");
}

#[test]
fn another_session_cannot_read_episodic_memory() {
    let (service, _) = service();
    let ctx = root();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let other_session = context("tenant-a", "agent-main", "s2", &ALL_SCOPES);
    assert_eq!(
        service
            .retrieve(
                &other_session,
                &RetrieveMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, etag),
                    detail: DetailLevel::Summary,
                    max_tokens: 100,
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// -- deletion ---------------------------------------------------------------

#[test]
fn a_deletion_revokes_projections_only_and_reports_a_backup_expiry() {
    let (service, _) = service();
    let ctx = root();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let receipt = service
        .forget(
            &ctx,
            &ForgetMemory {
                memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                reason: "gdpr".into(),
                expected_etag: etag,
            },
        )
        .unwrap();
    for projection in &receipt.projections {
        assert!(!memory_domain::deletion::NEVER_REVOKED.contains(&projection.name.as_str()));
    }
    assert_eq!(
        receipt.projections.len(),
        memory_domain::deletion::PROJECTIONS.len()
    );
    assert_eq!(
        receipt.cache_invalidation,
        memory_domain::domain::CacheInvalidation::Complete
    );
    assert!(receipt.backup_expiry_at.is_some());
    assert!(!receipt.legal_hold);
}

#[test]
fn a_tombstoned_record_cannot_be_updated_or_reforgotten() {
    let (service, _) = service();
    let ctx = root();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    service
        .forget(
            &ctx,
            &ForgetMemory {
                memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                reason: "gdpr".into(),
                expected_etag: etag.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .update_revision(
                &ctx,
                &UpdateMemory {
                    memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                    patch: MemoryPatch {
                        summary: Some("resurrect".into()),
                        ..MemoryPatch::default()
                    },
                    expected_etag: etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
    assert_eq!(
        service
            .forget(
                &ctx,
                &ForgetMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, etag.clone()),
                    reason: "again".into(),
                    expected_etag: etag,
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
}

#[test]
fn working_blocks_stay_inside_the_budget_after_removal() {
    let (service, _) = service();
    let ctx = root();
    let reference = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block("three tokens here"),
                automatic: false,
            },
        )
        .unwrap();
    let before = service
        .get_budget(
            &ctx,
            &GetMemoryBudget {
                agent_id: "agent-main".into(),
                session_id: "s1".into(),
            },
        )
        .unwrap();
    assert!(before.used > 0);

    service
        .remove_working_block(
            &ctx,
            &RemoveWorkingBlock {
                block: reference.clone(),
                expected_etag: reference.revision.clone(),
            },
        )
        .unwrap();
    let after = service
        .get_budget(
            &ctx,
            &GetMemoryBudget {
                agent_id: "agent-main".into(),
                session_id: "s1".into(),
            },
        )
        .unwrap();
    assert_eq!(after.used, 0);
    let listed = service
        .list_working_blocks(
            &ctx,
            &ListWorkingBlocks {
                session_id: "s1".into(),
                agent_id: "agent-main".into(),
                page: PageRequest::new(10),
            },
        )
        .unwrap();
    assert!(listed.items.is_empty());
}

#[test]
fn episodic_state_machine_is_reachable_only_through_legal_moves() {
    assert!(EpisodeState::Active.is_live());
    assert!(EpisodeState::Compressed.is_live());
    assert!(EpisodeState::Archived.is_live());
    assert!(
        !EpisodeState::Held.is_live(),
        "a hold is not a TTL state: the sweep must leave it alone"
    );
    assert!(!EpisodeState::Tombstoned.is_live());
    assert!(!EpisodeState::Purged.is_live());
}
