//! P10-WP1: three-profile port parity.
//!
//! Profile A (embedded) is a real deployment here, so every port it serves is
//! driven through one shared contract. Profiles B (sql) and C (hybrid) depend on
//! servers this workspace does not have; their adapters must therefore *declare*
//! themselves unavailable with a reason. The committed parity report is checked
//! against that reality, so it cannot claim a profile that does not exist.

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

const REPORT: &str = "contracts/testing/port-parity.json";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn temp_path(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-parity-{}-{tag}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn temp_dir(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-parity-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The one contract profile A must satisfy on every port it serves.
fn embedded_profile_contract(store: &EmbeddedStore, artifacts: &FilesystemArtifacts) {
    let scope = default_scope("tenant-a", "project-b");
    let other = default_scope("tenant-z", "project-b");

    // -- canonical metadata: CAS, revisioned read, typed key equality ----------
    let first = store
        .put_cas(&scope, "code/manifest", serde_json::json!({"gen": 1}), None)
        .unwrap();
    assert_eq!(first.revision, 1);
    let second = store
        .put_cas(
            &scope,
            "code/manifest",
            serde_json::json!({"gen": 2}),
            Some(first.revision),
        )
        .unwrap();
    assert_eq!(second.revision, 2);
    assert!(store
        .put_cas(
            &scope,
            "code/manifest",
            serde_json::json!({"gen": 3}),
            Some(first.revision),
        )
        .is_err());
    assert!(store.get(&other, "code/manifest").unwrap().is_none());
    store.put_manifest(&scope, "code", "gen-2").unwrap();
    assert_eq!(
        store.get_manifest(&scope, "code").unwrap().as_deref(),
        Some("gen-2")
    );

    // -- durable outbox -------------------------------------------------------
    store
        .enqueue(&scope, event("e1", "code.analysis.committed", 2))
        .unwrap();
    assert_eq!(store.pending(&scope, "projection", 10).unwrap().len(), 1);
    store.ack(&scope, "projection", "e1").unwrap();
    assert!(store.pending(&scope, "projection", 10).unwrap().is_empty());

    // -- full text projection ------------------------------------------------
    store
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
        FullTextPort::search(store, &scope, "icu-v1", None, "graph", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        FullTextPort::search(store, &other, "icu-v1", None, "graph", 10)
            .unwrap()
            .is_empty()
    );

    // -- vector projection: profile isolation --------------------------------
    store
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
    store
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
        VectorPort::search(store, &scope, "bge-m3", &[1.0, 0.0, 0.0, 0.0], 5, None)
            .unwrap()
            .len(),
        1
    );

    // -- immutable artifacts --------------------------------------------------
    let hash = artifacts
        .put_immutable(
            &scope,
            b"evidence bytes",
            "text/plain",
            Retention::KeepForever,
        )
        .unwrap();
    let record = artifacts
        .put_immutable(
            &scope,
            b"evidence bytes",
            "text/plain",
            Retention::KeepForever,
        )
        .unwrap();
    assert_eq!(record.hash, hash.hash, "content addressing is stable");
    assert!(artifacts.verify(&scope, &hash.hash).unwrap());
    assert_eq!(
        artifacts.get(&scope, &hash.hash).unwrap(),
        b"evidence bytes".to_vec()
    );
    assert!(artifacts.get(&other, &hash.hash).is_err());
}

#[test]
fn profile_a_serves_every_port_it_declares() {
    let store_path = temp_path("a");
    let artifact_root = temp_dir("a-artifacts");
    let store = EmbeddedStore::open(&store_path).unwrap();
    let artifacts = FilesystemArtifacts::open(&artifact_root).unwrap();

    store
        .capabilities()
        .assert_minimum_profile()
        .expect("embedded adapter meets the minimum profile");
    embedded_profile_contract(&store, &artifacts);

    // The same store is still consistent after a reload (durability is at flush).
    store.flush().unwrap();
    let reloaded = EmbeddedStore::open(&store_path).unwrap();
    let scope = default_scope("tenant-a", "project-b");
    assert_eq!(
        reloaded.get_manifest(&scope, "code").unwrap().as_deref(),
        Some("gen-2")
    );

    let _ = std::fs::remove_file(&store_path);
    let _ = std::fs::remove_dir_all(&artifact_root);
}

#[test]
fn profiles_b_and_c_declare_themselves_unavailable_with_a_reason() {
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
        let status = profile["status"].as_str().expect("status");
        match status {
            "available" => {
                assert_eq!(id, "A", "only profile A is available in this workspace");
                // Every port of the available profile must be listed.
                let ports = profile["ports"].as_object().expect("ports");
                for port in [
                    "MetadataPort",
                    "OutboxPort",
                    "FullTextPort",
                    "VectorPort",
                    "ArtifactPort",
                    "GraphPort",
                ] {
                    assert_eq!(
                        ports.get(port).and_then(Value::as_str),
                        Some("available"),
                        "profile A does not serve {port}"
                    );
                }
            }
            "unavailable" => {
                assert_ne!(id, "A", "profile A is available");
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
}
