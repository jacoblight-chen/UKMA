//! Contract tests for every storage port (§6.2), run against the reference
//! in-memory implementations. Adapters MUST reproduce these semantics.

use std::collections::BTreeMap;

use storage_ports::capabilities::CapabilityProbe;
use storage_ports::testing::{
    InMemoryArtifact, InMemoryFullText, InMemoryMetadata, InMemoryOutbox, InMemoryVector,
};
use storage_ports::{
    ArtifactPort, EmbeddingProfile, FullTextDoc, FullTextPort, MetadataPort, Metric, OutboxEvent,
    OutboxPort, Retention, StorageError, VectorItem, VectorPort,
};
use ukma_contracts::ScopeRef;

fn scope(tenant: &str, project: &str) -> ScopeRef {
    ScopeRef {
        tenant_id: tenant.into(),
        project_id: project.into(),
        repo_id: None,
        workspace_id: None,
        agent_id: None,
        session_id: None,
    }
}

fn event(id: &str) -> OutboxEvent {
    OutboxEvent {
        event_id: id.into(),
        event_type: "test.event".into(),
        event_version: 1,
        aggregate_ref: "agg:1".into(),
        aggregate_revision: 1,
        payload: serde_json::json!({"operation_id": "test"}),
        idempotency_key: None,
    }
}

fn profile(profile_id: &str, dimension: usize, metric: Metric) -> EmbeddingProfile {
    EmbeddingProfile {
        profile_id: profile_id.into(),
        model_id: "bge-m3".into(),
        model_revision: "1".into(),
        dimension,
        metric,
        normalization: "l2".into(),
    }
}

// ---------- Artifact ----------

#[test]
fn artifact_is_content_addressed_and_idempotent() {
    let store = InMemoryArtifact::new();
    let s = scope("t", "p");
    let first = store
        .put_immutable(&s, b"hello", "text/plain", Retention::KeepForever)
        .unwrap();
    let second = store
        .put_immutable(&s, b"hello", "text/plain", Retention::KeepForever)
        .unwrap();
    assert_eq!(
        first.hash, second.hash,
        "same bytes => same content address"
    );
    assert!(first.hash.starts_with("sha256:"));
    assert_eq!(store.get(&s, &first.hash).unwrap(), b"hello");
    assert!(store.exists(&s, &first.hash).unwrap());
    assert!(store.verify(&s, &first.hash).unwrap());
}

#[test]
fn artifact_isolated_per_scope() {
    let store = InMemoryArtifact::new();
    let a = scope("tenant-a", "p");
    let b = scope("tenant-b", "p");
    let record = store
        .put_immutable(&a, b"secret", "text/plain", Retention::KeepForever)
        .unwrap();
    assert!(!store.exists(&b, &record.hash).unwrap());
    match store.get(&b, &record.hash) {
        Err(StorageError::NotFound { .. }) => {}
        other => panic!("expected NotFound cross-scope, got {other:?}"),
    }
}

// ---------- Metadata ----------

#[test]
fn metadata_cas_rejects_stale_revision() {
    let store = InMemoryMetadata::new();
    let s = scope("t", "p");
    let created = store
        .put_cas(&s, "k", serde_json::json!({"v": 1}), None)
        .unwrap();
    assert_eq!(created.revision, 1);

    let updated = store
        .put_cas(&s, "k", serde_json::json!({"v": 2}), Some(1))
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_ne!(created.etag, updated.etag);

    match store.put_cas(&s, "k", serde_json::json!({"v": 3}), Some(1)) {
        Err(StorageError::Conflict {
            expected_revision,
            actual_revision,
            ..
        }) => {
            assert_eq!(expected_revision, 1);
            assert_eq!(actual_revision, 2);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(
        store.get(&s, "k").unwrap().unwrap().value,
        serde_json::json!({"v": 2}),
        "failed CAS must not mutate"
    );
}

#[test]
fn metadata_create_only_semantics() {
    let store = InMemoryMetadata::new();
    let s = scope("t", "p");
    match store.put_cas(&s, "missing", serde_json::json!(1), Some(1)) {
        Err(StorageError::Conflict {
            actual_revision, ..
        }) => assert_eq!(actual_revision, 0),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn metadata_list_pages_with_stable_order() {
    let store = InMemoryMetadata::new();
    let s = scope("t", "p");
    for key in ["a2", "a1", "a3", "b1"] {
        store
            .put_cas(&s, key, serde_json::json!(key), None)
            .unwrap();
    }
    let (first, cursor) = store.list(&s, "a", 2, None).unwrap();
    assert_eq!(
        first.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        vec!["a1", "a2"]
    );
    let cursor = cursor.expect("cursor for a full page");
    let (second, cursor) = store.list(&s, "a", 2, Some(&cursor)).unwrap();
    assert_eq!(
        second.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        vec!["a3"]
    );
    assert!(cursor.is_none(), "last page has no cursor");

    let (all, _) = store.list(&s, "", 10, None).unwrap();
    assert_eq!(all.len(), 4, "empty prefix lists everything in scope");
}

#[test]
fn metadata_manifest_pointer_is_atomic_replacement() {
    let store = InMemoryMetadata::new();
    let s = scope("t", "p");
    assert!(store.get_manifest(&s, "graph").unwrap().is_none());
    store.put_manifest(&s, "graph", "gen-1").unwrap();
    assert_eq!(
        store.get_manifest(&s, "graph").unwrap().as_deref(),
        Some("gen-1")
    );
    store.put_manifest(&s, "graph", "gen-2").unwrap();
    assert_eq!(
        store.get_manifest(&s, "graph").unwrap().as_deref(),
        Some("gen-2")
    );
}

#[test]
fn metadata_single_writer_fencing() {
    let store = InMemoryMetadata::new();
    let s = scope("t", "p");
    store.acquire_writer(&s, "agg:1", "token-a").unwrap();
    store.acquire_writer(&s, "agg:1", "token-a").unwrap();
    match store.acquire_writer(&s, "agg:1", "token-b") {
        Err(StorageError::Conflict { collection, .. }) => assert_eq!(collection, "writer_fence"),
        other => panic!("expected fencing conflict, got {other:?}"),
    }
    assert!(store.acquire_writer(&s, "agg:2", "token-b").is_ok());
}

// ---------- Full text ----------

#[test]
fn fulltext_ranks_by_bm25() {
    let store = InMemoryFullText::new();
    let s = scope("t", "p");
    for (id, text) in [
        ("d1", "graph traversal algorithm"),
        ("d2", "graph"),
        ("d3", "unrelated content"),
    ] {
        store
            .index(
                &s,
                "icu-v1",
                &FullTextDoc {
                    id: id.into(),
                    revision: "r1".into(),
                    fields: BTreeMap::from([("body".to_string(), text.to_string())]),
                },
            )
            .unwrap();
    }
    let hits = store
        .search(&s, "icu-v1", Some("body"), "graph", 10)
        .unwrap();
    assert_eq!(hits.len(), 2, "only matching documents");
    assert_eq!(
        hits[0].id, "d2",
        "shorter document scores higher (BM25 length normalisation)"
    );
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn fulltext_analyzers_and_scopes_are_isolated() {
    let store = InMemoryFullText::new();
    let a = scope("tenant-a", "p");
    let b = scope("tenant-b", "p");
    let doc = FullTextDoc {
        id: "d1".into(),
        revision: "r1".into(),
        fields: BTreeMap::from([("body".to_string(), "alpha beta".to_string())]),
    };
    store.index(&a, "icu-v1", &doc).unwrap();
    assert!(store
        .search(&b, "icu-v1", None, "alpha", 10)
        .unwrap()
        .is_empty());
    assert!(
        store
            .search(&a, "cjk-jieba-v1", None, "alpha", 10)
            .unwrap()
            .is_empty(),
        "analyzer version isolates the index"
    );
    store.delete(&a, "icu-v1", "d1").unwrap();
    assert!(store
        .search(&a, "icu-v1", None, "alpha", 10)
        .unwrap()
        .is_empty());
}

// ---------- Outbox ----------

#[test]
fn outbox_is_dedup_acked_and_replayable() {
    let store = InMemoryOutbox::new();
    let s = scope("t", "p");
    store.enqueue(&s, event("e1")).unwrap();
    store.enqueue(&s, event("e1")).unwrap();
    assert_eq!(store.len(&s), 1, "duplicate event_id is a no-op");

    let pending = store.pending(&s, "proj-1", 10).unwrap();
    assert_eq!(pending.len(), 1);
    store.ack(&s, "proj-1", "e1").unwrap();
    assert!(store.pending(&s, "proj-1", 10).unwrap().is_empty());

    // a second consumer still sees it: at-least-once per (event_id, consumer)
    assert_eq!(store.pending(&s, "proj-2", 10).unwrap().len(), 1);
}

#[test]
fn outbox_dead_letter_is_recorded() {
    let store = InMemoryOutbox::new();
    let s = scope("t", "p");
    store.enqueue(&s, event("e1")).unwrap();
    store
        .dead_letter(&s, "proj-1", "e1", "schema mismatch")
        .unwrap();
    assert_eq!(store.dead_letters(&s, "proj-1").unwrap(), vec!["e1"]);
    assert!(store.dead_letters(&s, "proj-2").unwrap().is_empty());
}

// ---------- Vector ----------

#[test]
fn vector_requires_declared_profile() {
    let store = InMemoryVector::new();
    let s = scope("t", "p");
    let item = VectorItem {
        id: "v1".into(),
        revision: "r1".into(),
        vector: vec![1.0, 0.0],
        metadata: BTreeMap::new(),
    };
    match store.upsert(&s, "missing", &[item]) {
        Err(StorageError::ModelProfileMismatch { .. }) => {}
        other => panic!("expected MODEL_PROFILE_MISMATCH, got {other:?}"),
    }
}

#[test]
fn vector_rejects_dimension_and_metric_drift() {
    let store = InMemoryVector::new();
    let s = scope("t", "p");
    store
        .declare_profile(&s, &profile("p1024", 3, Metric::Cosine))
        .unwrap();
    let wrong_dim = VectorItem {
        id: "v1".into(),
        revision: "r1".into(),
        vector: vec![1.0, 0.0],
        metadata: BTreeMap::new(),
    };
    match store.upsert(&s, "p1024", &[wrong_dim]) {
        Err(StorageError::ModelProfileMismatch { detail }) => {
            assert!(detail.contains("requires 3"), "{detail}")
        }
        other => panic!("expected dimension mismatch, got {other:?}"),
    }
    match store.declare_profile(&s, &profile("p1024", 3, Metric::L2)) {
        Err(StorageError::ModelProfileMismatch { .. }) => {}
        other => panic!("expected metric mismatch, got {other:?}"),
    }
}

#[test]
fn vector_search_orders_by_metric_and_filters() {
    let store = InMemoryVector::new();
    let s = scope("t", "p");
    store
        .declare_profile(&s, &profile("cos", 2, Metric::Cosine))
        .unwrap();
    let items = vec![
        VectorItem {
            id: "near".into(),
            revision: "r1".into(),
            vector: vec![1.0, 0.0],
            metadata: BTreeMap::from([("lang".to_string(), "en".to_string())]),
        },
        VectorItem {
            id: "far".into(),
            revision: "r1".into(),
            vector: vec![0.0, 1.0],
            metadata: BTreeMap::from([("lang".to_string(), "zh".to_string())]),
        },
    ];
    store.upsert(&s, "cos", &items).unwrap();
    let hits = store.search(&s, "cos", &[1.0, 0.0], 10, None).unwrap();
    assert_eq!(hits[0].id, "near");
    assert!((hits[0].score - 1.0).abs() < 1e-6);

    let filtered = store
        .search(
            &s,
            "cos",
            &[1.0, 0.0],
            10,
            Some(&BTreeMap::from([("lang".to_string(), "zh".to_string())])),
        )
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "far");
}

#[test]
fn vector_l2_is_a_distance() {
    let store = InMemoryVector::new();
    let s = scope("t", "p");
    store
        .declare_profile(&s, &profile("l2", 2, Metric::L2))
        .unwrap();
    store
        .upsert(
            &s,
            "l2",
            &[
                VectorItem {
                    id: "close".into(),
                    revision: "r1".into(),
                    vector: vec![0.1, 0.0],
                    metadata: BTreeMap::new(),
                },
                VectorItem {
                    id: "distant".into(),
                    revision: "r1".into(),
                    vector: vec![9.0, 0.0],
                    metadata: BTreeMap::new(),
                },
            ],
        )
        .unwrap();
    let hits = store.search(&s, "l2", &[0.0, 0.0], 10, None).unwrap();
    assert_eq!(hits[0].id, "close", "lower distance ranks first");
    assert!(hits[0].score < hits[1].score);
}

// ---------- Capability gate ----------

#[test]
fn in_memory_adapter_does_not_satisfy_the_minimum_profile() {
    let capabilities = InMemoryMetadata::new().capabilities();
    let missing = capabilities.missing_required();
    assert!(
        missing.contains(&"durable_outbox"),
        "in-memory must honestly report the missing capability, got {missing:?}"
    );
    assert!(capabilities.assert_minimum_profile().is_err());
}
