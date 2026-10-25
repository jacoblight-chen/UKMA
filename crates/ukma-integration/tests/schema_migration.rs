//! FM-006: canonical schema versioning and migration (§29.4).
//!
//! The same contract runs against two real adapters: the embedded JSON snapshot
//! and the SQLite database. A migration that only worked for one of them would
//! not be a storage-port contract, so every case is repeated.

use std::path::PathBuf;

use serde_json::json;
use storage_ports::{
    canonical_migrations, migrate, MetadataPort, Migration, MigrationReport, MigrationStore,
    PortResult, SchemaVersionPort, StorageError, CANONICAL_SCHEMA_VERSION,
};
use ukma_adapter_embedded::{default_scope, EmbeddedStore};
use ukma_adapter_sqlite::SqliteStore;
use ukma_contracts::ScopeRef;

struct Layout {
    root: PathBuf,
}

impl Layout {
    fn new(tag: &str) -> Self {
        let mut root = std::env::temp_dir();
        root.push(format!("ukma-migrate-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scope() -> ScopeRef {
    default_scope("tenant-a", "project-b")
}

fn other_scope() -> ScopeRef {
    default_scope("tenant-a", "project-c")
}

/// Writes three legacy (v1) records into `store` for `scope`.
fn seed(store: &dyn MetadataPort, scope: &ScopeRef) {
    for key in ["canon:alpha", "canon:beta", "canon:gamma"] {
        store
            .put_cas(scope, key, json!({"payload": key}), None)
            .expect("seed record");
    }
}

fn stamp_count(store: &dyn MetadataPort, scope: &ScopeRef) -> usize {
    let (page, _) = store.list(scope, "", 64, None).expect("list");
    page.iter()
        .filter(|entry| entry.value.get("schema_version").and_then(|v| v.as_u64()) == Some(2))
        .count()
}

fn run(store: &dyn MigrationStore, scope: &ScopeRef, target: u32) -> PortResult<MigrationReport> {
    let steps = canonical_migrations();
    migrate(store, scope, &steps, target)
}

#[test]
fn the_embedded_store_migrates_and_keeps_the_version_across_reopen() {
    let layout = Layout::new("embedded");
    let path = layout.file("store.json");
    let store = EmbeddedStore::open(&path).unwrap();
    seed(&store, &scope());
    // A store written by the v1 build reports v1.
    store.set_schema_version(1).unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);

    let report = run(&store, &scope(), CANONICAL_SCHEMA_VERSION).unwrap();
    assert_eq!(report.from, 1);
    assert_eq!(report.to, CANONICAL_SCHEMA_VERSION);
    assert!(!report.skipped);
    assert_eq!(
        report.applied,
        vec!["v2:stamp-schema-version"],
        "a v1 store only needs the v2 step"
    );
    assert_eq!(stamp_count(&store, &scope()), 3);
    store.flush().unwrap();
    drop(store);

    let reopened = EmbeddedStore::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 2);
    assert_eq!(stamp_count(&reopened, &scope()), 3);
}

#[test]
fn the_sqlite_store_migrates_through_the_same_contract() {
    let layout = Layout::new("sqlite");
    let path = layout.file("store.db");
    let store = SqliteStore::open(&path).unwrap();
    seed(&store, &scope());
    store.set_schema_version(1).unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);

    let report = run(&store, &scope(), CANONICAL_SCHEMA_VERSION).unwrap();
    assert_eq!(report.from, 1);
    assert_eq!(report.to, 2);
    assert_eq!(report.applied.len(), 1);
    assert_eq!(stamp_count(&store, &scope()), 3);
    drop(store);

    // The version lives in the database, not in process memory.
    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 2);
    assert_eq!(stamp_count(&reopened, &scope()), 3);
}

#[test]
fn re_running_after_migration_changes_nothing() {
    let layout = Layout::new("idempotent");
    let store = EmbeddedStore::open(layout.file("store.json")).unwrap();
    seed(&store, &scope());
    store.set_schema_version(1).unwrap();
    run(&store, &scope(), 2).unwrap();

    let second = run(&store, &scope(), 2).unwrap();
    assert!(second.skipped);
    assert!(second.applied.is_empty());
    assert_eq!(store.schema_version().unwrap(), 2);
}

#[test]
fn a_newer_store_is_refused_and_left_untouched() {
    let layout = Layout::new("newer");
    let store = EmbeddedStore::open(layout.file("store.json")).unwrap();
    seed(&store, &scope());
    store.set_schema_version(9).unwrap();

    let error = run(&store, &scope(), 2).unwrap_err();
    assert_eq!(error.code(), "INVALID_SCHEMA");
    assert!(matches!(
        error,
        StorageError::SchemaVersionUnsupported {
            found: 9,
            supported: 2
        }
    ));
    assert_eq!(store.schema_version().unwrap(), 9);
    assert_eq!(stamp_count(&store, &scope()), 0);
}

#[test]
fn migration_only_touches_the_requested_scope() {
    let layout = Layout::new("scope");
    let store = EmbeddedStore::open(layout.file("store.json")).unwrap();
    seed(&store, &scope());
    seed(&store, &other_scope());
    store.set_schema_version(1).unwrap();

    run(&store, &scope(), 2).unwrap();
    assert_eq!(stamp_count(&store, &scope()), 3);
    assert_eq!(
        stamp_count(&store, &other_scope()),
        0,
        "a foreign scope must not be rewritten"
    );
}

struct Refusing;

impl Migration for Refusing {
    fn version(&self) -> u32 {
        2
    }

    fn name(&self) -> &'static str {
        "refusing"
    }

    fn apply(&self, _store: &dyn MetadataPort, _scope: &ScopeRef) -> PortResult<()> {
        Err(StorageError::Internal("step refused".into()))
    }
}

#[test]
fn a_failing_step_does_not_advance_the_recorded_version() {
    let layout = Layout::new("failing");
    let store = EmbeddedStore::open(layout.file("store.json")).unwrap();
    seed(&store, &scope());
    store.set_schema_version(1).unwrap();

    let steps: Vec<&dyn Migration> = vec![&Refusing];
    let error = migrate(&store, &scope(), &steps, 2).unwrap_err();
    assert_eq!(error.code(), "INTERNAL");
    assert_eq!(store.schema_version().unwrap(), 1);
    assert_eq!(stamp_count(&store, &scope()), 0);
}

#[test]
fn a_missing_step_is_reported_instead_of_skipped() {
    let layout = Layout::new("gap");
    let store = EmbeddedStore::open(layout.file("store.json")).unwrap();
    store.set_schema_version(0).unwrap();

    let steps: Vec<&dyn Migration> = vec![&Refusing];
    let error = migrate(&store, &scope(), &steps, 2).unwrap_err();
    assert_eq!(error.code(), "INVALID_SCHEMA");
    assert!(matches!(
        error,
        StorageError::MigrationChainBroken { from: 0, next: 2 }
    ));
}
