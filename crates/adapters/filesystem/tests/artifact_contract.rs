//! Artifact adapter tests against the real filesystem (profile A).

use std::path::PathBuf;

use storage_ports::capabilities::CapabilityProbe;
use storage_ports::{ArtifactPort, Retention, StorageError};
use ukma_adapter_filesystem::FilesystemArtifacts;
use ukma_contracts::ScopeRef;

fn temp_root(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-artifacts-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn scope(tenant: &str) -> ScopeRef {
    ScopeRef {
        tenant_id: tenant.into(),
        project_id: "project-1".into(),
        repo_id: None,
        workspace_id: None,
        agent_id: None,
        session_id: None,
    }
}

#[test]
fn round_trip_is_content_addressed() {
    let root = temp_root("roundtrip");
    let store = FilesystemArtifacts::open(&root).unwrap();
    let s = scope("t");
    let record = store
        .put_immutable(&s, b"payload", "text/plain", Retention::KeepForever)
        .unwrap();
    assert!(record.hash.starts_with("sha256:"));
    assert_eq!(record.byte_len, 7);
    assert_eq!(store.get(&s, &record.hash).unwrap(), b"payload");
    assert!(store.exists(&s, &record.hash).unwrap());
    assert!(store.verify(&s, &record.hash).unwrap());

    // idempotent: same bytes, same address, no duplicate object
    let again = store
        .put_immutable(&s, b"payload", "text/plain", Retention::KeepForever)
        .unwrap();
    assert_eq!(again.hash, record.hash);
    let objects = std::fs::read_dir(root.join("objects")).unwrap().count();
    assert_eq!(objects, 1, "content addressing deduplicates bytes");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tampering_is_detected() {
    let root = temp_root("tamper");
    let store = FilesystemArtifacts::open(&root).unwrap();
    let s = scope("t");
    let record = store
        .put_immutable(&s, b"original", "text/plain", Retention::KeepForever)
        .unwrap();

    let hex = record.hash.strip_prefix("sha256:").unwrap();
    std::fs::write(root.join("objects").join(hex), b"tampered").unwrap();

    assert!(
        !store.verify(&s, &record.hash).unwrap(),
        "content address must reveal tampering"
    );
    assert_eq!(store.get(&s, &record.hash).unwrap(), b"tampered");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scope_isolation_hides_other_tenants_artifacts() {
    let root = temp_root("isolation");
    let store = FilesystemArtifacts::open(&root).unwrap();
    let a = scope("tenant-a");
    let b = scope("tenant-b");
    let record = store
        .put_immutable(&a, b"secret", "text/plain", Retention::KeepForever)
        .unwrap();
    assert!(!store.exists(&b, &record.hash).unwrap());
    match store.get(&b, &record.hash) {
        Err(StorageError::NotFound { .. }) => {}
        other => panic!("expected NotFound cross-scope, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn writes_leave_no_temporary_files() {
    let root = temp_root("temp");
    let store = FilesystemArtifacts::open(&root).unwrap();
    let s = scope("t");
    store
        .put_immutable(&s, b"atomic", "text/plain", Retention::KeepForever)
        .unwrap();
    let leftovers = std::fs::read_dir(root.join("objects"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("tmp"))
        .count();
    assert_eq!(leftovers, 0, "atomic write must not leave .tmp files");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn capability_is_declared_but_not_canonical_metadata() {
    let root = temp_root("caps");
    let store = FilesystemArtifacts::open(&root).unwrap();
    let capabilities = store.capabilities();
    assert!(capabilities.available);
    assert_eq!(
        capabilities.supported_ports,
        vec!["ArtifactPort".to_string()]
    );
    assert!(
        !capabilities.durable_outbox,
        "the artifact adapter must not claim outbox capabilities it does not provide"
    );
    let _ = std::fs::remove_dir_all(&root);
}
