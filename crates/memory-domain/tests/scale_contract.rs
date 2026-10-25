//! Scale contract (P10 §2): a corpus larger than `MAX_CANDIDATES` stays
//! queryable, and a changed record is re-indexed rather than served stale.

use std::sync::Arc;

use memory_domain::domain::{
    DetailLevel, EpisodeInput, EpisodeOutcome, EpisodeType, MemoryKind, MemoryPatch, MemoryQuery,
    Ref, SearchMemory, StoreEpisode, UpdateMemory,
};
use memory_domain::search::MAX_CANDIDATES;
use memory_domain::service::{FixedClock, MemoryService, SCOPE_READ, SCOPE_WRITE};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const SESSION: &str = "session-scale";
const AGENT: &str = "agent-scale";

fn service() -> MemoryService {
    MemoryService::new().with_clock(Arc::new(FixedClock::new(1_700_000_000)))
}

fn context() -> RequestContext {
    RequestContext {
        request_id: "req-scale".into(),
        trace_id: "trace-scale".into(),
        scope: ScopeRef {
            tenant_id: "tenant-a".into(),
            project_id: "project-b".into(),
            repo_id: None,
            workspace_id: Some("task-scale".into()),
            agent_id: Some(AGENT.into()),
            session_id: Some(SESSION.into()),
        },
        principal_id: "principal".into(),
        scopes: vec![SCOPE_READ.to_string(), SCOPE_WRITE.to_string()],
        deadline_at: None,
        consistency: Consistency::ReadYourWrites,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

/// Query with the detail level a caller would use for a list.
fn query(text: &str, max_results: u32) -> MemoryQuery {
    MemoryQuery {
        query: text.to_string(),
        kinds: vec![MemoryKind::Episodic],
        language: None,
        after: None,
        before: None,
        min_trust: None,
        max_results,
        detail: DetailLevel::Summary,
        max_tokens: 4096,
    }
}

fn store(service: &MemoryService, index: usize, summary: String) -> String {
    let receipt = service
        .store_episode(
            &context(),
            &StoreEpisode {
                input: EpisodeInput {
                    session_id: SESSION.into(),
                    agent_id: AGENT.into(),
                    agent_depth: 0,
                    episode_type: EpisodeType::ConversationTurn,
                    summary,
                    full_text_ref: None,
                    key_facts: Vec::new(),
                    outcome: EpisodeOutcome::Success,
                    source_refs: vec!["artifact:sha256:scale".into()],
                    importance_score: 0.5,
                    language: "en".into(),
                    evidence: vec![memory_domain::domain::Evidence {
                        evidence_id: format!("ev-{index}"),
                        source_ref: "artifact:sha256:scale".into(),
                        confidence: 0.9,
                        source_group: "group-scale".into(),
                    }],
                },
            },
        )
        .expect("store");
    receipt.resource_id
}

#[test]
fn a_corpus_beyond_the_candidate_limit_is_still_searchable() {
    let service = service();
    // One record more than the limit, so the indexed path is taken.
    let total = MAX_CANDIDATES + 50;
    for index in 0..total {
        store(
            &service,
            index,
            format!("shared note about the parser number{index}"),
        );
    }

    // A token that only one record carries is found without RESOURCE_LIMIT.
    let found = service
        .search(
            &context(),
            &SearchMemory {
                query: query("number4242", 10),
            },
        )
        .expect("a large corpus must stay queryable");
    let hits = found.items;
    assert_eq!(hits.len(), 1, "expected exactly the matching record");
    assert!(
        hits[0]
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("number4242"),
        "wrong record: {:?}",
        hits[0].summary
    );

    // The shared token matches far more than the limit, and the cap still applies
    // to the *matches* — that is the bounded-work guarantee, reported as an error
    // rather than silently truncating.
    let broad = service.search(
        &context(),
        &SearchMemory {
            query: query("shared", 10),
        },
    );
    assert!(
        broad.is_err(),
        "more matches than the candidate limit must be refused, not truncated"
    );

    // A term nobody used returns nothing instead of scanning the corpus.
    let none = service
        .search(
            &context(),
            &SearchMemory {
                query: query("absenttoken", 10),
            },
        )
        .expect("an unmatched query is not an error");
    assert!(none.items.is_empty());
}

#[test]
fn a_changed_record_is_reindexed_rather_than_served_stale() {
    let service = service();
    let total = MAX_CANDIDATES + 5;
    let mut target = String::new();
    for index in 0..total {
        let id = store(&service, index, format!("filler record{index}"));
        if index == 7 {
            target = id;
        }
    }
    let original = service.search(
        &context(),
        &SearchMemory {
            query: query("filler", 5),
        },
    );
    assert!(original.is_err(), "the broad term exceeds the limit");

    let before = service
        .search(
            &context(),
            &SearchMemory {
                query: query("record7", 5),
            },
        )
        .expect("search");
    assert_eq!(before.items.len(), 1);

    // Rewrite the record so its only distinctive token changes.
    let etag = before.items[0].reference.revision.clone();
    service
        .update_revision(
            &context(),
            &UpdateMemory {
                memory: Ref::new(target.clone(), MemoryKind::Episodic, etag.clone()),
                patch: MemoryPatch {
                    summary: Some("rewritten as zeta-unique-marker".into()),
                    ..MemoryPatch::default()
                },
                expected_etag: etag,
            },
        )
        .expect("update");

    let stale = service
        .search(
            &context(),
            &SearchMemory {
                query: query("record7", 5),
            },
        )
        .expect("search");
    assert!(
        stale.items.is_empty(),
        "the index served the old text: {:?}",
        stale.items.len()
    );
    let fresh = service
        .search(
            &context(),
            &SearchMemory {
                query: query("zeta", 5),
            },
        )
        .expect("search");
    assert_eq!(fresh.items.len(), 1, "the new text must be indexed");
    assert_eq!(fresh.items[0].reference.id, target);
}
