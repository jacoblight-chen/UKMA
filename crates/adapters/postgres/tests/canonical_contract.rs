//! PostgreSQL canonical adapter contract (profile C, DB-002/DB-010).
//!
//! The same cases the embedded and SQLite adapters pass, plus a cross-backend
//! equivalence check. Every test needs a live server:
//!
//! ```text
//! set UKMA_POSTGRES_DSN=host=127.0.0.1 port=55432 user=postgres dbname=postgres
//! cargo test -p ukma-adapter-postgres
//! ```
//!
//! Without the variable the tests report a skip and pass: a machine without a
//! server must not fail the build, but it must not claim coverage either — the
//! parity report records which profiles were really exercised.

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::json;
use storage_ports::capabilities::CapabilityProbe;
use storage_ports::{
    canonical_migrations, migrate, MetadataPort, OutboxPort, SchemaVersionPort, StorageError,
    CANONICAL_SCHEMA_VERSION,
};
use storage_ports::{EmbeddingProfile, FullTextDoc, FullTextPort, Metric, VectorItem, VectorPort};
use ukma_adapter_embedded::EmbeddedStore;
use ukma_adapter_postgres::{capabilities_with, event, PostgresStore, DSN_ENV};
use ukma_contracts::ScopeRef;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A store plus the scope it owns; `None` without `UKMA_POSTGRES_DSN`.
fn opened(tag: &str) -> Option<(PostgresStore, ScopeRef)> {
    let dsn = std::env::var(DSN_ENV).ok()?;
    if dsn.trim().is_empty() {
        return None;
    }
    let store = PostgresStore::connect(&dsn).expect("connect to the configured PostgreSQL");
    // A unique scope per test and run keeps concurrent runs independent.
    let scope = ukma_adapter_postgres::default_scope(
        &format!("tenant-pg-{}", std::process::id()),
        &format!("proj-{tag}-{}", COUNTER.fetch_add(1, Ordering::SeqCst)),
    );
    store.purge_scope(&scope).expect("clean scope");
    Some((store, scope))
}

macro_rules! needs_server {
    ($tag:literal) => {
        match opened($tag) {
            Some(pair) => pair,
            None => {
                eprintln!("skipped: {} is not set", DSN_ENV);
                return;
            }
        }
    };
}

#[test]
fn cas_produces_monotonic_revisions_and_content_etags() {
    let (store, scope) = needs_server!("cas");
    let first = store
        .put_cas(&scope, "canon:a", json!({"v": 1}), None)
        .unwrap();
    assert_eq!(first.revision, 1);
    let second = store
        .put_cas(&scope, "canon:a", json!({"v": 2}), Some(1))
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_ne!(first.etag, second.etag);

    let conflict = store
        .put_cas(&scope, "canon:a", json!({"v": 3}), Some(1))
        .unwrap_err();
    assert!(matches!(
        conflict,
        StorageError::Conflict {
            expected_revision: 1,
            actual_revision: 2,
            ..
        }
    ));
    // `None` is "no precondition", so it bumps an existing key rather than
    // refusing it; a CAS against a stale revision is what must fail.
    let bumped = store
        .put_cas(&scope, "canon:a", json!({"v": 4}), None)
        .unwrap();
    assert_eq!(bumped.revision, 3);

    store.purge_scope(&scope).unwrap();
}

#[test]
fn paging_is_cursor_stable_and_prefix_bounded() {
    let (store, scope) = needs_server!("paging");
    for index in 0..7 {
        store
            .put_cas(&scope, &format!("canon:{index}"), json!({"i": index}), None)
            .unwrap();
    }
    store
        .put_cas(&scope, "other:1", json!({"i": 99}), None)
        .unwrap();

    let (first, cursor) = store.list(&scope, "canon:", 3, None).unwrap();
    assert_eq!(first.len(), 3);
    let cursor = cursor.expect("a full page returns a cursor");
    let (second, cursor) = store.list(&scope, "canon:", 3, Some(&cursor)).unwrap();
    assert_eq!(second.len(), 3);
    assert!(second[0].key > first[2].key);
    let (last, cursor) = store.list(&scope, "canon:", 3, cursor.as_deref()).unwrap();
    assert_eq!(last.len(), 1);
    assert!(cursor.is_none());
    let mut keys: Vec<&str> = first
        .iter()
        .chain(second.iter())
        .chain(last.iter())
        .map(|entry| entry.key.as_str())
        .collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 7, "no duplicates and no prefix leak");

    store.purge_scope(&scope).unwrap();
}

#[test]
fn manifests_and_single_writer_fencing_hold() {
    let (store, scope) = needs_server!("fencing");
    assert!(store.get_manifest(&scope, "code").unwrap().is_none());
    store.put_manifest(&scope, "code", "gen-1").unwrap();
    store.put_manifest(&scope, "code", "gen-2").unwrap();
    assert_eq!(
        store.get_manifest(&scope, "code").unwrap().as_deref(),
        Some("gen-2")
    );

    store.acquire_writer(&scope, "code", "token-a").unwrap();
    store.acquire_writer(&scope, "code", "token-a").unwrap();
    let blocked = store.acquire_writer(&scope, "code", "token-b").unwrap_err();
    assert_eq!(blocked.code(), "ETAG_MISMATCH");

    store.purge_scope(&scope).unwrap();
}

#[test]
fn the_outbox_dedups_per_event_and_per_consumer() {
    let (store, scope) = needs_server!("outbox");
    store
        .enqueue(&scope, event("evt-1", "memory.updated", 1))
        .unwrap();
    store
        .enqueue(&scope, event("evt-2", "memory.updated", 2))
        .unwrap();
    // The same event id twice is one row: at-least-once, never twice.
    store
        .enqueue(&scope, event("evt-1", "memory.updated", 1))
        .unwrap();
    assert_eq!(store.pending(&scope, "consumer-a", 10).unwrap().len(), 2);

    store.ack(&scope, "consumer-a", "evt-1").unwrap();
    let left = store.pending(&scope, "consumer-a", 10).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].event_id, "evt-2");
    // Another consumer still sees both.
    assert_eq!(store.pending(&scope, "consumer-b", 10).unwrap().len(), 2);

    store
        .dead_letter(&scope, "consumer-b", "evt-2", "poison payload")
        .unwrap();
    assert_eq!(
        store.dead_letters(&scope, "consumer-b").unwrap(),
        vec!["evt-2"]
    );
    assert_eq!(store.pending(&scope, "consumer-b", 10).unwrap().len(), 1);

    store.purge_scope(&scope).unwrap();
}

#[test]
fn a_foreign_scope_is_invisible() {
    let (store, scope) = needs_server!("isolation");
    let other = ukma_adapter_postgres::default_scope("tenant-other", &scope.project_id);
    store
        .put_cas(&scope, "canon:secret", json!({"v": 1}), None)
        .unwrap();

    assert!(store.get(&other, "canon:secret").unwrap().is_none());
    let (page, _) = store.list(&other, "canon:", 10, None).unwrap();
    assert!(page.is_empty(), "another tenant must not see the record");
    assert_eq!(store.pending(&other, "consumer-a", 10).unwrap().len(), 0);
    // The read from the owning scope still works.
    assert!(store.get(&scope, "canon:secret").unwrap().is_some());

    store.purge_scope(&scope).unwrap();
}

#[test]
fn the_canonical_migration_runs_against_postgres() {
    let (store, scope) = needs_server!("migration");
    for index in 0..3 {
        store
            .put_cas(
                &scope,
                &format!("canon:m{index}"),
                json!({"i": index}),
                None,
            )
            .unwrap();
    }
    store.set_schema_version(1).unwrap();
    let steps = canonical_migrations();
    let report = migrate(&store, &scope, &steps, CANONICAL_SCHEMA_VERSION).unwrap();
    assert_eq!(report.from, 1);
    assert_eq!(report.to, 2);
    assert_eq!(report.applied, vec!["v2:stamp-schema-version"]);
    let (page, _) = store.list(&scope, "canon:", 10, None).unwrap();
    assert_eq!(page.len(), 3);
    assert!(page
        .iter()
        .all(|entry| entry.value.get("schema_version").and_then(|v| v.as_u64()) == Some(2)));
    // Idempotent, and the version survives a reconnect.
    assert!(migrate(&store, &scope, &steps, 2).unwrap().skipped);
    let dsn = std::env::var(DSN_ENV).unwrap();
    let reopened = PostgresStore::connect(&dsn).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 2);

    store.set_schema_version(1).unwrap();
    store.purge_scope(&scope).unwrap();
}

#[test]
fn postgres_and_embedded_agree_on_what_they_store() {
    let (postgres, scope) = needs_server!("equivalence");
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-pg-equiv-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let embedded = EmbeddedStore::open(&path).unwrap();

    for index in 0..5 {
        let key = format!("canon:e{index}");
        let value = json!({"summary": format!("record {index}")});
        let from_pg = postgres.put_cas(&scope, &key, value.clone(), None).unwrap();
        let from_embedded = embedded.put_cas(&scope, &key, value, None).unwrap();
        assert_eq!(from_pg.revision, from_embedded.revision, "{key}");
        assert_eq!(from_pg.etag, from_embedded.etag, "{key}");
    }
    let (pg_page, _) = postgres.list(&scope, "canon:", 10, None).unwrap();
    let (embedded_page, _) = embedded.list(&scope, "canon:", 10, None).unwrap();
    assert_eq!(pg_page, embedded_page);

    postgres.purge_scope(&scope).unwrap();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_adapter_is_available_only_with_a_dsn_and_then_meets_the_minimum_profile() {
    let unconfigured = capabilities_with(None);
    assert!(!unconfigured.available);
    assert!(unconfigured.assert_minimum_profile().is_err());
    assert!(unconfigured
        .unavailable_reason
        .contains("UKMA_POSTGRES_DSN"));

    let configured = capabilities_with(Some("host=127.0.0.1 port=55432 user=postgres"));
    assert!(configured.available);
    configured
        .assert_minimum_profile()
        .expect("a configured PostgreSQL adapter satisfies the minimum profile");
    assert!(configured
        .supported_ports
        .contains(&"MetadataPort".to_string()));
    assert!(configured
        .supported_ports
        .contains(&"OutboxPort".to_string()));
}

#[test]
fn the_store_reports_the_same_capabilities_as_the_probe() {
    let (store, scope) = needs_server!("probe");
    let reported = CapabilityProbe::capabilities(&store);
    assert!(reported.available);
    reported.assert_minimum_profile().unwrap();
    assert_eq!(reported.adapter_id, "postgres");
    store.purge_scope(&scope).unwrap();
}

#[test]
fn fulltext_is_ranked_inside_the_database() {
    use std::collections::BTreeMap;

    let (store, scope) = needs_server!("fulltext");
    let doc = |id: &str, body: &str, title: &str| {
        let mut fields = BTreeMap::new();
        fields.insert("body".to_string(), body.to_string());
        fields.insert("title".to_string(), title.to_string());
        FullTextDoc {
            id: id.to_string(),
            revision: "r1".to_string(),
            fields,
        }
    };
    store
        .index(
            &scope,
            "icu-v1",
            &doc("d1", "graph traversal over nodes", "graph"),
        )
        .unwrap();
    store
        .index(
            &scope,
            "icu-v1",
            &doc("d2", "graph graph graph traversal", "graph graph"),
        )
        .unwrap();
    store
        .index(&scope, "icu-v1", &doc("d3", "unrelated content", "other"))
        .unwrap();

    let hits = FullTextPort::search(&store, &scope, "icu-v1", None, "graph", 10).unwrap();
    assert_eq!(hits.len(), 2, "only matching documents come back");
    assert_eq!(hits[0].id, "d2", "the denser document ranks first");
    assert!(hits[0].score >= hits[1].score);
    assert_eq!(hits[0].revision, "r1");

    // Field-scoped search only looks at that field.
    let title_only =
        FullTextPort::search(&store, &scope, "icu-v1", Some("title"), "other", 10).unwrap();
    assert_eq!(title_only.len(), 1);
    assert_eq!(title_only[0].id, "d3");

    // Analyser isolation: another label sees nothing.
    assert!(
        FullTextPort::search(&store, &scope, "cjk-jieba-v1", None, "graph", 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(FullTextPort::analyzers(&store), vec!["icu-v1".to_string()]);

    FullTextPort::delete(&store, &scope, "icu-v1", "d2").unwrap();
    let after = FullTextPort::search(&store, &scope, "icu-v1", None, "graph", 10).unwrap();
    assert_eq!(after.len(), 1);
    store.purge_scope(&scope).unwrap();
}

#[test]
fn vectors_are_searched_in_the_database_and_the_profile_survives_a_reconnect() {
    use std::collections::BTreeMap;

    let (store, scope) = needs_server!("vector");
    let profile = EmbeddingProfile {
        profile_id: "bge-m3".into(),
        model_id: "bge-m3".into(),
        model_revision: "2024-01".into(),
        dimension: 4,
        metric: Metric::Cosine,
        normalization: "l2".into(),
    };
    VectorPort::declare_profile(&store, &scope, &profile).unwrap();

    let item = |id: &str, vector: Vec<f32>, topic: &str| {
        let mut metadata = BTreeMap::new();
        metadata.insert("topic".to_string(), topic.to_string());
        VectorItem {
            id: id.to_string(),
            revision: "r1".to_string(),
            vector,
            metadata,
        }
    };
    VectorPort::upsert(
        &store,
        &scope,
        "bge-m3",
        &[
            item("a", vec![1.0, 0.0, 0.0, 0.0], "alpha"),
            item("b", vec![0.0, 1.0, 0.0, 0.0], "beta"),
            item("c", vec![0.9, 0.1, 0.0, 0.0], "alpha"),
        ],
    )
    .unwrap();

    let hits =
        VectorPort::search(&store, &scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 3, None).unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "a");
    assert!(hits[0].score > 0.99, "cosine of an identical vector is 1.0");
    assert!(hits[0].score > hits[2].score);

    let mut filter = BTreeMap::new();
    filter.insert("topic".to_string(), "beta".to_string());
    let filtered = VectorPort::search(
        &store,
        &scope,
        "bge-m3",
        &[1.0, 0.0, 0.0, 0.0],
        5,
        Some(&filter),
    )
    .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "b");

    // A wrong dimension is refused rather than silently truncated.
    let wrong = VectorPort::upsert(
        &store,
        &scope,
        "bge-m3",
        &[item("x", vec![1.0, 0.0], "alpha")],
    )
    .unwrap_err();
    assert_eq!(wrong.code(), "MODEL_PROFILE_MISMATCH");

    // The profile lives in the database, not in process memory.
    let dsn = std::env::var(DSN_ENV).unwrap();
    let reopened = PostgresStore::connect(&dsn).unwrap();
    let restored = VectorPort::profile(&reopened, &scope, "bge-m3")
        .unwrap()
        .unwrap();
    assert_eq!(restored.dimension, 4);
    assert_eq!(restored.metric, Metric::Cosine);
    assert_eq!(restored.model_revision, "2024-01");

    VectorPort::delete(&store, &scope, "bge-m3", "b").unwrap();
    assert_eq!(
        VectorPort::search(&store, &scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, None)
            .unwrap()
            .len(),
        2
    );
    store.purge_scope(&scope).unwrap();
}
