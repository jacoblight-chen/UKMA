//! The snapshot-cache port: what the CLI's incremental working-tree snapshot
//! needs from a backend, with no database type in sight.
//!
//! This is a **cache**, not a system of record: `.ukma/index.db` can be deleted
//! at any time and rebuilt from the tree. The port exists so the incremental
//! policy ([`crate::snapshot`] consumers, the `ukma` CLI) never sees SQL, and so
//! the concrete backend lives in the adapter that owns it
//! (`crates/adapters/sqlite`), exactly like the six canonical ports.
//!
//! The port is deliberately small — read the metadata, read the rows, apply a
//! diff, reset — so the whole implementation is a few dozen lines and the
//! policy is testable with an in-memory store.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The snapshot format tag.
pub const SNAPSHOT_FORMAT: &str = "ukma.snapshot";
/// The schema version this build writes and understands.
pub const SNAPSHOT_SCHEMA_VERSION: i64 = 2;

/// Everything that can go wrong in the snapshot cache.
#[derive(Debug)]
pub enum CacheError {
    /// A filesystem operation failed.
    Io {
        /// What was being attempted.
        action: String,
        /// The path involved.
        path: PathBuf,
        /// The underlying message.
        detail: String,
    },
    /// The database rejected an operation.
    Db {
        /// The database file.
        path: PathBuf,
        /// The underlying message.
        detail: String,
    },
    /// The cache was written by an incompatible version.
    Version {
        /// The database file.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },
    /// The store is unusable for another reason (a poisoned lock, …).
    Invalid(String),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Io {
                action,
                path,
                detail,
            } => write!(f, "cannot {action} {}: {detail}", path.display()),
            CacheError::Db { path, detail } => {
                write!(f, "snapshot cache {}: {detail}", path.display())
            }
            CacheError::Version { path, detail } => {
                write!(
                    f,
                    "snapshot cache {} is incompatible: {detail}",
                    path.display()
                )
            }
            CacheError::Invalid(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for CacheError {}

/// Build a [`CacheError::Io`].
pub fn cache_io_error(action: &str, path: &Path, error: &std::io::Error) -> CacheError {
    CacheError::Io {
        action: action.to_string(),
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

/// Build a [`CacheError::Db`] (for adapters that wrap a database error).
pub fn cache_db_error(path: &Path, detail: impl std::fmt::Display) -> CacheError {
    CacheError::Db {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    }
}

/// One persisted source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// Repo-relative path with `/` separators (the revision's key).
    pub path: String,
    /// The file size in bytes at the last sync.
    pub size: u64,
    /// The file modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: u64,
    /// The SHA-256 of the content (full 64 hex characters).
    pub sha256: String,
    /// The content itself.
    pub content: String,
}

/// The metadata of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMeta {
    /// Always [`SNAPSHOT_FORMAT`].
    pub format: String,
    /// The schema version ([`SNAPSHOT_SCHEMA_VERSION`] when written).
    pub schema_version: i64,
    /// The content-addressed revision id (40 hex characters).
    pub revision: String,
    /// The number of files in the snapshot.
    pub files: usize,
    /// The fingerprint of the scan scope (root + options); a mismatch means the
    /// cached rows are not comparable and the next sync must rescan everything.
    pub scope: String,
    /// When the snapshot was written (RFC 3339).
    pub updated_at: String,
}

/// The persistence port behind the incremental snapshot.
pub trait SnapshotStore: Send + Sync {
    /// The backing file (for diagnostics).
    fn path(&self) -> &Path;

    /// The snapshot metadata, when the store holds one.
    fn meta(&self) -> Result<Option<SnapshotMeta>, CacheError>;

    /// Every persisted row, ordered by path.
    fn rows(&self) -> Result<Vec<FileRow>, CacheError>;

    /// Apply one diff: upsert the rows, delete the paths, atomically.
    fn apply(&self, upserts: &[FileRow], removals: &[String]) -> Result<(), CacheError>;

    /// Replace the metadata.
    fn set_meta(&self, meta: &SnapshotMeta) -> Result<(), CacheError>;

    /// Drop everything (used when the scan scope changes).
    fn reset(&self) -> Result<(), CacheError>;
}

/// The SHA-256 of a text as lower-case hex (64 characters).
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The fingerprint of a scan scope: the root plus the options that decide which
/// files belong to a revision. When this changes, cached rows are discarded.
pub fn scope_fingerprint(root: &Path, excludes: &[String], max_files: Option<usize>) -> String {
    scope_fingerprint_roots(&[root], excludes, max_files)
}

/// [`scope_fingerprint`] over an ordered root list (a multi-root project is a
/// different scope than any single one of its roots).
pub fn scope_fingerprint_roots(
    roots: &[&Path],
    excludes: &[String],
    max_files: Option<usize>,
) -> String {
    let mut excludes = excludes.to_vec();
    excludes.sort();
    let max_files = max_files
        .map(|cap| cap.to_string())
        .unwrap_or_else(|| "*".to_string());
    let roots: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    sha256_hex(&format!(
        "v2|roots={}|max_files={max_files}|excludes={}",
        roots.join(";"),
        excludes.join(",")
    ))
}

/// The identity of one cached parse.
///
/// The **path** is part of the key on purpose: a parsed symbol carries its
/// declaration site, so the same text in two files is two different parses.
/// `parser_version` keeps results from an older extractor out of a newer build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseKey {
    /// The cache format version of the parser that produced the entry.
    pub parser_version: String,
    /// The language id.
    pub language: String,
    /// The repo-relative path.
    pub path: String,
    /// The SHA-256 of the source text (64 hex characters).
    pub content_sha256: String,
}

/// A cache of parse results, keyed by [`ParseKey`].
///
/// The payload is an opaque string (the parser's own serialized result): the
/// port knows nothing about the parser's types, so it stays database- and
/// AST-agnostic.
pub trait ParseCache: Send + Sync {
    /// The cached payload for `key`, when present.
    fn get(&self, key: &ParseKey) -> Result<Option<String>, CacheError>;

    /// Store one payload.
    fn put(&self, key: &ParseKey, payload: &str, created_at: &str) -> Result<(), CacheError>;

    /// The number of cached parses (diagnostics).
    fn len(&self) -> Result<usize, CacheError>;

    /// Whether the cache holds nothing.
    fn is_empty(&self) -> Result<bool, CacheError> {
        self.len().map(|len| len == 0)
    }

    /// Drop every cached parse (config/parser change).
    fn clear(&self) -> Result<(), CacheError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_fingerprint_tracks_options_but_not_their_order() {
        let root = Path::new("/work/project");
        let a = (vec!["a/**".to_string(), "b/**".to_string()], Some(10usize));
        let b = (vec!["b/**".to_string(), "a/**".to_string()], Some(10usize));
        assert_eq!(
            scope_fingerprint(root, &a.0, a.1),
            scope_fingerprint(root, &b.0, b.1),
            "exclude order is not part of the scope"
        );
        assert_ne!(
            scope_fingerprint(root, &a.0, a.1),
            scope_fingerprint(root, &["a/**".to_string()], a.1)
        );
        assert_ne!(
            scope_fingerprint(root, &a.0, a.1),
            scope_fingerprint(root, &a.0, None)
        );
        assert_ne!(
            scope_fingerprint(root, &a.0, a.1),
            scope_fingerprint(Path::new("/work/other"), &a.0, a.1),
            "the root is part of the scope"
        );
    }

    #[test]
    fn sha256_is_the_standard_digest() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex("").len(), 64);
    }
}
