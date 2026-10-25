//! Qdrant vector adapter contract (profile C).
//!
//! Needs a live server:
//!
//! ```text
//! set UKMA_QDRANT_URL=http://127.0.0.1:6333
//! cargo test -p ukma-adapter-qdrant
//! ```
//!
//! Without the variable the tests report a skip and pass.

use std::collections::BTreeMap;

use storage_ports::capabilities::CapabilityProbe;
use storage_ports::ports::{EmbeddingProfile, Metric, VectorItem, VectorPort};
use ukma_adapter_qdrant::{capabilities_with, QdrantStore, URL_ENV};
use ukma_contracts::ScopeRef;

fn scope(project: &str) -> ScopeRef {
    ScopeRef {
        tenant_id: "tenant-qdrant".into(),
        project_id: project.into(),
        repo_id: None,
        workspace_id: None,
        agent_id: None,
        session_id: None,
    }
}

fn profile(dimension: usize) -> EmbeddingProfile {
    EmbeddingProfile {
        profile_id: "bge-m3".into(),
        model_id: "bge-m3".into(),
        model_revision: "2024-01".into(),
        dimension,
        metric: Metric::Cosine,
        normalization: "l2".into(),
    }
}

fn item(id: &str, vector: Vec<f32>, topic: &str) -> VectorItem {
    let mut metadata = BTreeMap::new();
    metadata.insert("topic".to_string(), topic.to_string());
    VectorItem {
        id: id.to_string(),
        revision: "r1".to_string(),
        vector,
        metadata,
    }
}

fn opened(tag: &str) -> Option<(QdrantStore, ScopeRef)> {
    let url = std::env::var(URL_ENV).ok()?;
    if url.trim().is_empty() {
        return None;
    }
    let store = QdrantStore::connect(&url, None).expect("connect to the configured Qdrant");
    Some((store, scope(tag)))
}

macro_rules! needs_server {
    ($tag:literal) => {
        match opened($tag) {
            Some(pair) => pair,
            None => {
                eprintln!("skipped: {} is not set", URL_ENV);
                return;
            }
        }
    };
}

#[test]
fn a_profile_round_trips_and_a_dimension_change_is_refused() {
    let (store, scope) = needs_server!("profile");
    store.declare_profile(&scope, &profile(4)).unwrap();
    let stored = store.profile(&scope, "bge-m3").unwrap().unwrap();
    assert_eq!(stored.dimension, 4);
    assert_eq!(stored.metric, Metric::Cosine);
    // Same declaration is idempotent.
    store.declare_profile(&scope, &profile(4)).unwrap();
    // A different dimension must not silently re-shape the collection.
    let mismatch = store.declare_profile(&scope, &profile(8)).unwrap_err();
    assert_eq!(mismatch.code(), "MODEL_PROFILE_MISMATCH");
}

#[test]
fn upsert_search_and_delete_round_trip() {
    let (store, scope) = needs_server!("roundtrip");
    store.declare_profile(&scope, &profile(4)).unwrap();
    store
        .upsert(
            &scope,
            "bge-m3",
            &[
                item("a", vec![1.0, 0.0, 0.0, 0.0], "alpha"),
                item("b", vec![0.0, 1.0, 0.0, 0.0], "beta"),
                item("c", vec![0.9, 0.1, 0.0, 0.0], "alpha"),
            ],
        )
        .unwrap();

    let hits = store
        .search(&scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 3, None)
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "a", "the closest vector wins");
    assert_eq!(hits[0].revision, "r1");
    assert!(hits[0].score > hits[2].score);

    let mut filter = BTreeMap::new();
    filter.insert("topic".to_string(), "beta".to_string());
    let filtered = store
        .search(&scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, Some(&filter))
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "b");

    store.delete(&scope, "bge-m3", "b").unwrap();
    let after = store
        .search(&scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, None)
        .unwrap();
    assert_eq!(after.len(), 2, "the deleted point is gone");
    assert!(after.iter().all(|hit| hit.id != "b"));
}

#[test]
fn a_wrong_dimension_or_undeclared_profile_is_refused() {
    let (store, scope) = needs_server!("mismatch");
    store.declare_profile(&scope, &profile(4)).unwrap();
    let wrong = store
        .upsert(&scope, "bge-m3", &[item("x", vec![1.0, 0.0], "alpha")])
        .unwrap_err();
    assert_eq!(wrong.code(), "MODEL_PROFILE_MISMATCH");

    let undeclared = store
        .upsert(
            &scope,
            "other-profile",
            &[item("y", vec![1.0, 0.0], "alpha")],
        )
        .unwrap_err();
    assert_eq!(undeclared.code(), "MODEL_PROFILE_MISMATCH");
    let query = store
        .search(&scope, "bge-m3", &[1.0, 0.0], 3, None)
        .unwrap_err();
    assert_eq!(query.code(), "MODEL_PROFILE_MISMATCH");
}

#[test]
fn one_scope_and_one_profile_never_see_another() {
    let (store, left) = needs_server!("iso-left");
    let right = scope("proj-qdrant-iso-right");
    store.declare_profile(&left, &profile(4)).unwrap();
    store.declare_profile(&right, &profile(4)).unwrap();
    store
        .upsert(
            &left,
            "bge-m3",
            &[item("left-1", vec![1.0, 0.0, 0.0, 0.0], "alpha")],
        )
        .unwrap();

    let right_hits = store
        .search(&right, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, None)
        .unwrap();
    assert!(
        right_hits.is_empty(),
        "another scope must not see the vector"
    );
    let left_hits = store
        .search(&left, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, None)
        .unwrap();
    assert_eq!(left_hits.len(), 1);
}

#[test]
fn the_adapter_is_available_only_with_a_url() {
    let unconfigured = capabilities_with(None);
    assert!(!unconfigured.available);
    assert!(unconfigured.unavailable_reason.contains("UKMA_QDRANT_URL"));
    let configured = capabilities_with(Some("http://127.0.0.1:6333"));
    assert!(configured.available);
    assert!(configured.vector_filters, "filtered search is supported");
    assert_eq!(
        configured.supported_ports,
        vec!["VectorPort".to_string()],
        "Qdrant serves vectors only"
    );
}

#[test]
fn the_store_reports_its_own_capabilities() {
    let (store, _scope) = needs_server!("probe");
    let reported = CapabilityProbe::capabilities(&store);
    assert!(reported.available);
    assert_eq!(reported.adapter_id, "qdrant");
}
