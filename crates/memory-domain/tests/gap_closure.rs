//! MAPI-26..28 (memory.context / memory.list / memory.stats) contract tests —
//! Phase 8 gap closure.
//!
//! Each operation is exercised with a positive path plus the failure paths the
//! registry declares: authorization, cross-scope, revision/consistency and
//! input-validation failures.

use std::sync::Arc;

use context_engine::Reserves;
use memory_domain::domain::{
    BlockKind, EpisodeInput, EpisodeOutcome, EpisodeType, Evidence, GetMemoryStats,
    ListMemoryQuery, MemoryContextQuery, MemoryKind, MemoryPatch, PageRequest, PutWorkingBlock,
    StoreEpisode, WorkingBlockInput,
};
use memory_domain::service::{FixedClock, MemoryService, SCOPE_WRITE};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const ALL_SCOPES: [&str; 10] = [
    "memory:read",
    "memory:write",
    "memory:delegate",
    "memory:fold",
    "memory:manage",
    "memory:forget",
    "memory:review",
    "memory:consolidate",
    "memory:procedure_write",
    "memory:procedure_review",
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

fn service() -> (MemoryService, Arc<FixedClock>) {
    let clock = Arc::new(FixedClock::new(1_000_000_000));
    let service = MemoryService::new()
        .with_reserves(Reserves::new(16_384, 4_096, 2_048, 1_024))
        .with_clock(clock.clone());
    (service, clock)
}

fn episode_input(summary: &str) -> EpisodeInput {
    EpisodeInput {
        session_id: "s1".into(),
        agent_id: "agent-main".into(),
        agent_depth: 0,
        episode_type: EpisodeType::ConversationTurn,
        summary: summary.to_string(),
        full_text_ref: None,
        key_facts: vec!["fact".to_string()],
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

fn put_block(service: &MemoryService, ctx: &RequestContext, content: &str) {
    service
        .put_working_block(
            ctx,
            &PutWorkingBlock {
                input: WorkingBlockInput {
                    session_id: "s1".into(),
                    agent_id: "agent-main".into(),
                    block_type: BlockKind::ActiveContext,
                    content: content.to_string(),
                    language: "en".into(),
                    priority: 0.5,
                    protected: false,
                    expires_after_turns: None,
                },
                automatic: false,
            },
        )
        .expect("working block");
}

fn forget(service: &MemoryService, ctx: &RequestContext, id: &str, etag: &str) {
    service
        .forget(
            ctx,
            &memory_domain::domain::ForgetMemory {
                memory: memory_domain::domain::Ref::new(id, MemoryKind::Episodic, etag),
                reason: "cleanup".into(),
                expected_etag: etag.to_string(),
            },
        )
        .expect("forget");
}

fn context_query(path: Option<&str>, symbol: Option<&str>) -> MemoryContextQuery {
    MemoryContextQuery {
        path: path.map(str::to_string),
        symbol_id: symbol.map(str::to_string),
        min_trust: None,
        limit: 0,
    }
}

// -- MAPI-26 memory.context --------------------------------------------------

#[test]
fn mapi26_context_positive_matches_path_and_symbol() {
    let (service, _) = service();
    let ctx = full();
    store(
        &service,
        &ctx,
        "worked on crates/memory-domain/src/service.rs for the search path",
    );
    store(&service, &ctx, "unrelated memory about the weather");

    let by_path = service
        .memory_context(
            &ctx,
            &context_query(Some("crates/memory-domain/src/service.rs"), None),
        )
        .expect("positive path");
    assert_eq!(by_path.total, 1);
    assert_eq!(by_path.matches.len(), 1);
    assert_eq!(by_path.matches[0].matched_on, "path");

    let by_symbol = service
        .memory_context(&ctx, &context_query(None, Some("memory-domain")))
        .expect("positive symbol");
    assert_eq!(by_symbol.total, 1);
    assert_eq!(by_symbol.matches[0].matched_on, "symbol");

    // No match: empty result, zero total.
    let none = service
        .memory_context(&ctx, &context_query(None, Some("does-not-exist")))
        .expect("no match");
    assert_eq!(none.total, 0);
    assert!(none.matches.is_empty());
}

#[test]
fn mapi26_context_validates_input() {
    let (service, _) = service();
    let ctx = full();

    // Neither path nor symbol.
    let err = service
        .memory_context(&ctx, &context_query(None, None))
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");

    // Blank values count as missing.
    let err = service
        .memory_context(
            &ctx,
            &MemoryContextQuery {
                path: Some("   ".into()),
                symbol_id: Some("  ".into()),
                min_trust: None,
                limit: 0,
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");

    // Out-of-range trust.
    for bad in [Some(-0.1), Some(1.5), Some(f64::NAN)] {
        let err = service
            .memory_context(
                &ctx,
                &MemoryContextQuery {
                    path: Some("x".into()),
                    symbol_id: None,
                    min_trust: bad,
                    limit: 0,
                },
            )
            .unwrap_err();
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    // Limit above the cap.
    let err = service
        .memory_context(
            &ctx,
            &MemoryContextQuery {
                path: Some("x".into()),
                symbol_id: None,
                min_trust: None,
                limit: 101,
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
}

#[test]
fn mapi26_context_requires_the_read_scope() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "something about src/main.rs");
    let write_only = context("tenant-a", &[SCOPE_WRITE]);
    let err = service
        .memory_context(&write_only, &context_query(Some("src/main.rs"), None))
        .unwrap_err();
    assert_eq!(err.code(), "FORBIDDEN");
}

#[test]
fn mapi26_context_hides_other_tenants_and_tombstones() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "tenant-a touched src/other.rs");

    // Another tenant never sees the record.
    let other = context("tenant-z", &ALL_SCOPES);
    let page = service
        .memory_context(&other, &context_query(Some("src/other.rs"), None))
        .expect("cross-scope read");
    assert_eq!(page.total, 0);

    // A tombstoned record drops out of context results.
    forget(&service, &ctx, &id, &etag);
    let page = service
        .memory_context(&ctx, &context_query(Some("src/other.rs"), None))
        .expect("post-forget read");
    assert_eq!(page.total, 0);
}

#[test]
fn mapi26_context_min_trust_filter_and_deterministic_replay() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "worked on src/alpha.rs");
    store(&service, &ctx, "also touched src/alpha.rs");

    let base = service
        .memory_context(&ctx, &context_query(Some("src/alpha.rs"), None))
        .expect("base");
    assert_eq!(base.total, 2);

    // min_trust at the record trust (0.5) keeps them; above it drops them.
    let at = service
        .memory_context(
            &ctx,
            &MemoryContextQuery {
                path: Some("src/alpha.rs".into()),
                symbol_id: None,
                min_trust: Some(0.5),
                limit: 0,
            },
        )
        .expect("min trust 0.5");
    assert_eq!(at.total, 2);
    let above = service
        .memory_context(
            &ctx,
            &MemoryContextQuery {
                path: Some("src/alpha.rs".into()),
                symbol_id: None,
                min_trust: Some(0.51),
                limit: 0,
            },
        )
        .expect("min trust 0.51");
    assert_eq!(above.total, 0);

    // Deterministic replay: two reads return byte-identical results.
    let again = service
        .memory_context(&ctx, &context_query(Some("src/alpha.rs"), None))
        .expect("replay");
    assert_eq!(base.matches, again.matches);
    assert_eq!(base.total, again.total);

    // limit truncates, total reports the full match count.
    let limited = service
        .memory_context(
            &ctx,
            &MemoryContextQuery {
                path: Some("src/alpha.rs".into()),
                symbol_id: None,
                min_trust: None,
                limit: 1,
            },
        )
        .expect("limited");
    assert_eq!(limited.matches.len(), 1);
    assert_eq!(limited.total, 2);
}

// -- MAPI-27 memory.list -----------------------------------------------------

#[test]
fn mapi27_list_positive_pages_and_filters_kinds() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "first");
    store(&service, &ctx, "second");
    store(&service, &ctx, "third");

    let all = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(2),
            },
        )
        .expect("positive");
    assert_eq!(all.total, 3);
    assert_eq!(all.items.len(), 2);
    assert_eq!(all.next_cursor.as_deref(), Some("2"));

    let next = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest {
                    cursor: Some("2".into()),
                    limit: 2,
                },
            },
        )
        .expect("second page");
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.next_cursor, None);

    // Deterministic: ordered by id.
    let ids: Vec<&str> = all
        .items
        .iter()
        .chain(next.items.iter())
        .map(|item| item.reference.id.as_str())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);

    // Kind filter: episodes only.
    let episodes = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: vec![MemoryKind::Episodic],
                page: PageRequest::new(10),
            },
        )
        .expect("kind filter");
    assert_eq!(episodes.total, 3);
    assert!(episodes
        .items
        .iter()
        .all(|item| item.kind == MemoryKind::Episodic));
}

#[test]
fn mapi27_list_validates_page_and_cursor() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "first");

    // Zero limit.
    let err = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(0),
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");

    // Limit above the cap.
    let err = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(201),
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");

    // Cursor beyond the end.
    let err = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest {
                    cursor: Some("99".into()),
                    limit: 2,
                },
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "CURSOR_EXPIRED");
}

#[test]
fn mapi27_list_requires_the_read_scope() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "first");
    let write_only = context("tenant-a", &[SCOPE_WRITE]);
    let err = service
        .list_memory(
            &write_only,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "FORBIDDEN");
}

#[test]
fn mapi27_list_hides_other_tenants_and_tombstones() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "tenant-a record");
    store(&service, &ctx, "tenant-a second");

    // Another tenant sees nothing.
    let other = context("tenant-z", &ALL_SCOPES);
    let page = service
        .list_memory(
            &other,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .expect("cross-scope read");
    assert_eq!(page.total, 0);

    // A tombstoned record drops out of the listing.
    forget(&service, &ctx, &id, &etag);
    let page = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .expect("post-forget read");
    assert_eq!(page.total, 1);
}

// -- MAPI-28 memory.stats ----------------------------------------------------

#[test]
fn mapi28_stats_positive_counts_kinds_and_blocks() {
    let (service, _) = service();
    let ctx = full();
    put_block(&service, &ctx, "working context");
    store(&service, &ctx, "one");
    store(&service, &ctx, "two");

    let stats = service
        .memory_stats(&ctx, &GetMemoryStats)
        .expect("positive");
    assert_eq!(stats.total, 2);
    assert_eq!(stats.by_kind.len(), 4);
    assert_eq!(stats.by_verification.len(), 4);
    let episodes = stats
        .by_kind
        .iter()
        .find(|entry| entry.kind == MemoryKind::Episodic)
        .expect("episodes entry");
    assert_eq!(episodes.count, 2);
    let working = stats
        .by_kind
        .iter()
        .find(|entry| entry.kind == MemoryKind::Working)
        .expect("working entry");
    assert_eq!(working.count, 0, "blocks are not memory items");
    assert_eq!(stats.working_blocks, 1);
    // All harness episodes are unverified.
    let unverified = stats
        .by_verification
        .iter()
        .find(|entry| entry.state == memory_domain::domain::VerificationState::Unverified)
        .expect("unverified entry");
    assert_eq!(unverified.count, 2);
}

#[test]
fn mapi28_stats_excludes_tombstones() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "one");
    store(&service, &ctx, "two");
    forget(&service, &ctx, &id, &etag);
    let stats = service.memory_stats(&ctx, &GetMemoryStats).expect("stats");
    assert_eq!(stats.total, 1);
}

#[test]
fn mapi28_stats_requires_the_read_scope() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "one");
    let write_only = context("tenant-a", &[SCOPE_WRITE]);
    let err = service
        .memory_stats(&write_only, &GetMemoryStats)
        .unwrap_err();
    assert_eq!(err.code(), "FORBIDDEN");
}

#[test]
fn mapi28_stats_hides_other_tenants() {
    let (service, _) = service();
    let ctx = full();
    store(&service, &ctx, "one");
    let other = context("tenant-z", &ALL_SCOPES);
    let stats = service
        .memory_stats(&other, &GetMemoryStats)
        .expect("cross-scope read");
    assert_eq!(stats.total, 0);
}

// -- shared failure path: an update after a read (revision consistency) -------

#[test]
fn mapi26_27_28_reflect_a_revised_record() {
    let (service, _) = service();
    let ctx = full();
    let (id, etag) = store(&service, &ctx, "initial summary about src/alpha.rs");

    service
        .update_revision(
            &ctx,
            &memory_domain::domain::UpdateMemory {
                memory: memory_domain::domain::Ref::new(
                    id.clone(),
                    MemoryKind::Episodic,
                    etag.clone(),
                ),
                expected_etag: etag,
                patch: MemoryPatch {
                    summary: Some("revised summary about src/alpha.rs".into()),
                    ..MemoryPatch::default()
                },
            },
        )
        .expect("update");

    let context = service
        .memory_context(&ctx, &context_query(Some("src/alpha.rs"), None))
        .expect("context after update");
    assert_eq!(context.matches.len(), 1);
    assert_eq!(
        context.matches[0].summary,
        "revised summary about src/alpha.rs"
    );

    let page = service
        .list_memory(
            &ctx,
            &ListMemoryQuery {
                kinds: Vec::new(),
                page: PageRequest::new(10),
            },
        )
        .expect("list after update");
    assert_eq!(page.items[0].summary, "revised summary about src/alpha.rs");

    let stats = service.memory_stats(&ctx, &GetMemoryStats).expect("stats");
    assert_eq!(stats.total, 1);
}
