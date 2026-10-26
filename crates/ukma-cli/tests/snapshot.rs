//! The snapshot-cache policy driven end to end over the real SQLite backend:
//! revision parity with the scanner, persistence across a reopen, incrementality
//! and status.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use storage_ports::snapshot::{
    scope_fingerprint, CacheError, FileRow, SnapshotMeta, SnapshotStore,
};
use ukma_adapter_sqlite::snapshot::SqliteSnapshotCache;
use ukma_cli::setup::snapshot::{status, status_with, sync, sync_snapshot, sync_with, PendingKind};
use ukma_mcp::index::{index_multi, IndexOptions};

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ukma-snapshot-it-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create tree");
        Self { root }
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(&path, content).expect("write");
    }

    fn db(&self) -> PathBuf {
        self.root.join(".ukma").join("index.db")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn scanned_revision(root: &Path, options: &IndexOptions) -> String {
    let roots = [root];
    index_multi(&roots, options).expect("scan").index.commit_sha
}

#[test]
fn the_cached_revision_matches_a_fresh_scan() {
    let tree = Tree::new("parity");
    tree.write("src/main.rs", "fn main() {}\n");
    tree.write("src/lib.rs", "pub fn helper() {}\n");
    tree.write("docs/guide.md", "# guide\n");
    tree.write("target/debug/junk.rs", "fn junk() {}\n");
    tree.write(".hidden.rs", "fn hidden() {}\n");

    for options in [
        IndexOptions::default(),
        IndexOptions {
            excludes: vec!["*.md".into()],
            max_files: None,
        },
        IndexOptions {
            excludes: vec!["docs/**".into()],
            max_files: Some(2),
        },
    ] {
        let store = SqliteSnapshotCache::open(tree.db()).expect("open");
        let report = sync(&tree.root, &options, &store).expect("sync");
        assert_eq!(
            report.revision,
            scanned_revision(&tree.root, &options),
            "the cached revision must equal the scanned revision for {options:?}"
        );
        assert!(report.full, "a fresh store starts with a full rescan");
        let _ = fs::remove_dir_all(tree.root.join(".ukma"));
    }
}

#[test]
fn a_snapshot_survives_reopening_the_database() {
    let tree = Tree::new("persist");
    tree.write("a.rs", "fn a() {}\n");
    tree.write("b.rs", "fn b() {}\n");
    let options = IndexOptions::default();

    let first = {
        let store = SqliteSnapshotCache::open(tree.db()).expect("open");
        let report = sync(&tree.root, &options, &store).expect("sync");
        assert_eq!(report.read, 2, "the first scan reads both files");
        report
    };

    // A brand-new handle stands in for a new process.
    let store = SqliteSnapshotCache::open(tree.db()).expect("reopen");
    let meta = store.meta().expect("meta").expect("a snapshot");
    assert_eq!(meta.revision, first.revision);
    assert_eq!(meta.files, 2);
    assert_eq!(store.rows().expect("rows").len(), 2);

    let second = sync(&tree.root, &options, &store).expect("second sync");
    assert_eq!(second.read, 0, "the persisted snapshot is reused");
    assert_eq!(second.revision, first.revision);
}

#[test]
fn only_the_edited_file_is_read_after_reopening() {
    let tree = Tree::new("incremental");
    tree.write("a.rs", "fn a() {}\n");
    tree.write("b.rs", "fn b() {}\n");
    tree.write("c.rs", "fn c() {}\n");
    let options = IndexOptions::default();
    {
        let store = SqliteSnapshotCache::open(tree.db()).expect("open");
        sync(&tree.root, &options, &store).expect("first sync");
    }

    tree.write("b.rs", "fn b() { b(); }\n");
    let store = SqliteSnapshotCache::open(tree.db()).expect("reopen");
    let report = sync(&tree.root, &options, &store).expect("second sync");
    assert_eq!(report.read, 1, "only b.rs is read");
    assert_eq!(report.modified, 1);
    assert_eq!(report.unchanged, 2);
    assert_eq!(report.revision, scanned_revision(&tree.root, &options));
}

#[test]
fn the_snapshot_rows_materialize_in_path_order() {
    let tree = Tree::new("rows");
    tree.write("b.rs", "fn b() {}\n");
    tree.write("a.rs", "fn a() {}\n");
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    let (report, rows) = sync_snapshot(&tree.root, &IndexOptions::default(), &store).expect("sync");

    assert_eq!(
        rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
        vec!["a.rs", "b.rs"]
    );
    assert_eq!(report.files, rows.len());
    assert_eq!(rows[0].content, "fn a() {}\n");
    assert_eq!(rows[0].sha256.len(), 64);
    assert_eq!(rows[0].size, "fn a() {}\n".len() as u64);
}

#[test]
fn a_same_size_edit_with_a_restored_timestamp_is_found_by_verify() {
    let tree = Tree::new("verify");
    tree.write("a.rs", "fn a() {}\n");
    let options = IndexOptions::default();
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    sync(&tree.root, &options, &store).expect("sync");
    let original = fs::metadata(tree.root.join("a.rs"))
        .expect("metadata")
        .modified()
        .expect("mtime");

    // Same byte length; then put the timestamp back exactly.
    tree.write("a.rs", "fn b() {}\n");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(tree.root.join("a.rs"))
        .expect("open");
    file.set_modified(original).expect("restore mtime");
    drop(file);
    assert_eq!(
        fs::metadata(tree.root.join("a.rs"))
            .expect("metadata")
            .len(),
        "fn b() {}\n".len() as u64,
        "the edit keeps the size"
    );

    let invisible = status(&tree.root, &options, Some(&store)).expect("status");
    assert!(
        invisible.is_current(),
        "the stat prefilter cannot see this edit: {:?}",
        invisible.pending
    );
    assert_eq!(invisible.verified, 0);

    let verified = status_with(&tree.root, &options, Some(&store), true).expect("verified status");
    assert_eq!(
        verified
            .pending
            .iter()
            .map(|change| (change.path.as_str(), change.kind))
            .collect::<Vec<_>>(),
        vec![("a.rs", PendingKind::Modified)]
    );
    assert_eq!(verified.verified, 1);

    let repaired = sync_with(&tree.root, &options, &store, true).expect("verified sync");
    assert_eq!(repaired.modified, 1);
    assert_eq!(repaired.verified, 1);
    assert_eq!(repaired.revision, scanned_revision(&tree.root, &options));
    assert!(status(&tree.root, &options, Some(&store))
        .expect("status")
        .is_current());
}

#[test]
fn status_lists_the_drift_and_writes_nothing() {
    let tree = Tree::new("status");
    tree.write("a.rs", "fn a() {}\n");
    tree.write("b.rs", "fn b() {}\n");
    let options = IndexOptions::default();
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    sync(&tree.root, &options, &store).expect("sync");
    let before = store.rows().expect("rows");

    tree.write("a.rs", "fn a() { a(); }\n");
    fs::remove_file(tree.root.join("b.rs")).expect("delete");
    tree.write("c.rs", "fn c() {}\n");

    let report = status(&tree.root, &options, Some(&store)).expect("status");
    assert!(!report.is_current());
    assert_eq!(report.on_disk, 2);
    assert_eq!(
        report
            .pending
            .iter()
            .map(|change| (change.path.as_str(), change.kind))
            .collect::<Vec<_>>(),
        vec![
            ("a.rs", PendingKind::Modified),
            ("b.rs", PendingKind::Deleted),
            ("c.rs", PendingKind::Added)
        ]
    );
    assert_eq!(
        store.rows().expect("rows"),
        before,
        "status must not write to the cache"
    );
}

#[test]
fn status_notices_a_changed_scan_scope() {
    let tree = Tree::new("scope");
    tree.write("a.rs", "fn a() {}\n");
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    let options = IndexOptions::default();
    sync(&tree.root, &options, &store).expect("sync");

    let narrowed = IndexOptions {
        excludes: vec!["*.rs".into()],
        max_files: None,
    };
    let report = status(&tree.root, &narrowed, Some(&store)).expect("status");
    assert!(
        report.stale_scope,
        "different options mean a different scope"
    );
    assert_ne!(
        scope_fingerprint(&tree.root, &options.excludes, options.max_files),
        scope_fingerprint(&tree.root, &narrowed.excludes, narrowed.max_files)
    );

    let rescanned = sync(&tree.root, &narrowed, &store).expect("sync");
    assert!(rescanned.full);
    assert_eq!(rescanned.files, 0);
    let report = status(&tree.root, &narrowed, Some(&store)).expect("status");
    assert!(report.is_current(), "a full rescan clears the stale scope");
}

#[test]
fn a_scope_change_discards_stale_rows() {
    let tree = Tree::new("discard");
    tree.write("keep.rs", "fn keep() {}\n");
    tree.write("drop.md", "# drop\n");
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    sync(&tree.root, &IndexOptions::default(), &store).expect("first sync");
    let narrowed = IndexOptions {
        excludes: vec!["*.md".into()],
        max_files: None,
    };
    sync(&tree.root, &narrowed, &store).expect("second sync");
    assert_eq!(
        store
            .rows()
            .expect("rows")
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        vec!["keep.rs"],
        "the excluded file is gone from the cache, not merely ignored"
    );
}

#[test]
fn a_reset_clears_the_sql_tables() {
    let tree = Tree::new("reset");
    tree.write("a.rs", "fn a() {}\n");
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    sync(&tree.root, &IndexOptions::default(), &store).expect("sync");
    assert_eq!(store.rows().expect("rows").len(), 1);

    store.reset().expect("reset");
    assert!(store.rows().expect("rows").is_empty());
    assert_eq!(store.meta().expect("meta"), None);

    let again = sync(&tree.root, &IndexOptions::default(), &store).expect("resync");
    assert!(again.full);
    assert_eq!(again.read, 1);
}

/// Compile-time proof that the port is object-safe and usable through a trait
/// object, which is how the CLI holds it.
#[test]
fn the_store_is_usable_as_a_trait_object() {
    let tree = Tree::new("object");
    tree.write("a.rs", "fn a() {}\n");
    let store = SqliteSnapshotCache::open(tree.db()).expect("open");
    let dynamic: &dyn SnapshotStore = &store;
    let report = sync(&tree.root, &IndexOptions::default(), dynamic).expect("sync");
    assert_eq!(report.files, 1);

    let rows: BTreeMap<String, FileRow> = dynamic
        .rows()
        .expect("rows")
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    assert_eq!(rows.len(), 1);
    let meta: Option<SnapshotMeta> = dynamic.meta().expect("meta");
    assert!(meta.is_some());
}

/// The cache is a cache: an unreadable file must surface as a typed error, not
/// as a silent empty snapshot.
#[test]
fn a_corrupt_cache_is_a_typed_error() {
    let tree = Tree::new("corrupt");
    tree.write("a.rs", "fn a() {}\n");
    fs::create_dir_all(tree.root.join(".ukma")).expect("mkdir");
    fs::write(tree.db(), b"not a database").expect("write");
    let error = SqliteSnapshotCache::open(tree.db()).expect_err("a corrupt cache is refused");
    assert!(matches!(error, CacheError::Db { .. }), "{error}");
}
