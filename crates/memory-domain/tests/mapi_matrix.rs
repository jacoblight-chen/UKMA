//! MAPI-01..09, 13..17, 23..25 operation contract tests (§34.2, §37).
//!
//! Every operation is exercised with a positive path plus the failure paths the
//! registry declares: authorization, cross-scope, optimistic concurrency,
//! idempotency and the capability gap of the P6 operations.

use std::sync::Arc;

use context_engine::Reserves;
use memory_domain::domain::{
    AgentScopeGrant, AgentStatus, BeginReconsolidation, BlockKind, CloseAgentScope,
    CommitReconsolidation, Consolidate, DetailLevel, EpisodeInput, EpisodeOutcome, EpisodeType,
    EvaluateEviction, Evidence, FoldInput, FoldMemory, ForgetMemory, GetMemoryBudget,
    ListWorkingBlocks, MemoryKind, MemoryPatch, MemoryQuery, OpenAgentScope, PageIn, PageOut,
    PageRequest, PagingStatus, ProjectionState, PutWorkingBlock, Ref, RemoveWorkingBlock,
    RetrieveMemory, SearchMemory, StoreEpisode, UpdateMemory, WindowState, WorkingBlockInput,
};
use memory_domain::scope::ParentAuthority;
use memory_domain::service::{
    DelegationAuthority, FixedClock, FractalMemoryPort, MemoryService, POLICY_DETERMINISTIC,
    POLICY_SHADOW_RL, SCOPE_CONSOLIDATE, SCOPE_DELEGATE, SCOPE_FOLD, SCOPE_FORGET, SCOPE_MANAGE,
    SCOPE_PROCEDURE_REVIEW, SCOPE_PROCEDURE_WRITE, SCOPE_READ, SCOPE_REVIEW, SCOPE_WRITE,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const ALL_SCOPES: [&str; 10] = [
    SCOPE_READ,
    SCOPE_WRITE,
    SCOPE_DELEGATE,
    SCOPE_FOLD,
    SCOPE_MANAGE,
    SCOPE_FORGET,
    SCOPE_REVIEW,
    SCOPE_CONSOLIDATE,
    SCOPE_PROCEDURE_WRITE,
    SCOPE_PROCEDURE_REVIEW,
];

fn context(tenant: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-1".into(),
        trace_id: "trace-1".into(),
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

fn block_input(content: &str) -> WorkingBlockInput {
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

fn episode_input(summary: &str) -> EpisodeInput {
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

fn grant_template() -> AgentScopeGrant {
    AgentScopeGrant {
        parent_agent_id: "agent-main".into(),
        child_agent_id: "agent-child".into(),
        session_id: "s1".into(),
        task_id: "task-1".into(),
        depth: 1,
        allowed_scopes: vec![SCOPE_READ.to_string()],
        allowed_tools: Vec::new(),
        expires_at: "2001-09-09T01:46:40Z".into(),
        max_context_tokens: 4_096,
    }
}

fn evidence(id: &str) -> Evidence {
    Evidence {
        evidence_id: id.to_string(),
        source_ref: format!("artifact:sha256:{id}"),
        confidence: 0.8,
        source_group: "doc-b".to_string(),
    }
}

fn store(service: &MemoryService, ctx: &RequestContext, summary: &str) -> (String, String) {
    let receipt = service
        .store_episode(
            ctx,
            &StoreEpisode {
                input: episode_input(summary),
            },
        )
        .expect("episode should be stored");
    (receipt.resource_id, receipt.etag)
}

fn search_query(text: &str) -> MemoryQuery {
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

/// Authority that holds a tool, used to prove the positive delegation path.
struct WideAuthority;

impl DelegationAuthority for WideAuthority {
    fn authority(&self, ctx: &RequestContext) -> ParentAuthority {
        ParentAuthority {
            tenant_id: ctx.scope.tenant_id.clone(),
            agent_instance_id: "agent-main".into(),
            depth: 0,
            scopes: ctx.scopes.clone(),
            tools: vec!["read_file".into()],
            max_context_tokens: 8_192,
            expires_at: i64::MAX,
            remaining_tokens: 8_192,
        }
    }
}

/// Authority whose own grant expires at the fixed clock instant.
struct BoundedAuthority;

impl DelegationAuthority for BoundedAuthority {
    fn authority(&self, ctx: &RequestContext) -> ParentAuthority {
        ParentAuthority {
            tenant_id: ctx.scope.tenant_id.clone(),
            agent_instance_id: "agent-main".into(),
            depth: 0,
            scopes: ctx.scopes.clone(),
            tools: Vec::new(),
            max_context_tokens: 8_192,
            expires_at: 1_000_000_000,
            remaining_tokens: 8_192,
        }
    }
}

// -- MAPI-01 ----------------------------------------------------------------

#[test]
fn mapi01_open_agent_scope_requires_the_delegate_scope() {
    let (service, _) = service();
    let ctx = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .open_agent_scope(
                &ctx,
                &OpenAgentScope {
                    grant: grant_template()
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

#[test]
fn mapi01_open_agent_scope_opens_and_cannot_escalate() {
    let (service, _) = service();
    let ctx = full();
    let request = OpenAgentScope {
        grant: grant_template(),
    };
    let view = service.open_agent_scope(&ctx, &request).unwrap();
    assert!(view
        .scope_id
        .starts_with("scope:tenant-a:agent-child:task-1"));
    assert_eq!(view.expires_at, "2001-09-09T01:46:40Z");
    assert!(service.scope_opened_at(&view.scope_id).is_some());

    assert_eq!(
        service.open_agent_scope(&ctx, &request).unwrap_err().code(),
        "LINK_STATE_CONFLICT"
    );

    let mut escalated = grant_template();
    escalated.task_id = "task-2".into();
    escalated.allowed_scopes = vec!["memory:admin".to_string()];
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: escalated })
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    // The default authority holds no tools, so tool delegation cannot be
    // verified and is refused rather than assumed.
    let mut tool = grant_template();
    tool.task_id = "task-3".into();
    tool.allowed_tools = vec!["read_file".into()];
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: tool })
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );
}

#[test]
fn mapi01_tool_delegation_works_when_the_parent_holds_the_tool() {
    let clock = fixed_clock();
    let service = MemoryService::new()
        .with_reserves(Reserves::new(16_384, 4_096, 2_048, 1_024))
        .with_clock(clock)
        .with_delegation(Arc::new(WideAuthority));
    let ctx = full();
    let mut grant = grant_template();
    grant.allowed_tools = vec!["read_file".into()];
    grant.max_context_tokens = 4_096;
    assert!(service
        .open_agent_scope(&ctx, &OpenAgentScope { grant })
        .is_ok());
}

#[test]
fn mapi01_grants_cannot_outlive_a_bounded_parent() {
    let clock = fixed_clock();
    let service = MemoryService::new()
        .with_reserves(Reserves::new(16_384, 4_096, 2_048, 1_024))
        .with_clock(clock)
        .with_delegation(Arc::new(BoundedAuthority));
    let ctx = full();
    let mut far = grant_template();
    far.expires_at = "2999-01-01T00:00:00Z".into();
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: far })
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );
}

#[test]
fn mapi01_open_agent_scope_rejects_bad_depth_budget_and_expiry() {
    let clock = fixed_clock();
    let service = MemoryService::new()
        .with_reserves(Reserves::new(16_384, 4_096, 2_048, 1_024))
        .with_clock(clock);
    let ctx = full();

    let mut deep = grant_template();
    deep.depth = 4;
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: deep })
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    let mut rich = grant_template();
    rich.max_context_tokens = 9_999;
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: rich })
            .unwrap_err()
            .code(),
        "CONTEXT_BUDGET_EXCEEDED"
    );

    let mut broken = grant_template();
    broken.expires_at = "soon".into();
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: broken })
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    let mut no_task = grant_template();
    no_task.task_id = " ".into();
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: no_task })
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

#[test]
fn mapi01_concurrent_subagents_are_capped() {
    let (service, _) = service();
    let ctx = full();
    for index in 0..memory_domain::scope::MAX_CONCURRENT {
        let mut grant = grant_template();
        grant.task_id = format!("task-{index}");
        grant.child_agent_id = format!("agent-child-{index}");
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant })
            .expect("the first ten scopes open");
    }
    let mut extra = grant_template();
    extra.task_id = "task-extra".into();
    extra.child_agent_id = "agent-extra".into();
    assert_eq!(
        service
            .open_agent_scope(&ctx, &OpenAgentScope { grant: extra })
            .unwrap_err()
            .code(),
        "RESOURCE_EXHAUSTED"
    );
}

// -- MAPI-02 ----------------------------------------------------------------

#[test]
fn mapi02_close_agent_scope_seals_and_reports_status() {
    let (service, _) = service();
    let ctx = full();
    let view = service
        .open_agent_scope(
            &ctx,
            &OpenAgentScope {
                grant: grant_template(),
            },
        )
        .unwrap();
    let closed = service
        .close_agent_scope(
            &ctx,
            &CloseAgentScope {
                scope_id: view.scope_id.clone(),
                reason: "task finished".into(),
                fold_on_close: false,
            },
        )
        .unwrap();
    assert!(closed.closed);
    assert_eq!(closed.status, AgentStatus::Sealed);
    assert_eq!(
        service
            .transition_scope(&ctx, &view.scope_id, AgentStatus::Sealed)
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );

    let again = service
        .close_agent_scope(
            &ctx,
            &CloseAgentScope {
                scope_id: view.scope_id,
                reason: "again".into(),
                fold_on_close: false,
            },
        )
        .unwrap();
    assert!(!again.closed);
}

#[test]
fn mapi02_close_agent_scope_rejects_other_tenants_and_empty_reasons() {
    let (service, _) = service();
    let ctx = full();
    let view = service
        .open_agent_scope(
            &ctx,
            &OpenAgentScope {
                grant: grant_template(),
            },
        )
        .unwrap();

    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        service
            .close_agent_scope(
                &other,
                &CloseAgentScope {
                    scope_id: view.scope_id.clone(),
                    reason: "x".into(),
                    fold_on_close: false,
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
    assert_eq!(
        service
            .close_agent_scope(
                &ctx,
                &CloseAgentScope {
                    scope_id: view.scope_id.clone(),
                    reason: " ".into(),
                    fold_on_close: false,
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .close_agent_scope(
                &ctx,
                &CloseAgentScope {
                    scope_id: "scope:nope".into(),
                    reason: "x".into(),
                    fold_on_close: false,
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
}

#[test]
fn mapi02_close_agent_scope_can_hand_over_a_folded_result_only() {
    let (service, _) = service();
    let ctx = full();
    let child = context("tenant-a", &ALL_SCOPES);
    let child_ctx = RequestContext {
        scope: ScopeRef {
            agent_id: Some("agent-child".into()),
            ..child.scope.clone()
        },
        ..child
    };
    let view = service
        .open_agent_scope(
            &ctx,
            &OpenAgentScope {
                grant: grant_template(),
            },
        )
        .unwrap();
    service
        .store_episode(
            &child_ctx,
            &StoreEpisode {
                input: EpisodeInput {
                    agent_id: "agent-child".into(),
                    ..episode_input("child worked on alpha")
                },
            },
        )
        .unwrap();
    let closed = service
        .close_agent_scope(
            &ctx,
            &CloseAgentScope {
                scope_id: view.scope_id,
                reason: "done".into(),
                fold_on_close: true,
            },
        )
        .unwrap();
    assert!(closed.closed);
    let folded = closed.folded_result.expect("a folded hand-over");
    assert_eq!(folded.source_refs.len(), 1);
    assert!(closed.evidence_refs.len() <= folded.protected_evidence_refs.len());
}

// -- MAPI-03 ----------------------------------------------------------------

#[test]
fn mapi03_put_working_block_writes_inside_the_budget() {
    let (service, _) = service();
    let ctx = full();
    let reference = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("task state alpha"),
                automatic: false,
            },
        )
        .unwrap();
    assert_eq!(reference.kind, MemoryKind::Working);
    assert!(reference.revision.starts_with('"'));
    assert_eq!(service.block_is_automatic(&reference.id), Some(false));
    assert_eq!(service.block_turn(&reference.id), Some(0));
    assert!(service.block_evidence(&reference.id).is_empty());
}

#[test]
fn mapi03_put_working_block_enforces_the_budget_and_validation() {
    let (service, _) = service();
    let ctx = full();
    // 6000 tokens exceed the 5529-token working-memory share of this profile.
    let big = vec!["token"; 6_000].join(" ");
    assert_eq!(
        service
            .put_working_block(
                &ctx,
                &PutWorkingBlock {
                    input: block_input(&big),
                    automatic: false,
                }
            )
            .unwrap_err()
            .code(),
        "CONTEXT_BUDGET_EXCEEDED"
    );

    for (input, field) in [
        (
            WorkingBlockInput {
                content: "  ".into(),
                ..block_input("x")
            },
            "input.content",
        ),
        (
            WorkingBlockInput {
                priority: 1.5,
                ..block_input("x")
            },
            "input.priority",
        ),
        (
            WorkingBlockInput {
                language: String::new(),
                ..block_input("x")
            },
            "input.language",
        ),
    ] {
        let error = service
            .put_working_block(
                &ctx,
                &PutWorkingBlock {
                    input,
                    automatic: false,
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), "INVALID_ARGUMENT");
        assert!(error.to_string().contains(field), "{error}");
    }
}

#[test]
fn mapi03_put_working_block_refuses_another_sandbox() {
    let (service, _) = service();
    let ctx = full();
    let other = WorkingBlockInput {
        session_id: "s-other".into(),
        ..block_input("alpha")
    };
    assert_eq!(
        service
            .put_working_block(
                &ctx,
                &PutWorkingBlock {
                    input: other,
                    automatic: false,
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

#[test]
fn mapi03_automatic_blocks_are_capped_per_turn_and_expire_by_turn() {
    let (service, _) = service();
    let ctx = full();
    for index in 0..memory_domain::domain::MAX_BLOCKS_PER_TURN {
        service
            .put_working_block(
                &ctx,
                &PutWorkingBlock {
                    input: block_input(&format!("auto {index}")),
                    automatic: true,
                },
            )
            .expect("within the per-turn cap");
    }
    assert_eq!(
        service
            .put_working_block(
                &ctx,
                &PutWorkingBlock {
                    input: block_input("auto overflow"),
                    automatic: true,
                }
            )
            .unwrap_err()
            .code(),
        "RESOURCE_LIMIT"
    );

    let expiring = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    expires_after_turns: Some(1),
                    ..block_input("expires next turn")
                },
                automatic: false,
            },
        )
        .unwrap();
    service.begin_turn("s1", "agent-main");
    let listed = service
        .list_working_blocks(
            &ctx,
            &ListWorkingBlocks {
                session_id: "s1".into(),
                agent_id: "agent-main".into(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert!(listed
        .items
        .iter()
        .all(|view| view.reference.id != expiring.id));
}

// -- MAPI-04 ----------------------------------------------------------------

#[test]
fn mapi04_list_working_blocks_pages_within_the_sandbox() {
    let (service, _) = service();
    let ctx = full();
    for index in 0..3 {
        service
            .put_working_block(
                &ctx,
                &PutWorkingBlock {
                    input: block_input(&format!("block {index}")),
                    automatic: false,
                },
            )
            .unwrap();
    }
    let first = service
        .list_working_blocks(
            &ctx,
            &ListWorkingBlocks {
                session_id: "s1".into(),
                agent_id: "agent-main".into(),
                page: PageRequest::new(2),
            },
        )
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let cursor = first.next_cursor.expect("a second page exists");
    let second = service
        .list_working_blocks(
            &ctx,
            &ListWorkingBlocks {
                session_id: "s1".into(),
                agent_id: "agent-main".into(),
                page: PageRequest {
                    cursor: Some(cursor),
                    limit: 2,
                },
            },
        )
        .unwrap();
    assert_eq!(second.items.len(), 1);

    assert_eq!(
        service
            .list_working_blocks(
                &ctx,
                &ListWorkingBlocks {
                    session_id: "s1".into(),
                    agent_id: "agent-main".into(),
                    page: PageRequest::new(0),
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .list_working_blocks(
                &ctx,
                &ListWorkingBlocks {
                    session_id: "s-other".into(),
                    agent_id: "agent-main".into(),
                    page: PageRequest::new(5),
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// -- MAPI-05 ----------------------------------------------------------------

#[test]
fn mapi05_remove_working_block_deletes_and_returns_a_receipt() {
    let (service, _) = service();
    let ctx = full();
    let reference = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("temporary"),
                automatic: false,
            },
        )
        .unwrap();
    let receipt = service
        .remove_working_block(
            &ctx,
            &RemoveWorkingBlock {
                block: reference.clone(),
                expected_etag: reference.revision.clone(),
            },
        )
        .unwrap();
    assert!(receipt.tombstone_revision.starts_with("tombstone:"));
    assert!(!receipt.legal_hold);
    assert!(receipt
        .projections
        .iter()
        .all(|projection| projection.name != "source_artifact"));
    assert!(receipt
        .projections
        .iter()
        .all(|projection| projection.state == ProjectionState::Cleared));
}

#[test]
fn mapi05_remove_working_block_refuses_protected_blocks_and_stale_etags() {
    let (service, _) = service();
    let ctx = full();
    let protected = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    protected: true,
                    ..block_input("pinned")
                },
                automatic: false,
            },
        )
        .unwrap();
    assert_eq!(
        service
            .remove_working_block(
                &ctx,
                &RemoveWorkingBlock {
                    block: protected.clone(),
                    expected_etag: protected.revision.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    let ordinary = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("ordinary"),
                automatic: false,
            },
        )
        .unwrap();
    assert_eq!(
        service
            .remove_working_block(
                &ctx,
                &RemoveWorkingBlock {
                    block: ordinary.clone(),
                    expected_etag: "\"stale\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );
    assert_eq!(
        service
            .remove_working_block(
                &ctx,
                &RemoveWorkingBlock {
                    block: Ref::new("wmb:nope", MemoryKind::Working, "\"x\""),
                    expected_etag: "\"x\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
}

// -- MAPI-06 ----------------------------------------------------------------

#[test]
fn mapi06_store_episode_is_idempotent_and_reports_conflicts() {
    let (service, _) = service();
    let mut ctx = full();
    ctx.idempotency_key = Some("idem-1".into());
    let first = service
        .store_episode(
            &ctx,
            &StoreEpisode {
                input: episode_input("first"),
            },
        )
        .unwrap();
    assert!(first.changed);
    assert_eq!(first.events, vec!["memory.episode.committed"]);

    let replay = service
        .store_episode(
            &ctx,
            &StoreEpisode {
                input: episode_input("first"),
            },
        )
        .unwrap();
    assert!(!replay.changed);
    assert_eq!(replay.resource_id, first.resource_id);

    assert_eq!(
        service
            .store_episode(
                &ctx,
                &StoreEpisode {
                    input: episode_input("different"),
                }
            )
            .unwrap_err()
            .code(),
        "IDEMPOTENCY_CONFLICT"
    );
}

#[test]
fn mapi06_store_episode_validates_input_and_depth() {
    let (service, _) = service();
    let ctx = full();
    for (input, field) in [
        (
            EpisodeInput {
                summary: " ".into(),
                ..episode_input("x")
            },
            "input.summary",
        ),
        (
            EpisodeInput {
                importance_score: 2.0,
                ..episode_input("x")
            },
            "input.importance_score",
        ),
    ] {
        let error = service
            .store_episode(&ctx, &StoreEpisode { input })
            .unwrap_err();
        assert_eq!(error.code(), "INVALID_ARGUMENT");
        assert!(error.to_string().contains(field), "{error}");
    }

    let deep = EpisodeInput {
        agent_depth: 9,
        ..episode_input("too deep")
    };
    assert_eq!(
        service
            .store_episode(&ctx, &StoreEpisode { input: deep })
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    let unauthorised = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .store_episode(
                &unauthorised,
                &StoreEpisode {
                    input: episode_input("x")
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

// -- MAPI-07 ----------------------------------------------------------------

#[test]
fn mapi07_search_ranks_live_records_and_hides_other_tenants() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "alpha order shipped");
    store(&service, &ctx, "beta unrelated");

    let other = context("tenant-z", &ALL_SCOPES);
    store(&service, &other, "alpha from another tenant");

    let page = service
        .search(
            &ctx,
            &SearchMemory {
                query: MemoryQuery {
                    kinds: vec![MemoryKind::Episodic],
                    detail: DetailLevel::Summary,
                    ..search_query("alpha")
                },
            },
        )
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, MemoryKind::Episodic);
    assert!(page.items[0].content.is_none(), "summary has no body");
}

#[test]
fn mapi07_search_validates_the_query() {
    let (service, _) = service();
    let ctx = full();
    assert_eq!(
        service
            .search(
                &ctx,
                &SearchMemory {
                    query: MemoryQuery {
                        query: " ".into(),
                        ..search_query("x")
                    }
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .search(
                &ctx,
                &SearchMemory {
                    query: MemoryQuery {
                        max_results: 0,
                        ..search_query("alpha")
                    }
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        service
            .search(
                &ctx,
                &SearchMemory {
                    query: MemoryQuery {
                        after: Some("not-a-date".into()),
                        ..search_query("alpha")
                    }
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

// -- MAPI-08 ----------------------------------------------------------------

#[test]
fn mapi08_retrieve_honours_detail_and_the_token_ceiling() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let reference = Ref::new(id.clone(), MemoryKind::Episodic, etag);

    let full_view = service
        .retrieve(
            &ctx,
            &RetrieveMemory {
                memory: reference.clone(),
                detail: DetailLevel::Full,
                max_tokens: 1_000,
            },
        )
        .unwrap();
    assert!(full_view.content.is_some());
    assert!(!full_view.redacted);

    let title = service
        .retrieve(
            &ctx,
            &RetrieveMemory {
                memory: reference.clone(),
                detail: DetailLevel::Title,
                max_tokens: 1_000,
            },
        )
        .unwrap();
    assert!(title.summary.is_none());
    assert!(title.content.is_none());

    assert_eq!(
        service
            .retrieve(
                &ctx,
                &RetrieveMemory {
                    memory: reference,
                    detail: DetailLevel::Full,
                    max_tokens: 1,
                }
            )
            .unwrap_err()
            .code(),
        "CONTEXT_BUDGET_EXCEEDED"
    );

    assert_eq!(
        service
            .retrieve(
                &ctx,
                &RetrieveMemory {
                    memory: Ref::new("mem:none", MemoryKind::Episodic, "\"x\""),
                    detail: DetailLevel::Full,
                    max_tokens: 100,
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
}

#[test]
fn mapi08_retrieve_refuses_another_tenant() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        service
            .retrieve(
                &other,
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

// -- MAPI-09 ----------------------------------------------------------------

#[test]
fn mapi09_update_revision_appends_and_checks_the_etag() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let receipt = service
        .update_revision(
            &ctx,
            &UpdateMemory {
                memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                patch: MemoryPatch {
                    summary: Some("alpha order delivered".into()),
                    importance_score: Some(0.9),
                    ..MemoryPatch::default()
                },
                expected_etag: etag.clone(),
            },
        )
        .unwrap();
    assert!(receipt.changed);
    assert_ne!(receipt.etag, etag);
    assert_eq!(receipt.events, vec!["memory.revision.committed"]);

    assert_eq!(
        service
            .update_revision(
                &ctx,
                &UpdateMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, etag.clone()),
                    patch: MemoryPatch {
                        summary: Some("stale write".into()),
                        ..MemoryPatch::default()
                    },
                    expected_etag: etag,
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );
}

// -- MAPI-13 ----------------------------------------------------------------

#[test]
fn mapi13_fold_produces_a_bounded_result_with_evidence() {
    let (service, _) = service();
    let ctx = full();
    let (first, _) = store(&service, &ctx, "alpha order shipped");
    let (second, _) = store(&service, &ctx, "beta order signed");
    let result = service
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
                model_score: None,
            },
        )
        .unwrap();
    assert_eq!(result.fold_level, 1);
    assert_eq!(result.source_refs.len(), 2);
    assert_eq!(result.protected_evidence_refs.len(), 1);
    assert!(result.trust_score < 0.5);
    assert!(!result.forced_preserve);
    assert!(result.token_count <= 100);
}

#[test]
fn mapi13_fold_never_drops_protected_evidence() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    service
        .set_episode_protection(&ctx, &id, true, false)
        .unwrap();
    let result = service
        .fold(
            &ctx,
            &FoldMemory {
                input: FoldInput {
                    episode_refs: vec![Ref::new(id.clone(), MemoryKind::Episodic, etag)],
                    policy_id: "policy:default".into(),
                    max_tokens: 100,
                    target_agent_id: "agent-main".into(),
                },
                model_score: None,
            },
        )
        .unwrap();
    assert!(result.forced_preserve);

    assert_eq!(
        service
            .fold(
                &ctx,
                &FoldMemory {
                    input: FoldInput {
                        episode_refs: vec![Ref::new(id, MemoryKind::Episodic, "\"x\"")],
                        policy_id: "policy:default".into(),
                        max_tokens: 1,
                        target_agent_id: "agent-main".into(),
                    },
                    model_score: None,
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );
}

#[test]
fn mapi13_fold_validates_its_inputs() {
    let (service, _) = service();
    let ctx = full();
    assert_eq!(
        service
            .fold(
                &ctx,
                &FoldMemory {
                    input: FoldInput {
                        episode_refs: Vec::new(),
                        policy_id: "policy:default".into(),
                        max_tokens: 10,
                        target_agent_id: "agent-main".into(),
                    },
                    model_score: None,
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );

    assert_eq!(
        service
            .fold(
                &ctx,
                &FoldMemory {
                    input: FoldInput {
                        episode_refs: vec![Ref::new("mem:none", MemoryKind::Episodic, "\"x\"")],
                        policy_id: "policy:default".into(),
                        max_tokens: 10,
                        target_agent_id: "agent-main".into(),
                    },
                    model_score: None,
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );

    let unauthorised = context("tenant-a", &[SCOPE_READ]);
    let (id, etag) = store(&service, &ctx, "alpha");
    assert_eq!(
        service
            .fold(
                &unauthorised,
                &FoldMemory {
                    input: FoldInput {
                        episode_refs: vec![Ref::new(id, MemoryKind::Episodic, etag)],
                        policy_id: "policy:default".into(),
                        max_tokens: 10,
                        target_agent_id: "agent-main".into(),
                    },
                    model_score: None,
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

// -- MAPI-14 / MAPI-15 ------------------------------------------------------

#[test]
fn mapi14_and_15_page_out_and_back_in_with_integrity() {
    let (service, clock) = service();
    let ctx = full();
    let reference = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("page me"),
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
    assert_eq!(receipt.integrity_hash.len(), 71);
    assert_eq!(receipt.status, PagingStatus::PagedOut);
    let page = receipt.page_ref.clone().expect("a page reference");
    assert!(service.page_generation(&page.id).is_some());
    assert!(service.page_expires_at(&page.id).is_some());

    let restored = service
        .page_in(
            &ctx,
            &PageIn {
                page_ref: page.clone(),
                expected_hash: receipt.integrity_hash.clone(),
            },
        )
        .unwrap();
    assert_eq!(restored.receipt.status, PagingStatus::PagedIn);
    assert_eq!(restored.block.content.as_deref(), Some("page me"));

    clock.advance(25 * 3_600);
    assert_eq!(
        service
            .page_in(
                &ctx,
                &PageIn {
                    page_ref: page.clone(),
                    expected_hash: receipt.integrity_hash.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "SOURCE_UNAVAILABLE"
    );
}

#[test]
fn mapi15_page_in_refuses_a_wrong_hash_and_a_foreign_scope() {
    let (service, _) = service();
    let ctx = full();
    let reference = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("page me"),
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
    let page = receipt.page_ref.clone().expect("a page reference");

    assert_eq!(
        service
            .page_in(
                &ctx,
                &PageIn {
                    page_ref: page.clone(),
                    expected_hash: "sha256:deadbeef".into(),
                }
            )
            .unwrap_err()
            .code(),
        "SOURCE_UNAVAILABLE"
    );

    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        service
            .page_in(
                &other,
                &PageIn {
                    page_ref: page,
                    expected_hash: receipt.integrity_hash,
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

#[test]
fn mapi14_page_out_refuses_protected_blocks_and_foreign_scopes() {
    let (service, _) = service();
    let ctx = full();
    let protected = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    protected: true,
                    ..block_input("pinned")
                },
                automatic: false,
            },
        )
        .unwrap();
    assert_eq!(
        service
            .page_out(
                &ctx,
                &PageOut {
                    block: protected,
                    policy_id: "page-policy".into(),
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );

    let ordinary = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("ordinary"),
                automatic: false,
            },
        )
        .unwrap();
    let other = context("tenant-z", &ALL_SCOPES);
    assert_eq!(
        service
            .page_out(
                &other,
                &PageOut {
                    block: ordinary,
                    policy_id: "page-policy".into(),
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

// -- MAPI-16 / MAPI-17 ------------------------------------------------------

#[test]
fn mapi16_and_17_commit_inside_the_window() {
    let (service, _) = service();
    let ctx = full();
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
    assert_eq!(window.state, WindowState::Open);

    assert_eq!(
        service
            .begin_reconsolidation(
                &ctx,
                &BeginReconsolidation {
                    fact: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                    expected_etag: etag.clone(),
                    window_hours: None,
                }
            )
            .unwrap_err()
            .code(),
        "LINK_STATE_CONFLICT"
    );

    assert_eq!(
        service
            .commit_reconsolidation(
                &ctx,
                &CommitReconsolidation {
                    window_id: window.window_id.clone(),
                    patch: MemoryPatch {
                        summary: Some("x".into()),
                        ..MemoryPatch::default()
                    },
                    evidence: Vec::new(),
                    expected_etag: "\"stale\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );
    assert_eq!(
        service
            .commit_reconsolidation(
                &ctx,
                &CommitReconsolidation {
                    window_id: window.window_id.clone(),
                    patch: MemoryPatch {
                        summary: Some("x".into()),
                        ..MemoryPatch::default()
                    },
                    evidence: Vec::new(),
                    expected_etag: etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "EVIDENCE_REQUIRED"
    );

    let committed = service
        .commit_reconsolidation(
            &ctx,
            &CommitReconsolidation {
                window_id: window.window_id.clone(),
                patch: MemoryPatch {
                    summary: Some("alpha order delivered".into()),
                    ..MemoryPatch::default()
                },
                evidence: vec![evidence("e2")],
                expected_etag: etag,
            },
        )
        .unwrap();
    assert!(committed.changed);
    assert_eq!(committed.events, vec!["memory.reconsolidation.committed"]);

    assert_eq!(
        service
            .commit_reconsolidation(
                &ctx,
                &CommitReconsolidation {
                    window_id: window.window_id,
                    patch: MemoryPatch {
                        summary: Some("again".into()),
                        ..MemoryPatch::default()
                    },
                    evidence: vec![evidence("e3")],
                    expected_etag: "\"whatever\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );
}

#[test]
fn mapi16_window_expires_with_the_clock() {
    let (service, clock) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let window = service
        .begin_reconsolidation(
            &ctx,
            &BeginReconsolidation {
                fact: Ref::new(id, MemoryKind::Episodic, etag.clone()),
                expected_etag: etag.clone(),
                window_hours: Some(1.0),
            },
        )
        .unwrap();
    clock.advance(2 * 3_600);
    assert_eq!(
        service
            .commit_reconsolidation(
                &ctx,
                &CommitReconsolidation {
                    window_id: window.window_id,
                    patch: MemoryPatch {
                        summary: Some("late".into()),
                        ..MemoryPatch::default()
                    },
                    evidence: vec![evidence("e2")],
                    expected_etag: etag,
                }
            )
            .unwrap_err()
            .code(),
        "POLICY_DENIED"
    );
    assert_eq!(
        service
            .commit_reconsolidation(
                &ctx,
                &CommitReconsolidation {
                    window_id: "rw:none".into(),
                    patch: MemoryPatch {
                        summary: Some("x".into()),
                        ..MemoryPatch::default()
                    },
                    evidence: Vec::new(),
                    expected_etag: "\"x\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
}

// -- MAPI-23 ----------------------------------------------------------------

#[test]
fn mapi23_forget_tombstones_and_keeps_authorities() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    service.link_trace(&ctx, &id, "trace:9").unwrap();

    let receipt = service
        .forget(
            &ctx,
            &ForgetMemory {
                memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                reason: "user request".into(),
                expected_etag: etag,
            },
        )
        .unwrap();
    assert!(receipt.tombstone_revision.starts_with("tombstone:"));
    assert!(!receipt.legal_hold);
    assert!(receipt
        .projections
        .iter()
        .all(|projection| projection.state == ProjectionState::Cleared));

    // The accepted trace link survives and is reported as stale.
    assert_eq!(service.trace_links(&id), vec!["trace:9".to_string()]);
    assert_eq!(service.stale_trace_links(), vec!["trace:9".to_string()]);

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
    let page = service
        .search(
            &ctx,
            &SearchMemory {
                query: search_query("alpha"),
            },
        )
        .unwrap();
    assert!(page.items.is_empty());
}

#[test]
fn mapi23_forget_is_blocked_by_a_legal_hold() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    service
        .set_episode_protection(&ctx, &id, false, true)
        .unwrap();
    assert_eq!(
        service
            .forget(
                &ctx,
                &ForgetMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, etag.clone()),
                    reason: "user request".into(),
                    expected_etag: etag,
                }
            )
            .unwrap_err()
            .code(),
        "DELETION_BLOCKED"
    );
}

#[test]
fn mapi23_forget_requires_the_scope_and_a_fresh_etag() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "alpha order shipped");
    let reader = context("tenant-a", &[SCOPE_READ]);
    assert_eq!(
        service
            .forget(
                &reader,
                &ForgetMemory {
                    memory: Ref::new(id.clone(), MemoryKind::Episodic, etag.clone()),
                    reason: "no scope".into(),
                    expected_etag: etag.clone(),
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
    assert_eq!(
        service
            .forget(
                &ctx,
                &ForgetMemory {
                    memory: Ref::new(id, MemoryKind::Episodic, etag),
                    reason: "stale".into(),
                    expected_etag: "\"stale\"".into(),
                }
            )
            .unwrap_err()
            .code(),
        "ETAG_MISMATCH"
    );
}

// -- MAPI-24 ----------------------------------------------------------------

#[test]
fn mapi24_evaluate_eviction_protects_task_state_and_honours_dry_run() {
    let (service, _) = service();
    let ctx = full();
    let removable = service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("scratch note"),
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
                    ..block_input("current task")
                },
                automatic: false,
            },
        )
        .unwrap();

    let request = EvaluateEviction {
        agent_id: "agent-main".into(),
        session_id: "s1".into(),
        policy_id: POLICY_DETERMINISTIC.into(),
        dry_run: true,
        required_tokens: 1,
    };
    let plan = service.evaluate_eviction(&ctx, &request).unwrap();
    assert_eq!(plan.candidates.len(), 2);
    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.selected[0].id, removable.id);
    assert_eq!(plan.protected.len(), 1);
    assert_eq!(plan.protected[0].id, task.id);
    assert!(plan.deterministic_fallback);
    assert!(!plan.overflow_safe_failure);

    let listed = service
        .list_working_blocks(
            &ctx,
            &ListWorkingBlocks {
                session_id: "s1".into(),
                agent_id: "agent-main".into(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert_eq!(listed.items.len(), 2);

    let applied = service
        .evaluate_eviction(
            &ctx,
            &EvaluateEviction {
                dry_run: false,
                ..request
            },
        )
        .unwrap();
    assert_eq!(applied.selected.len(), 1);
    let after = service
        .list_working_blocks(
            &ctx,
            &ListWorkingBlocks {
                session_id: "s1".into(),
                agent_id: "agent-main".into(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert_eq!(after.items.len(), 1);
    assert_eq!(after.items[0].reference.id, task.id);
}

#[test]
fn mapi24_evaluate_eviction_fails_safe_and_rejects_unknown_policies() {
    let (service, _) = service();
    let ctx = full();
    service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    block_type: BlockKind::TaskState,
                    ..block_input("current task")
                },
                automatic: false,
            },
        )
        .unwrap();
    let base = EvaluateEviction {
        agent_id: "agent-main".into(),
        session_id: "s1".into(),
        policy_id: POLICY_DETERMINISTIC.into(),
        dry_run: true,
        required_tokens: 1_000_000,
    };
    let plan = service.evaluate_eviction(&ctx, &base).unwrap();
    assert!(plan.overflow_safe_failure);
    assert!(plan.selected.is_empty());
    assert_eq!(
        service
            .evaluate_eviction(
                &ctx,
                &EvaluateEviction {
                    dry_run: false,
                    ..base.clone()
                }
            )
            .unwrap_err()
            .code(),
        "CONTEXT_OVERFLOW_SAFE_FAILURE"
    );
    assert_eq!(
        service
            .evaluate_eviction(
                &ctx,
                &EvaluateEviction {
                    policy_id: "mystery-v9".into(),
                    ..base
                }
            )
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

#[test]
fn mapi24_shadow_policy_keeps_the_deterministic_decision() {
    let (service, _) = service();
    let ctx = full();
    service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("scratch"),
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
                policy_id: POLICY_SHADOW_RL.into(),
                dry_run: true,
                required_tokens: 1,
            },
        )
        .unwrap();
    assert!(!plan.deterministic_fallback);
    assert_eq!(plan.model_profile.as_deref(), Some("shadow-no-model"));
    assert_eq!(plan.selected.len(), 1);
}

// -- MAPI-25 ----------------------------------------------------------------

#[test]
fn mapi25_get_budget_reports_the_registered_split() {
    let (service, _) = service();
    let ctx = full();
    let view = service
        .get_budget(
            &ctx,
            &GetMemoryBudget {
                agent_id: "agent-main".into(),
                session_id: "s1".into(),
            },
        )
        .unwrap();
    assert_eq!(view.model_context, 16_384);
    assert_eq!(view.available, 9_216);
    assert_eq!(view.agent_budget, 9_216);
    assert_eq!(view.wm, 5_529);
    assert_eq!(view.core, 2_304);
    assert_eq!(view.ring, 1_383);
    assert_eq!(view.wm + view.core + view.ring, view.agent_budget);
    assert_eq!(view.used, 0);

    service
        .put_working_block(
            &ctx,
            &PutWorkingBlock {
                input: block_input("two tokens"),
                automatic: false,
            },
        )
        .unwrap();
    let used = service
        .get_budget(
            &ctx,
            &GetMemoryBudget {
                agent_id: "agent-main".into(),
                session_id: "s1".into(),
            },
        )
        .unwrap();
    assert_eq!(used.used, 2);
}

#[test]
fn mapi25_get_budget_requires_read_and_the_same_session() {
    let (service, _) = service();
    let unauthorised = context("tenant-a", &[]);
    assert_eq!(
        service
            .get_budget(
                &unauthorised,
                &GetMemoryBudget {
                    agent_id: "agent-main".into(),
                    session_id: "s1".into(),
                }
            )
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
    assert_eq!(
        service
            .get_budget(
                &full(),
                &GetMemoryBudget {
                    agent_id: "agent-main".into(),
                    session_id: "s9".into(),
                }
            )
            .unwrap_err()
            .code(),
        "CROSS_SCOPE_DENIED"
    );
}

#[test]
fn mapi25_insufficient_context_budget_is_reported() {
    let service = MemoryService::new().with_reserves(Reserves::new(8_192, 4_096, 1_024, 1_024));
    assert_eq!(
        service
            .get_budget(
                &full(),
                &GetMemoryBudget {
                    agent_id: "agent-main".into(),
                    session_id: "s1".into(),
                }
            )
            .unwrap_err()
            .code(),
        "INSUFFICIENT_CONTEXT_BUDGET"
    );
}

// -- §37 port ---------------------------------------------------------------

#[test]
fn port_delegates_every_operation() {
    let (service, _) = service();
    let ctx = full();
    let receipt = block_on(FractalMemoryPort::store_episode(
        &service,
        &ctx,
        &StoreEpisode {
            input: episode_input("alpha port"),
        },
    ))
    .unwrap();
    let view = block_on(FractalMemoryPort::retrieve(
        &service,
        &ctx,
        &RetrieveMemory {
            memory: Ref::new(
                receipt.resource_id.clone(),
                MemoryKind::Episodic,
                receipt.etag.clone(),
            ),
            detail: DetailLevel::Summary,
            max_tokens: 100,
        },
    ))
    .unwrap();
    assert_eq!(view.kind, MemoryKind::Episodic);

    // The advanced operations delegate too: with the right scope the call no
    // longer reports a capability gap (ADR-052 supersedes ADR-051 D-09).
    let fact = block_on(FractalMemoryPort::propose_fact(
        &service,
        &ctx,
        &memory_domain::domain::ProposeFact {
            input: memory_domain::domain::SemanticFactInput {
                subject: Ref::new("concept:order", MemoryKind::Semantic, "\"1\""),
                predicate: "status".into(),
                object: "shipped".into(),
                language: "en".into(),
                evidence: vec![Evidence {
                    evidence_id: "e1".into(),
                    source_ref: "artifact:sha256:aa".into(),
                    confidence: 0.9,
                    source_group: "doc-a".into(),
                }],
                confidence: 0.8,
            },
        },
    ))
    .unwrap();
    assert_eq!(fact.kind, MemoryKind::Semantic);

    let denied = context("tenant-a", &[SCOPE_READ]);
    let gap = block_on(FractalMemoryPort::consolidate(
        &service,
        &denied,
        &Consolidate {
            episode_refs: vec![Ref::new(
                receipt.resource_id,
                MemoryKind::Episodic,
                receipt.etag,
            )],
            policy_id: "policy:default".into(),
            asynchronous: true,
        },
    ))
    .unwrap_err();
    assert_eq!(gap.code(), "FORBIDDEN", "authorization still applies");
}
