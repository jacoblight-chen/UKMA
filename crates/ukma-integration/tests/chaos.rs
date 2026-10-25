//! P9-WP5: chaos and crash replay (§38.1 E2E-API-06/09/12).
//!
//! The crash points are stated explicitly: the canonical commit landed, the
//! projection did not. The recovery path is the outbox, and the assertions are
//! about what a reader may observe afterwards — one READY generation and no
//! ghost records.

use std::collections::BTreeMap;
use std::path::PathBuf;

use indexing_engine::{
    CommitPhase, GenerationPointer, JobError, JobManager, ProjectionPhase, Stage,
};
use storage_ports::{FullTextDoc, FullTextPort, MetadataPort, OutboxPort};
use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};
use ukma_contracts::ScopeRef;

const TENANT: &str = "tenant-a";
const PROJECT: &str = "project-b";
const GENERATION: &str = "gen-2";

fn temp_path(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-chaos-{}-{tag}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn scope() -> ScopeRef {
    default_scope(TENANT, PROJECT)
}

/// Advance a job to the canonical commit, as a crashed worker would leave it.
fn committed_job() -> ukma_contracts::generated::OperationSpec {
    // The registry entry is used only to name the job kind.
    ukma_contracts::generated::OPERATIONS
        .iter()
        .find(|spec| spec.operation_id == "code.analyze_repository")
        .copied()
        .expect("registered operation")
}

#[test]
fn chaos_after_commit_before_publish_replays_to_exactly_one_ready_generation() {
    let path = temp_path("commit");
    let s = scope();

    // --- worker: commit the canonical data, queue the projection, then crash.
    {
        let store = EmbeddedStore::open(&path).unwrap();
        store
            .put_cas(&s, "graph/manifest", serde_json::json!({"gen": 2}), None)
            .unwrap();
        store.put_manifest(&s, "graph", GENERATION).unwrap();
        store
            .enqueue(&s, event("e1", "code.index.publish", 2))
            .unwrap();
        store.flush().unwrap();
        // crash: nothing is published, the process disappears here.
    }

    // --- restart: the canonical commit is durable and the projection is pending.
    let store = EmbeddedStore::open(&path).unwrap();
    assert_eq!(
        store.get_manifest(&s, "graph").unwrap().as_deref(),
        Some(GENERATION),
        "the canonical commit must survive the crash"
    );
    let pending = store.pending(&s, "projection", 10).unwrap();
    assert_eq!(pending.len(), 1, "the projection event must be replayed");

    let mut pointer = GenerationPointer::new();
    assert!(matches!(
        pointer.require(GENERATION),
        Err(JobError::StaleGeneration { .. })
    ));

    // --- replay: publish once, then ack.
    fn replay(store: &EmbeddedStore, scope: &ScopeRef, pointer: &mut GenerationPointer) -> usize {
        let mut published = 0;
        for pending in store.pending(scope, "projection", 10).unwrap() {
            if pointer.current() != Some(GENERATION) {
                pointer.publish(GENERATION);
                published += 1;
            }
            store.ack(scope, "projection", &pending.event_id).unwrap();
        }
        published
    }
    assert_eq!(replay(&store, &s, &mut pointer), 1);
    pointer
        .require(GENERATION)
        .expect("the generation is READY now");
    assert_eq!(pointer.current(), Some(GENERATION));
    assert!(
        pointer.retained().is_empty(),
        "a single publish retains nothing: {:?}",
        pointer.retained()
    );
    store.flush().unwrap();

    // --- a second crash right after publish, before the ack, must not publish twice.
    let store = EmbeddedStore::open(&path).unwrap();
    let replayed_again = replay(&store, &s, &mut pointer);
    assert_eq!(
        replayed_again, 0,
        "replaying a published generation is a no-op"
    );
    assert_eq!(pointer.current(), Some(GENERATION));
    assert!(pointer.retained().is_empty(), "{:?}", pointer.retained());
    assert!(
        store.pending(&s, "projection", 10).unwrap().is_empty(),
        "the ack must be durable"
    );

    // No ghost records: exactly the one canonical manifest entry exists.
    let entries = store
        .list(&s, "graph/", 100, None)
        .map(|(items, _cursor)| items.len())
        .unwrap_or(0);
    assert_eq!(entries, 1, "the crash left {entries} records behind");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn chaos_a_crashed_projection_never_publishes_a_partial_index() {
    let path = temp_path("partial");
    let s = scope();

    {
        let store = EmbeddedStore::open(&path).unwrap();
        store
            .put_cas(&s, "doc/1", serde_json::json!({"text": "alpha beta"}), None)
            .unwrap();
        store
            .enqueue(&s, event("e1", "document.ingest.projected", 1))
            .unwrap();
        // Half of the projection lands, then the process dies before the ack.
        store
            .index(
                &s,
                "icu-v1",
                &FullTextDoc {
                    id: "doc/1".into(),
                    revision: "rev-1".into(),
                    fields: BTreeMap::from([("body".to_string(), "alpha beta".to_string())]),
                },
            )
            .unwrap();
        store.flush().unwrap();
    }

    // Restart: the derived index is gone (profile A keeps it in memory) and the
    // event is still pending, so a reader must not be told the index is current.
    let store = EmbeddedStore::open(&path).unwrap();
    let mut pointer = GenerationPointer::new();
    assert!(pointer.require("gen-1").is_err());
    assert!(
        store
            .search(&s, "icu-v1", None, "alpha", 10)
            .unwrap()
            .is_empty(),
        "a derived index must not claim to be current after a crash"
    );

    // Full replay rebuilds the index and publishes the generation exactly once.
    for pending in store.pending(&s, "projection", 10).unwrap() {
        let entry = store.get(&s, "doc/1").unwrap().expect("canonical document");
        store
            .index(
                &s,
                "icu-v1",
                &FullTextDoc {
                    id: "doc/1".into(),
                    revision: "rev-1".into(),
                    fields: BTreeMap::from([(
                        "body".to_string(),
                        entry.value["text"].as_str().unwrap_or_default().to_string(),
                    )]),
                },
            )
            .unwrap();
        pointer.publish("gen-1");
        store.ack(&s, "projection", &pending.event_id).unwrap();
    }
    assert_eq!(
        store.search(&s, "icu-v1", None, "alpha", 10).unwrap().len(),
        1,
        "replay must produce exactly one searchable document"
    );
    pointer.require("gen-1").expect("READY after replay");
    assert_eq!(pointer.retained().len(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn chaos_a_job_cancelled_before_the_commit_leaves_no_canonical_data() {
    let spec = committed_job();
    let mut jobs = JobManager::new();
    jobs.submit("job-1", spec.operation_id);
    for _ in 0..2 {
        jobs.advance("job-1").unwrap();
    }
    let cancelled = jobs.cancel("job-1", "operator stopped the run").unwrap();
    assert_eq!(cancelled.stage, Stage::Cancelled);
    assert_eq!(
        cancelled.outcome.commit,
        CommitPhase::NotCommitted,
        "a pre-commit cancel must not leave a commit"
    );

    // The same job after the commit point cannot be cancelled, and the commit and
    // its (retryable) projection state stay observable.
    jobs.submit("job-2", spec.operation_id);
    while jobs.get("job-2").unwrap().stage != Stage::Indexing {
        jobs.advance("job-2").unwrap();
    }
    let running = jobs
        .projection_failed("job-2", "index node unavailable")
        .unwrap();
    assert_eq!(running.outcome.commit, CommitPhase::Committed);
    assert_eq!(running.outcome.projection, ProjectionPhase::Failed);
    assert!(running.projection_retryable());
    assert!(matches!(
        jobs.cancel("job-2", "too late"),
        Err(JobError::JobNotCancellable { .. })
    ));
    // Retrying the projection keeps the commit and republishes.
    let retried = jobs.advance("job-2").unwrap();
    assert_eq!(retried.outcome.commit, CommitPhase::Committed);
    assert_eq!(jobs.get("job-2").unwrap().stage, Stage::Ready);
}
