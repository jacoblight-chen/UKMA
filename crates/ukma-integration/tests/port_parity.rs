//! P10-WP1: three-profile port parity.
//!
//! Profile A (embedded JSON) and profile B (SQLite) are real, production-capable
//! deployments in this workspace, so **one shared contract** runs against both —
//! the same assertions, different backends. Profile C needs PostgreSQL, Qdrant and
//! Neo4j concurrently; its adapters must declare themselves unavailable with a
//! reason, and the committed report is checked against that reality.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use storage_ports::capabilities::CapabilityProbe;
use storage_ports::{
    ArtifactPort, EmbeddingProfile, FullTextDoc, FullTextPort, MetadataPort, Metric, OutboxPort,
    Retention, VectorItem, VectorPort,
};
use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};
use ukma_adapter_filesystem::FilesystemArtifacts;
use ukma_adapter_sqlite::SqliteStore;

const REPORT: &str = "contracts/testing/port-parity.json";

/// The canonical port pair every backend must implement.
trait CanonicalStore: MetadataPort + OutboxPort {}
impl<T: MetadataPort + OutboxPort> CanonicalStore for T {}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn temp(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-parity-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = temp(tag);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let _ = std::fs::remove_dir_all(path);
}

/// The one contract every canonical backend must satisfy.
fn canonical_contract(store: &dyn CanonicalStore, label: &str, artifacts: &FilesystemArtifacts) {
    let scope = default_scope("tenant-a", "project-b");
    let other = default_scope("tenant-z", "project-b");

    // -- CAS, revisioned read, typed key equality, scope isolation ------------
    let first = store
        .put_cas(&scope, "code/manifest", serde_json::json!({"gen": 1}), None)
        .unwrap_or_else(|error| panic!("{label} put_cas: {error}"));
    assert_eq!(first.revision, 1, "{label}");
    let second = store
        .put_cas(
            &scope,
            "code/manifest",
            serde_json::json!({"gen": 2}),
            Some(first.revision),
        )
        .unwrap_or_else(|error| panic!("{label} put_cas: {error}"));
    assert_eq!(second.revision, 2, "{label}");
    assert!(
        store
            .put_cas(
                &scope,
                "code/manifest",
                serde_json::json!({"gen": 3}),
                Some(first.revision),
            )
            .is_err(),
        "{label} accepted a stale revision"
    );
    assert!(
        store.get(&other, "code/manifest").unwrap().is_none(),
        "{label} leaked a record across scopes"
    );
    let (page, _cursor) = store.list(&scope, "code/", 10, None).unwrap();
    assert_eq!(page.len(), 1, "{label}");
    assert_eq!(page[0].revision, 2, "{label}");

    // -- manifests are a separate atomic pointer ------------------------------
    store.put_manifest(&scope, "code", "gen-2").unwrap();
    assert_eq!(
        store.get_manifest(&scope, "code").unwrap().as_deref(),
        Some("gen-2"),
        "{label}"
    );

    // -- durable outbox with per-consumer ack --------------------------------
    store
        .enqueue(&scope, event("e1", "code.analysis.committed", 2))
        .unwrap();
    assert_eq!(store.pending(&scope, "projection", 10).unwrap().len(), 1);
    store.ack(&scope, "projection", "e1").unwrap();
    assert!(store.pending(&scope, "projection", 10).unwrap().is_empty());
    assert_eq!(
        store.pending(&scope, "audit", 10).unwrap().len(),
        1,
        "{label}: ack must be per consumer"
    );

    // -- single-writer fencing -----------------------------------------------
    store.acquire_writer(&scope, "agg:1", "writer-a").unwrap();
    assert!(
        store.acquire_writer(&scope, "agg:1", "writer-b").is_err(),
        "{label} allowed two live writers"
    );

    // -- immutable artifacts (shared adapter, still part of the contract) ----
    let hash = artifacts
        .put_immutable(
            &scope,
            b"evidence bytes",
            "text/plain",
            Retention::KeepForever,
        )
        .unwrap();
    let again = artifacts
        .put_immutable(
            &scope,
            b"evidence bytes",
            "text/plain",
            Retention::KeepForever,
        )
        .unwrap();
    assert_eq!(again.hash, hash.hash, "{label}: content addressing");
    assert!(artifacts.verify(&scope, &hash.hash).unwrap());
    assert_eq!(
        artifacts.get(&scope, &hash.hash).unwrap(),
        b"evidence bytes".to_vec()
    );
    assert!(artifacts.get(&other, &hash.hash).is_err());
}

/// The derived projections (in-memory by design in both profiles).
fn derived_contract<F, V>(fulltext: &F, vector: &V, label: &str)
where
    F: FullTextPort,
    V: VectorPort,
{
    let scope = default_scope("tenant-a", "project-b");
    let other = default_scope("tenant-z", "project-b");

    fulltext
        .index(
            &scope,
            "icu-v1",
            &FullTextDoc {
                id: "doc-1".into(),
                revision: "r1".into(),
                fields: BTreeMap::from([("body".to_string(), "graph engine".to_string())]),
            },
        )
        .unwrap();
    assert_eq!(
        fulltext
            .search(&scope, "icu-v1", None, "graph", 10)
            .unwrap()
            .len(),
        1,
        "{label}"
    );
    assert!(fulltext
        .search(&other, "icu-v1", None, "graph", 10)
        .unwrap()
        .is_empty());

    vector
        .declare_profile(
            &scope,
            &EmbeddingProfile {
                profile_id: "bge-m3".into(),
                model_id: "bge-m3".into(),
                model_revision: "1".into(),
                dimension: 4,
                metric: Metric::Cosine,
                normalization: "l2".into(),
            },
        )
        .unwrap();
    vector
        .upsert(
            &scope,
            "bge-m3",
            &[VectorItem {
                id: "v1".into(),
                revision: "r1".into(),
                vector: vec![1.0, 0.0, 0.0, 0.0],
                metadata: BTreeMap::new(),
            }],
        )
        .unwrap();
    assert_eq!(
        vector
            .search(&scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, None)
            .unwrap()
            .len(),
        1,
        "{label}"
    );
    // A vector from another profile is never compared (PROP-07).
    assert!(vector.search(&scope, "bge-m3", &[1.0; 8], 5, None).is_err());
}

#[test]
fn profile_a_embedded_json_serves_every_port() {
    let store_path = temp("a.json");
    let artifact_root = temp_dir("a-artifacts");
    let store = EmbeddedStore::open(&store_path).unwrap();
    let artifacts = FilesystemArtifacts::open(&artifact_root).unwrap();

    store
        .capabilities()
        .assert_minimum_profile()
        .expect("embedded adapter meets the minimum profile");
    canonical_contract(&store, "profile A", &artifacts);
    derived_contract(&store, &store, "profile A");

    store.flush().unwrap();
    let reloaded = EmbeddedStore::open(&store_path).unwrap();
    let scope = default_scope("tenant-a", "project-b");
    assert_eq!(
        reloaded.get_manifest(&scope, "code").unwrap().as_deref(),
        Some("gen-2")
    );
    cleanup(&store_path);
    cleanup(&artifact_root);
}

#[test]
fn profile_b_sqlite_passes_the_same_contract() {
    let database = temp("b.db");
    let artifact_root = temp_dir("b-artifacts");
    let store = SqliteStore::open(&database).unwrap();
    let artifacts = FilesystemArtifacts::open(&artifact_root).unwrap();

    store
        .capabilities()
        .assert_minimum_profile()
        .expect("sqlite adapter meets the minimum profile");
    canonical_contract(&store, "profile B", &artifacts);
    derived_contract(&store, &store, "profile B");

    store.checkpoint().unwrap();
    let reloaded = SqliteStore::open(&database).unwrap();
    let scope = default_scope("tenant-a", "project-b");
    assert_eq!(
        reloaded.get_manifest(&scope, "code").unwrap().as_deref(),
        Some("gen-2")
    );
    assert!(reloaded.file_size() > 0, "the SQL database is a real file");
    cleanup(&database);
    cleanup(&artifact_root);
}

#[test]
fn the_two_available_profiles_agree_on_what_they_store() {
    // Two-way equivalence: the same writes through both backends produce the same
    // observable entries (etags included, because the etag is content-derived).
    let embedded_path = temp("equiv.json");
    let sqlite_path = temp("equiv.db");
    let scope = default_scope("tenant-a", "project-b");

    let embedded = EmbeddedStore::open(&embedded_path).unwrap();
    let sqlite = SqliteStore::open(&sqlite_path).unwrap();
    for index in 0..5 {
        let key = format!("memory/mem:{index}");
        let value = serde_json::json!({"summary": format!("record {index}")});
        let from_embedded = embedded.put_cas(&scope, &key, value.clone(), None).unwrap();
        let from_sqlite = sqlite.put_cas(&scope, &key, value, None).unwrap();
        assert_eq!(from_embedded.revision, from_sqlite.revision, "{key}");
        assert_eq!(from_embedded.etag, from_sqlite.etag, "{key}");
    }
    let (embedded_page, _) = embedded.list(&scope, "memory/", 10, None).unwrap();
    let (sqlite_page, _) = sqlite.list(&scope, "memory/", 10, None).unwrap();
    assert_eq!(embedded_page, sqlite_page);

    cleanup(&embedded_path);
    cleanup(&sqlite_path);
}

#[test]
fn profile_c_declares_itself_unavailable_with_a_reason() {
    // A profile is only "declared" when its adapters really say so. Nothing here
    // is allowed to look available without a server behind it.
    for (name, capabilities) in [
        ("postgres", ukma_adapter_postgres::capabilities()),
        ("qdrant", ukma_adapter_qdrant::capabilities()),
        ("neo4j", ukma_adapter_neo4j::capabilities()),
    ] {
        assert!(!capabilities.available, "{name} claims to be available");
        assert!(
            !capabilities.unavailable_reason.trim().is_empty(),
            "{name} is unavailable without a reason"
        );
        assert!(
            capabilities.assert_minimum_profile().is_err(),
            "{name} must fail closed"
        );
    }
}

#[test]
fn the_committed_parity_report_matches_the_adapters() {
    let path = root().join(REPORT);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let report: Value = serde_json::from_str(&text).expect("the parity report is JSON");

    let profiles = report["profiles"].as_array().expect("profiles array");
    assert_eq!(profiles.len(), 3, "three profiles are declared (A/B/C)");

    let mut seen: Vec<&str> = Vec::new();
    for profile in profiles {
        let id = profile["id"].as_str().expect("profile id");
        seen.push(id);
        match profile["status"].as_str().expect("status") {
            "available" => {
                assert!(
                    ["A", "B"].contains(&id),
                    "profile {id} cannot be available: only the embedded and SQLite backends exist"
                );
                let ports = profile["ports"].as_object().expect("ports");
                for port in ["MetadataPort", "OutboxPort", "FullTextPort", "VectorPort"] {
                    assert_eq!(
                        ports.get(port).and_then(Value::as_str),
                        Some("available"),
                        "profile {id} does not serve {port}"
                    );
                }
            }
            "unavailable" => {
                let reason = profile["reason"].as_str().unwrap_or_default();
                assert!(
                    reason.len() > 20,
                    "profile {id} is unavailable without a real reason"
                );
            }
            other => panic!("profile {id} has an unknown status `{other}`"),
        }
    }
    seen.sort();
    assert_eq!(seen, vec!["A", "B", "C"]);
    assert_eq!(report["rocksdb_loaded"], false);
    assert_eq!(report["spec_version"], "1.2.3");
    assert_eq!(
        report["two_way_equivalence"]["verified"], true,
        "the report must state whether the two available profiles were compared"
    );
}
