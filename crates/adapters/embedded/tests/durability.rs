//! Durability + capability tests for the embedded adapter (profile A).

use std::collections::BTreeMap;
use std::path::PathBuf;

use storage_ports::capabilities::CapabilityProbe;
use storage_ports::{FullTextDoc, FullTextPort, MetadataPort, OutboxPort, StorageError};
use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};
use ukma_contracts::ScopeRef;

fn temp_path(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-embedded-{}-{}.json", std::process::id(), tag));
    let _ = std::fs::remove_file(&path);
    path
}

fn scope() -> ScopeRef {
    default_scope("tenant-a", "project-1")
}

#[test]
fn satisfies_the_minimum_production_profile() {
    let path = temp_path("caps");
    let store = EmbeddedStore::open(&path).unwrap();
    let capabilities = store.capabilities();
    assert!(capabilities.available);
    assert_eq!(
        capabilities.missing_required(),
        Vec::<&str>::new(),
        "embedded adapter must provide every minimum-profile capability (§40.5)"
    );
    capabilities.assert_minimum_profile().unwrap();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn metadata_survives_flush_and_reload() {
    let path = temp_path("metadata");
    let s = scope();
    {
        let store = EmbeddedStore::open(&path).unwrap();
        store
            .put_cas(&s, "graph/manifest", serde_json::json!({"gen": 7}), None)
            .unwrap();
        store.put_manifest(&s, "graph", "gen-7").unwrap();
        store.acquire_writer(&s, "agg:1", "writer-a").unwrap();
        store.flush().unwrap();
    }
    let store = EmbeddedStore::open(&path).unwrap();
    let entry = store
        .get(&s, "graph/manifest")
        .unwrap()
        .expect("entry survives");
    assert_eq!(entry.value, serde_json::json!({"gen": 7}));
    assert_eq!(entry.revision, 1);
    assert_eq!(
        store.get_manifest(&s, "graph").unwrap().as_deref(),
        Some("gen-7")
    );
    // the fence survived too, so a second writer is still rejected
    match store.acquire_writer(&s, "agg:1", "writer-b") {
        Err(StorageError::Conflict { .. }) => {}
        other => panic!("expected fencing conflict after reload, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn unflushed_state_does_not_persist() {
    let path = temp_path("noflush");
    let s = scope();
    {
        let store = EmbeddedStore::open(&path).unwrap();
        store.put_cas(&s, "k", serde_json::json!(1), None).unwrap();
        // deliberately no flush
    }
    let store = EmbeddedStore::open(&path).unwrap();
    assert!(
        store.get(&s, "k").unwrap().is_none(),
        "durability is obtained at flush(), not per mutation"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn outbox_replays_after_reload_and_keeps_ack_state() {
    let path = temp_path("outbox");
    let s = scope();
    {
        let store = EmbeddedStore::open(&path).unwrap();
        store
            .enqueue(&s, event("e1", "memory.episode.committed", 1))
            .unwrap();
        store.enqueue(&s, event("e2", "memory.folded", 1)).unwrap();
        store.ack(&s, "projection", "e1").unwrap();
        store.flush().unwrap();
    }
    let store = EmbeddedStore::open(&path).unwrap();
    let pending = store.pending(&s, "projection", 10).unwrap();
    assert_eq!(pending.len(), 1, "acked event stays acked; e2 is replayed");
    assert_eq!(pending[0].event_id, "e2");
    // a consumer that never acked sees both
    assert_eq!(store.pending(&s, "audit", 10).unwrap().len(), 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn derived_projections_do_not_survive_without_outbox_replay() {
    // Honest assertion: full-text/vector are derived (in memory) in profile A.
    let path = temp_path("derived");
    let s = scope();
    {
        let store = EmbeddedStore::open(&path).unwrap();
        store
            .index(
                &s,
                "icu-v1",
                &FullTextDoc {
                    id: "d1".into(),
                    revision: "r1".into(),
                    fields: BTreeMap::from([("body".to_string(), "graph".to_string())]),
                },
            )
            .unwrap();
        assert_eq!(
            store.search(&s, "icu-v1", None, "graph", 10).unwrap().len(),
            1
        );
        store.flush().unwrap();
    }
    let store = EmbeddedStore::open(&path).unwrap();
    assert!(
        store
            .search(&s, "icu-v1", None, "graph", 10)
            .unwrap()
            .is_empty(),
        "derived index must be rebuilt from canonical data + outbox replay (§6.4)"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn corrupt_file_is_a_typed_error() {
    let path = temp_path("corrupt");
    std::fs::write(&path, b"not a snapshot {").unwrap();
    match EmbeddedStore::open(&path) {
        Err(StorageError::Internal(message)) => assert!(message.contains("corrupt"), "{message}"),
        Ok(_) => panic!("expected a typed error for a corrupt store file"),
        Err(other) => panic!("unexpected error: {other}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn scope_isolation_holds_across_reload() {
    let path = temp_path("isolation");
    let a = default_scope("tenant-a", "project-1");
    let b = default_scope("tenant-b", "project-1");
    {
        let store = EmbeddedStore::open(&path).unwrap();
        store
            .put_cas(&a, "k", serde_json::json!("a"), None)
            .unwrap();
        store
            .put_cas(&b, "k", serde_json::json!("b"), None)
            .unwrap();
        store.flush().unwrap();
    }
    let store = EmbeddedStore::open(&path).unwrap();
    assert_eq!(
        store.get(&a, "k").unwrap().unwrap().value,
        serde_json::json!("a")
    );
    assert_eq!(
        store.get(&b, "k").unwrap().unwrap().value,
        serde_json::json!("b")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn unavailable_profile_helper_reports_honestly() {
    let capabilities =
        ukma_adapter_embedded::unavailable_adapter_capabilities("postgres", "no server configured");
    assert!(!capabilities.available);
    assert_eq!(capabilities.unavailable_reason, "no server configured");
    assert!(capabilities.assert_minimum_profile().is_err());
}
