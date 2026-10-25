use ukma_code_runtime::{
    EventOutbox, GraphStore, JsonFileAdapter, ReadContext, StoreError, SymbolRecord, WriteContext,
};
fn symbol() -> SymbolRecord {
    SymbolRecord {
        id: "symbol:one".into(),
        tenant_id: "tenant".into(),
        project_id: "project".into(),
        repo_id: "repo".into(),
        revision: "r1".into(),
        name: "call".into(),
        kind: "function".into(),
        etag: 0,
    }
}
fn write(key: &str, etag: Option<u64>) -> WriteContext {
    WriteContext {
        tenant_id: "tenant".into(),
        project_id: "project".into(),
        idempotency_key: key.into(),
        expected_etag: etag,
        can_index: true,
    }
}
fn fresh_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ukma-rust-contract-{}-{:?}.json",
        std::process::id(),
        std::thread::current().id()
    ))
}
#[test]
fn persistence_idempotency_conflict_and_outbox() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    assert_eq!(
        db.upsert_symbol(&write("k1", None), symbol()).unwrap().etag,
        1
    );
    assert_eq!(
        db.upsert_symbol(&write("k1", None), symbol()).unwrap().etag,
        1
    );
    assert_eq!(db.pending().unwrap().len(), 1);
    let mut altered = symbol();
    altered.name = "other".into();
    assert!(matches!(
        db.upsert_symbol(&write("k1", None), altered),
        Err(StoreError::Conflict)
    ));
    drop(db);
    let db = JsonFileAdapter::open(&p).unwrap();
    assert_eq!(
        db.get_symbol(
            &ReadContext {
                tenant_id: "tenant".into(),
                project_id: "project".into(),
                can_read: true
            },
            "symbol:one"
        )
        .unwrap()
        .etag,
        1
    );
    db.mark_delivered(1).unwrap();
    assert!(db.pending().unwrap().is_empty());
    std::fs::remove_file(p).unwrap();
}
#[test]
fn etag_and_tenant_isolation() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    db.upsert_symbol(&write("initial", None), symbol()).unwrap();
    assert!(matches!(
        db.upsert_symbol(&write("update", None), symbol()),
        Err(StoreError::Conflict)
    ));
    let mut r = symbol();
    r.revision = "r2".into();
    assert_eq!(
        db.upsert_symbol(&write("update", Some(1)), r).unwrap().etag,
        2
    );
    assert!(matches!(
        db.get_symbol(
            &ReadContext {
                tenant_id: "different".into(),
                project_id: "project".into(),
                can_read: true
            },
            "symbol:one"
        ),
        Err(StoreError::NotFound)
    ));
    let mut denied = write("bad", Some(2));
    denied.can_index = false;
    assert!(matches!(
        db.upsert_symbol(&denied, symbol()),
        Err(StoreError::PermissionDenied)
    ));
    std::fs::remove_file(p).unwrap();
}
#[test]
fn mark_delivered_unknown_event_id_is_not_found() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    assert!(matches!(db.mark_delivered(99), Err(StoreError::NotFound)));
    let _ = std::fs::remove_file(&p);
}
#[test]
fn corrupt_state_file_is_rejected_on_open() {
    let p = fresh_path();
    std::fs::write(&p, b"{ this is not json").unwrap();
    assert!(matches!(
        JsonFileAdapter::open(&p),
        Err(StoreError::Serialization(_))
    ));
    std::fs::remove_file(&p).unwrap();
}
#[test]
fn missing_required_identity_fields_are_invalid() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    assert!(matches!(
        db.upsert_symbol(&write("", None), symbol()),
        Err(StoreError::Invalid(_))
    ));
    let mut no_id = symbol();
    no_id.id = String::new();
    assert!(matches!(
        db.upsert_symbol(&write("k", None), no_id),
        Err(StoreError::Invalid(_))
    ));
    let _ = std::fs::remove_file(&p);
}
#[test]
fn cross_tenant_write_on_existing_symbol_is_denied() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    db.upsert_symbol(&write("initial", None), symbol()).unwrap();
    let mut intruder = write("intruder-write", None);
    intruder.tenant_id = "intruder".into();
    assert!(matches!(
        db.upsert_symbol(&intruder, symbol()),
        Err(StoreError::PermissionDenied)
    ));
    std::fs::remove_file(&p).unwrap();
}
#[test]
fn expected_etag_on_missing_symbol_is_conflict() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    assert!(matches!(
        db.upsert_symbol(&write("new", Some(5)), symbol()),
        Err(StoreError::Conflict)
    ));
    let _ = std::fs::remove_file(&p);
}
#[test]
fn idempotency_key_rebound_to_different_aggregate_is_conflict() {
    let p = fresh_path();
    let _ = std::fs::remove_file(&p);
    let db = JsonFileAdapter::open(&p).unwrap();
    db.upsert_symbol(&write("k", None), symbol()).unwrap();
    let mut other = symbol();
    other.id = "symbol:two".into();
    assert!(matches!(
        db.upsert_symbol(&write("k", None), other),
        Err(StoreError::Conflict)
    ));
    std::fs::remove_file(&p).unwrap();
}
