//! Incremental scan, sync and status for the project snapshot cache.
//!
//! The algorithm is the same one `git status` uses, applied to a code snapshot:
//! walk the tree **stat-only**, then read (and hash) only the files whose
//! `(size, mtime)` no longer matches the cached row. Unchanged files are never
//! opened, so a sync over a large unchanged tree does no file I/O at all.
//!
//! The revision is then computed with [`ukma_mcp::index::revision_of`] — the
//! same function the scanner uses — so a cached snapshot and a fresh scan of the
//! same tree agree on the id by construction.
//!
//! The persistence contract is [`storage_ports::snapshot::SnapshotStore`]; the
//! SQLite backend lives in `crates/adapters/sqlite`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use storage_ports::snapshot::{
    cache_io_error, scope_fingerprint_roots, sha256_hex, CacheError, FileRow, SnapshotMeta,
    SnapshotStore, SNAPSHOT_FORMAT, SNAPSHOT_SCHEMA_VERSION,
};
use ukma_mcp::index::{
    glob_match, is_skipped_dir, is_skipped_file, revision_of, IndexOptions, MAX_FILE_BYTES,
};

/// One file seen by the stat-only walk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiskFile {
    /// The path relative to the root that owns it.
    path: String,
    /// The absolute path (the row only keeps the relative one).
    absolute: PathBuf,
    size: u64,
    mtime_ns: u64,
}

/// What one [`sync`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// The revision of the merged snapshot.
    pub revision: String,
    /// The number of files in the snapshot.
    pub files: usize,
    /// Files that were not in the cache.
    pub added: usize,
    /// Files whose content changed.
    pub modified: usize,
    /// Files whose `(size, mtime)` moved but whose content did not.
    pub touched: usize,
    /// Files that are gone.
    pub removed: usize,
    /// Files whose cached row was reused without reading them.
    pub unchanged: usize,
    /// Files actually read from disk (the incrementality measure).
    pub read: usize,
    /// Files whose content was hashed even though `(size, mtime)` matched
    /// (`--verify`).
    pub verified: usize,
    /// Whether the whole tree was rescanned because the cache was missing or
    /// out of scope.
    pub full: bool,
}

impl SyncReport {
    /// Whether any file changed (and therefore whether rows were written).
    pub fn changed(&self) -> bool {
        self.added + self.modified + self.touched + self.removed > 0
    }
}

/// What kind of drift one path has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    /// On disk, not in the cache.
    Added,
    /// In the cache with different content.
    Modified,
    /// In the cache, gone from disk.
    Deleted,
}

impl PendingKind {
    /// The report label.
    pub fn label(&self) -> &'static str {
        match self {
            PendingKind::Added => "added",
            PendingKind::Modified => "modified",
            PendingKind::Deleted => "deleted",
        }
    }
}

/// One drifted path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChange {
    /// The repo-relative path.
    pub path: String,
    /// What happened to it.
    pub kind: PendingKind,
}

/// The read-only answer of [`status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    /// The scanned root.
    pub root: PathBuf,
    /// The cache file, when one was opened.
    pub store: Option<PathBuf>,
    /// The snapshot metadata the cache holds.
    pub meta: Option<SnapshotMeta>,
    /// The number of files in the working tree.
    pub on_disk: usize,
    /// The drifted paths, ordered by path.
    pub pending: Vec<PendingChange>,
    /// Files whose `(size, mtime)` moved but whose content is identical.
    pub touched: usize,
    /// Files whose content was hashed even though `(size, mtime)` matched
    /// (`--verify`).
    pub verified: usize,
    /// Whether the cached rows were computed with different scan options.
    pub stale_scope: bool,
}

impl StatusReport {
    /// Whether the cache matches the working tree exactly.
    pub fn is_current(&self) -> bool {
        self.meta.is_some() && self.pending.is_empty() && !self.stale_scope
    }
}

/// Sync the working tree into the cache and report what changed.
pub fn sync(
    root: &Path,
    options: &IndexOptions,
    store: &dyn SnapshotStore,
) -> Result<SyncReport, CacheError> {
    sync_with(root, options, store, false)
}

/// Sync, hashing every file whose `(size, mtime)` matched as well.
///
/// The stat prefilter is what makes a sync incremental, and it is also its only
/// blind spot: an edit that keeps the size and restores the timestamp looks
/// unchanged. `verify` reads the files anyway and compares content hashes.
pub fn sync_with(
    root: &Path,
    options: &IndexOptions,
    store: &dyn SnapshotStore,
    verify: bool,
) -> Result<SyncReport, CacheError> {
    sync_snapshot_with(root, options, store, verify).map(|(report, _)| report)
}

/// Sync and return the merged snapshot rows (used to build a gateway without
/// re-reading the tree).
pub fn sync_snapshot(
    root: &Path,
    options: &IndexOptions,
    store: &dyn SnapshotStore,
) -> Result<(SyncReport, Vec<FileRow>), CacheError> {
    sync_snapshot_with(root, options, store, false)
}

/// [`sync_snapshot`] with the optional content verification.
pub fn sync_snapshot_with(
    root: &Path,
    options: &IndexOptions,
    store: &dyn SnapshotStore,
    verify: bool,
) -> Result<(SyncReport, Vec<FileRow>), CacheError> {
    sync_snapshot_roots(&[root], options, store, verify)
}

/// Sync an ordered root list into the cache (a project can have several roots).
pub fn sync_roots(
    roots: &[&Path],
    options: &IndexOptions,
    store: &dyn SnapshotStore,
    verify: bool,
) -> Result<SyncReport, CacheError> {
    sync_snapshot_roots(roots, options, store, verify).map(|(report, _)| report)
}

/// [`sync_snapshot`] over an ordered root list.
pub fn sync_snapshot_roots(
    roots: &[&Path],
    options: &IndexOptions,
    store: &dyn SnapshotStore,
    verify: bool,
) -> Result<(SyncReport, Vec<FileRow>), CacheError> {
    let resolved: Vec<PathBuf> = roots
        .iter()
        .map(|root| {
            super::project::resolve_root(root)
                .map_err(|error| CacheError::Invalid(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    let resolved: Vec<&Path> = resolved.iter().map(PathBuf::as_path).collect();
    let on_disk = walk_roots(&resolved, options)?;
    let scope = fingerprint_roots(&resolved, options);

    let previous_meta = store.meta()?;
    let full = match &previous_meta {
        Some(meta) => meta.scope != scope,
        None => true,
    };
    if full {
        store.reset()?;
    }
    let mut cached: BTreeMap<String, FileRow> = if full {
        BTreeMap::new()
    } else {
        store
            .rows()?
            .into_iter()
            .map(|row| (row.path.clone(), row))
            .collect()
    };

    let mut rows: Vec<FileRow> = Vec::with_capacity(on_disk.len());
    let mut upserts: Vec<FileRow> = Vec::new();
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut touched = 0usize;
    let mut unchanged = 0usize;
    let mut read = 0usize;
    let mut verified = 0usize;

    for file in &on_disk {
        match cached.remove(&file.path) {
            Some(row) if row.size == file.size && row.mtime_ns == file.mtime_ns => {
                if !verify {
                    unchanged += 1;
                    rows.push(row);
                    continue;
                }
                // `--verify`: the stat says "same", so read the file and let the
                // content hash decide.
                let content = read_file(&file.absolute)?;
                read += 1;
                verified += 1;
                let sha256 = sha256_hex(&content);
                if sha256 == row.sha256 {
                    unchanged += 1;
                    rows.push(row);
                    continue;
                }
                modified += 1;
                let row = FileRow {
                    path: file.path.clone(),
                    size: file.size,
                    mtime_ns: file.mtime_ns,
                    sha256,
                    content,
                };
                rows.push(row.clone());
                upserts.push(row);
            }
            previous => {
                let content = read_file(&file.absolute)?;
                read += 1;
                let sha256 = sha256_hex(&content);
                match &previous {
                    Some(row) if row.sha256 == sha256 => touched += 1,
                    Some(_) => modified += 1,
                    None => added += 1,
                }
                let row = FileRow {
                    path: file.path.clone(),
                    size: file.size,
                    mtime_ns: file.mtime_ns,
                    sha256,
                    content,
                };
                rows.push(row.clone());
                upserts.push(row);
            }
        }
    }

    // Whatever is left in `cached` is gone from the tree.
    let removals: Vec<String> = cached.into_keys().collect();
    let removed = removals.len();

    let revision = revision_of(
        rows.iter()
            .map(|row| (row.path.as_str(), row.content.as_str())),
    );
    let files = rows.len();
    let report = SyncReport {
        revision: revision.clone(),
        files,
        added,
        modified,
        touched,
        removed,
        unchanged,
        read,
        verified,
        full,
    };

    if upserts.is_empty() && removals.is_empty() && !full {
        // Nothing changed and the scope is intact: do not touch the store
        // (not even the timestamp).
        if previous_meta.is_none() {
            store.set_meta(&meta_of(&revision, files, &scope))?;
        }
        return Ok((report, rows));
    }
    store.apply(&upserts, &removals)?;
    store.set_meta(&meta_of(&revision, files, &scope))?;
    Ok((report, rows))
}

/// Compare the working tree with the cache without writing anything.
pub fn status(
    root: &Path,
    options: &IndexOptions,
    store: Option<&dyn SnapshotStore>,
) -> Result<StatusReport, CacheError> {
    status_with(root, options, store, false)
}

/// [`status`] with the optional content verification.
///
/// With `verify`, every file is hashed even when `(size, mtime)` matches, so an
/// edit that keeps the size and restores the timestamp is still reported.
pub fn status_with(
    root: &Path,
    options: &IndexOptions,
    store: Option<&dyn SnapshotStore>,
    verify: bool,
) -> Result<StatusReport, CacheError> {
    status_roots(&[root], options, store, verify)
}

/// [`status`] over an ordered root list.
pub fn status_roots(
    roots: &[&Path],
    options: &IndexOptions,
    store: Option<&dyn SnapshotStore>,
    verify: bool,
) -> Result<StatusReport, CacheError> {
    let resolved: Vec<PathBuf> = roots
        .iter()
        .map(|root| {
            super::project::resolve_root(root)
                .map_err(|error| CacheError::Invalid(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    let resolved: Vec<&Path> = resolved.iter().map(PathBuf::as_path).collect();
    let root = resolved.first().copied().unwrap_or_else(|| Path::new("."));
    let on_disk = walk_roots(&resolved, options)?;
    let Some(store) = store else {
        return Ok(StatusReport {
            root: root.to_path_buf(),
            store: None,
            meta: None,
            on_disk: on_disk.len(),
            pending: Vec::new(),
            touched: 0,
            verified: 0,
            stale_scope: false,
        });
    };
    let Some(meta) = store.meta()? else {
        // An empty cache has no comparison basis: the tree is not "drifted",
        // it is simply not cached yet.
        return Ok(StatusReport {
            root: root.to_path_buf(),
            store: Some(store.path().to_path_buf()),
            meta: None,
            on_disk: on_disk.len(),
            pending: Vec::new(),
            touched: 0,
            verified: 0,
            stale_scope: false,
        });
    };
    let stale_scope = meta.scope != fingerprint_roots(&resolved, options);
    let cached: BTreeMap<String, FileRow> = store
        .rows()?
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();

    let mut pending = Vec::new();
    let mut touched = 0usize;
    let mut verified = 0usize;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for file in &on_disk {
        seen.insert(file.path.as_str());
        match cached.get(&file.path) {
            None => pending.push(PendingChange {
                path: file.path.clone(),
                kind: PendingKind::Added,
            }),
            Some(row) => {
                let stat_matches = row.size == file.size && row.mtime_ns == file.mtime_ns;
                if stat_matches && !verify {
                    continue;
                }
                // The stat moved (or `--verify` asked): hash the file so
                // "modified" means the content actually differs, not that a
                // formatter rewrote it verbatim.
                let content = read_file(&file.absolute)?;
                if verify {
                    verified += 1;
                }
                if sha256_hex(&content) == row.sha256 {
                    touched += 1;
                } else {
                    pending.push(PendingChange {
                        path: file.path.clone(),
                        kind: PendingKind::Modified,
                    });
                }
            }
        }
    }
    for path in cached.keys() {
        if !seen.contains(path.as_str()) {
            pending.push(PendingChange {
                path: path.clone(),
                kind: PendingKind::Deleted,
            });
        }
    }
    pending.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(StatusReport {
        root: root.to_path_buf(),
        store: Some(store.path().to_path_buf()),
        meta: Some(meta),
        on_disk: on_disk.len(),
        pending,
        touched,
        verified,
        stale_scope,
    })
}

fn fingerprint_roots(roots: &[&Path], options: &IndexOptions) -> String {
    scope_fingerprint_roots(roots, &options.excludes, options.max_files)
}

/// The snapshot statistics of a tree: `path -> (size, mtime_ns)`, from a
/// stat-only walk (no file is read).
///
/// This is what the staleness probe compares against, so it uses exactly the
/// same skip rules and limits as the scan that built the snapshot.
pub fn stat_map(
    root: &Path,
    options: &IndexOptions,
) -> Result<std::collections::BTreeMap<String, (u64, u64)>, CacheError> {
    let root = super::project::resolve_root(root)
        .map_err(|error| CacheError::Invalid(error.to_string()))?;
    Ok(walk(&root, options)?
        .into_iter()
        .map(|file| (file.path, (file.size, file.mtime_ns)))
        .collect())
}

/// [`stat_map`] over an ordered root list, keeping each file's absolute path
/// (the staleness probe needs it to stat the file it resolves).
pub fn stat_map_roots(
    roots: &[&Path],
    options: &IndexOptions,
) -> Result<std::collections::BTreeMap<String, (PathBuf, u64, u64)>, CacheError> {
    let resolved: Vec<PathBuf> = roots
        .iter()
        .map(|root| {
            super::project::resolve_root(root)
                .map_err(|error| CacheError::Invalid(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    let resolved: Vec<&Path> = resolved.iter().map(PathBuf::as_path).collect();
    Ok(walk_roots(&resolved, options)?
        .into_iter()
        .map(|file| (file.path, (file.absolute, file.size, file.mtime_ns)))
        .collect())
}

fn meta_of(revision: &str, files: usize, scope: &str) -> SnapshotMeta {
    SnapshotMeta {
        format: SNAPSHOT_FORMAT.to_string(),
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        revision: revision.to_string(),
        files,
        scope: scope.to_string(),
        updated_at: now_rfc3339(),
    }
}

/// The current UTC instant as RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    ukma_mcp::instant::format_rfc3339(seconds)
}

fn read_file(path: &Path) -> Result<String, CacheError> {
    fs::read_to_string(path).map_err(|error| cache_io_error("read", path, &error))
}

/// Walk the tree **without reading any file**, applying the scanner's own skip
/// rules, exclusions and file cap.
fn walk(root: &Path, options: &IndexOptions) -> Result<Vec<DiskFile>, CacheError> {
    walk_roots(&[root], options)
}

/// Walk every root and merge exactly like [`ukma_mcp::index::index_multi`]: the
/// cap applies per root, a relative path seen in an earlier root wins, and the
/// merged list is path-sorted. That is what keeps the cached revision equal to a
/// fresh multi-root scan.
fn walk_roots(roots: &[&Path], options: &IndexOptions) -> Result<Vec<DiskFile>, CacheError> {
    let mut merged: Vec<DiskFile> = Vec::new();
    for root in roots {
        let mut files = Vec::new();
        walk_into(root, root, options, &mut files)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        if let Some(cap) = options.max_files {
            files.truncate(cap);
        }
        for file in files {
            if !merged.iter().any(|existing| existing.path == file.path) {
                merged.push(file);
            }
        }
    }
    merged.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(merged)
}

fn walk_into(
    root: &Path,
    current: &Path,
    options: &IndexOptions,
    files: &mut Vec<DiskFile>,
) -> Result<(), CacheError> {
    let entries = fs::read_dir(current).map_err(|error| cache_io_error("read", current, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| cache_io_error("read", current, &error))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if is_skipped_dir(&name) {
                continue;
            }
            walk_into(root, &path, options, files)?;
        } else if path.is_file() {
            if is_skipped_file(&name) {
                continue;
            }
            let metadata =
                fs::metadata(&path).map_err(|error| cache_io_error("stat", &path, &error))?;
            if metadata.len() > MAX_FILE_BYTES {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| {
                    CacheError::Invalid(format!("{} is outside {}", path.display(), root.display()))
                })?
                .to_string_lossy()
                .replace('\\', "/");
            if options
                .excludes
                .iter()
                .any(|glob| glob_match(glob, &relative))
            {
                continue;
            }
            files.push(DiskFile {
                path: relative,
                absolute: path.clone(),
                size: metadata.len(),
                mtime_ns: mtime_ns(&metadata),
            });
        }
    }
    Ok(())
}

fn mtime_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A tiny in-memory store, to prove the algorithm through the port only.
    #[derive(Default)]
    struct MemoryStore {
        path: PathBuf,
        meta: Mutex<Option<SnapshotMeta>>,
        rows: Mutex<BTreeMap<String, FileRow>>,
    }

    impl SnapshotStore for MemoryStore {
        fn path(&self) -> &Path {
            &self.path
        }
        fn meta(&self) -> Result<Option<SnapshotMeta>, CacheError> {
            Ok(self
                .meta
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .clone())
        }
        fn rows(&self) -> Result<Vec<FileRow>, CacheError> {
            Ok(self
                .rows
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .values()
                .cloned()
                .collect())
        }
        fn apply(&self, upserts: &[FileRow], removals: &[String]) -> Result<(), CacheError> {
            let mut rows = self
                .rows
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?;
            for row in upserts {
                rows.insert(row.path.clone(), row.clone());
            }
            for path in removals {
                rows.remove(path);
            }
            Ok(())
        }
        fn set_meta(&self, meta: &SnapshotMeta) -> Result<(), CacheError> {
            *self
                .meta
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))? = Some(meta.clone());
            Ok(())
        }
        fn reset(&self) -> Result<(), CacheError> {
            self.rows
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))?
                .clear();
            *self
                .meta
                .lock()
                .map_err(|_| CacheError::Invalid("poisoned".into()))? = None;
            Ok(())
        }
    }

    fn tree(tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ukma-cli-snapshot-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create tree");
        path
    }

    #[test]
    fn a_second_sync_reads_nothing() {
        let root = tree("noop");
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        fs::write(root.join("b.rs"), "fn b() {}\n").expect("write");
        let store = MemoryStore::default();
        let options = IndexOptions::default();

        let first = sync(&root, &options, &store).expect("first sync");
        assert!(first.full, "the first sync is a full scan");
        assert_eq!(first.read, 2);
        assert_eq!(first.added, 2);
        assert_eq!(first.revision.len(), 40);

        let second = sync(&root, &options, &store).expect("second sync");
        assert!(!second.full);
        assert_eq!(second.read, 0, "unchanged files are never read");
        assert_eq!(second.unchanged, 2);
        assert!(!second.changed());
        assert_eq!(second.revision, first.revision);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn only_the_changed_file_is_read() {
        let root = tree("incremental");
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        fs::write(root.join("b.rs"), "fn b() {}\n").expect("write");
        let store = MemoryStore::default();
        let options = IndexOptions::default();
        sync(&root, &options, &store).expect("first sync");

        fs::write(root.join("b.rs"), "fn b() { b(); }\n").expect("edit");
        let report = sync(&root, &options, &store).expect("second sync");
        assert_eq!(report.read, 1, "only the edited file is read");
        assert_eq!(report.modified, 1);
        assert_eq!(report.unchanged, 1);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_rewrite_with_identical_content_only_refreshes_the_stat() {
        let root = tree("touched");
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let store = MemoryStore::default();
        let options = IndexOptions::default();
        let first = sync(&root, &options, &store).expect("first sync");

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("rewrite");
        let report = sync(&root, &options, &store).expect("second sync");
        assert_eq!(report.read, 1, "the stat moved, so the file is re-read");
        assert_eq!(report.touched, 1, "content is identical");
        assert_eq!(report.modified, 0);
        assert_eq!(report.revision, first.revision, "the revision did not move");

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn added_and_deleted_files_are_tracked() {
        let root = tree("addremove");
        fs::write(root.join("keep.rs"), "fn keep() {}\n").expect("write");
        fs::write(root.join("gone.rs"), "fn gone() {}\n").expect("write");
        let store = MemoryStore::default();
        let options = IndexOptions::default();
        sync(&root, &options, &store).expect("first sync");

        fs::remove_file(root.join("gone.rs")).expect("delete");
        fs::write(root.join("new.rs"), "fn new() {}\n").expect("write");
        let report = sync(&root, &options, &store).expect("second sync");
        assert_eq!(report.added, 1);
        assert_eq!(report.removed, 1);
        assert_eq!(report.files, 2);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_scope_change_forces_a_full_rescan() {
        let root = tree("scope");
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        fs::write(root.join("b.md"), "# b\n").expect("write");
        let store = MemoryStore::default();
        sync(&root, &IndexOptions::default(), &store).expect("first sync");

        let narrowed = IndexOptions {
            excludes: vec!["*.md".into()],
            max_files: None,
        };
        let report = sync(&root, &narrowed, &store).expect("second sync");
        assert!(report.full, "a different scope invalidates the cache");
        assert_eq!(report.files, 1);
        assert_eq!(
            report.removed, 0,
            "a full rescan discards the cache instead of diffing it"
        );

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn status_reports_drift_without_writing() {
        let root = tree("status");
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        fs::write(root.join("b.rs"), "fn b() {}\n").expect("write");
        let store = MemoryStore::default();
        let options = IndexOptions::default();

        let empty = status(&root, &options, Some(&store)).expect("status");
        assert_eq!(empty.meta, None);
        assert!(!empty.is_current());
        assert_eq!(empty.on_disk, 2);
        assert!(empty.pending.is_empty(), "no cache means no comparison");

        sync(&root, &options, &store).expect("sync");
        let clean = status(&root, &options, Some(&store)).expect("status");
        assert!(clean.is_current());
        assert!(clean.pending.is_empty());

        fs::write(root.join("a.rs"), "fn a() { a(); }\n").expect("edit");
        fs::remove_file(root.join("b.rs")).expect("delete");
        fs::write(root.join("c.rs"), "fn c() {}\n").expect("write");
        let drifted = status(&root, &options, Some(&store)).expect("status");
        assert!(!drifted.is_current());
        assert_eq!(
            drifted
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

        // status must not have changed the store
        assert_eq!(store.rows().expect("rows").len(), 2);

        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn status_without_a_store_still_counts_the_tree() {
        let root = tree("status-none");
        fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let report = status(&root, &IndexOptions::default(), None).expect("status");
        assert_eq!(report.store, None);
        assert_eq!(report.on_disk, 1);
        assert!(!report.is_current());
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn the_walk_applies_the_scanner_skip_rules() {
        let root = tree("skip");
        fs::create_dir_all(root.join("target")).expect("mkdir");
        fs::write(root.join("target/debug.rs"), "fn hidden() {}\n").expect("write");
        fs::write(root.join(".hidden.rs"), "fn hidden() {}\n").expect("write");
        fs::write(root.join("logo.bin"), [0u8, 1, 2]).expect("write");
        fs::write(root.join("keep.rs"), "fn keep() {}\n").expect("write");
        let walked = walk(&root, &IndexOptions::default()).expect("walk");
        assert_eq!(
            walked.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["keep.rs"]
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
