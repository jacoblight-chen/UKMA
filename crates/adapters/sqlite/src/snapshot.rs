//! Snapshot-cache adapter (profile B): the working-tree snapshot behind
//! `ukma init` / `ukma sync` lives in a real SQLite database, so it survives
//! process exit and is safe to open from a second process (WAL + file locks).
//!
//! This is the concrete backend for the [`storage_ports::snapshot::SnapshotStore`] port. The
//! policy (what to rescan, what "changed" means, how the revision is derived)
//! stays in `ukma-cache`; this module only stores and returns rows.
//!
//! Stated limits: the database is a **cache** ??deleting it loses no
//! information, only work ??and a file written by a newer schema version is
//! refused rather than silently reset.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection};
use storage_ports::snapshot::{
    CacheError, FileRow, ParseCache, ParseKey, SnapshotMeta, SnapshotStore, SNAPSHOT_FORMAT,
    SNAPSHOT_SCHEMA_VERSION,
};

const SNAPSHOT_DDL: &str = "
CREATE TABLE IF NOT EXISTS snapshot_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshot_files (
    path     TEXT PRIMARY KEY,
    size     INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    sha256   TEXT NOT NULL,
    content  TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS parse_cache (
    parser_version TEXT NOT NULL,
    language       TEXT NOT NULL,
    path           TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    payload        TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (parser_version, language, path, content_sha256)
) WITHOUT ROWID;
";

/// A [`SnapshotStore`] backed by one SQLite database file.
pub struct SqliteSnapshotCache {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl SqliteSnapshotCache {
    /// Open (creating if absent) the cache at `path`.
    ///
    /// A newer schema is a typed [`CacheError::Version`] rather than a silent
    /// reset, so a downgrade never destroys a newer cache.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, CacheError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| CacheError::Io {
                action: "create".into(),
                path: parent.to_path_buf(),
                detail: error.to_string(),
            })?;
        }
        let conn = Connection::open(&path).map_err(|error| db_error(&path, &error))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
            .map_err(|error| db_error(&path, &error))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|error| db_error(&path, &error))?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| db_error(&path, &error))?;
        if version > SNAPSHOT_SCHEMA_VERSION {
            return Err(CacheError::Version {
                path,
                detail: format!(
                    "schema_version {version} is newer than the supported {SNAPSHOT_SCHEMA_VERSION}; \
                     run `ukma sync --force` with a newer ukma, or delete the file"
                ),
            });
        }
        conn.execute_batch(SNAPSHOT_DDL)
            .map_err(|error| db_error(&path, &error))?;
        conn.pragma_update(None, "user_version", SNAPSHOT_SCHEMA_VERSION)
            .map_err(|error| db_error(&path, &error))?;
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, CacheError> {
        self.conn.lock().map_err(|_| {
            CacheError::Invalid(format!(
                "snapshot cache {}: connection lock poisoned",
                self.path.display()
            ))
        })
    }
}

impl std::fmt::Debug for SqliteSnapshotCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSnapshotCache")
            .field("path", &self.path)
            .finish()
    }
}

fn db_error(path: &Path, error: &rusqlite::Error) -> CacheError {
    CacheError::Db {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

impl SnapshotStore for SqliteSnapshotCache {
    fn path(&self) -> &Path {
        &self.path
    }

    fn meta(&self) -> Result<Option<SnapshotMeta>, CacheError> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare("SELECT key, value FROM snapshot_meta")
            .map_err(|error| db_error(&self.path, &error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| db_error(&self.path, &error))?;
        let mut map = std::collections::BTreeMap::new();
        for row in rows {
            let (key, value) = row.map_err(|error| db_error(&self.path, &error))?;
            map.insert(key, value);
        }
        let Some(format) = map.get("format") else {
            return Ok(None);
        };
        if format != SNAPSHOT_FORMAT {
            return Err(CacheError::Version {
                path: self.path.clone(),
                detail: format!("format `{format}` is not `{SNAPSHOT_FORMAT}`"),
            });
        }
        let schema_version = map
            .get("schema_version")
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| CacheError::Version {
                path: self.path.clone(),
                detail: "`schema_version` is missing or not an integer".into(),
            })?;
        if schema_version > SNAPSHOT_SCHEMA_VERSION {
            return Err(CacheError::Version {
                path: self.path.clone(),
                detail: format!(
                    "schema_version {schema_version} is newer than {SNAPSHOT_SCHEMA_VERSION}"
                ),
            });
        }
        let files = map
            .get("files")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        Ok(Some(SnapshotMeta {
            format: format.clone(),
            schema_version,
            revision: map.get("revision").cloned().unwrap_or_default(),
            files,
            scope: map.get("scope").cloned().unwrap_or_default(),
            updated_at: map.get("updated_at").cloned().unwrap_or_default(),
        }))
    }

    fn rows(&self) -> Result<Vec<FileRow>, CacheError> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT path, size, mtime_ns, sha256, content FROM snapshot_files ORDER BY path",
            )
            .map_err(|error| db_error(&self.path, &error))?;
        let rows = statement
            .query_map([], |row| {
                Ok(FileRow {
                    path: row.get(0)?,
                    size: row.get::<_, i64>(1)? as u64,
                    mtime_ns: row.get::<_, i64>(2)? as u64,
                    sha256: row.get(3)?,
                    content: row.get(4)?,
                })
            })
            .map_err(|error| db_error(&self.path, &error))?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row.map_err(|error| db_error(&self.path, &error))?);
        }
        Ok(files)
    }

    fn apply(&self, upserts: &[FileRow], removals: &[String]) -> Result<(), CacheError> {
        if upserts.is_empty() && removals.is_empty() {
            return Ok(());
        }
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction()
            .map_err(|error| db_error(&self.path, &error))?;
        {
            let mut upsert = transaction
                .prepare(
                    "INSERT INTO snapshot_files (path, size, mtime_ns, sha256, content) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(path) DO UPDATE SET \
                       size = excluded.size, mtime_ns = excluded.mtime_ns, \
                       sha256 = excluded.sha256, content = excluded.content",
                )
                .map_err(|error| db_error(&self.path, &error))?;
            for row in upserts {
                upsert
                    .execute(params![
                        row.path,
                        row.size as i64,
                        row.mtime_ns as i64,
                        row.sha256,
                        row.content
                    ])
                    .map_err(|error| db_error(&self.path, &error))?;
            }
            let mut remove = transaction
                .prepare("DELETE FROM snapshot_files WHERE path = ?1")
                .map_err(|error| db_error(&self.path, &error))?;
            for path in removals {
                remove
                    .execute(params![path])
                    .map_err(|error| db_error(&self.path, &error))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| db_error(&self.path, &error))
    }

    fn set_meta(&self, meta: &SnapshotMeta) -> Result<(), CacheError> {
        let mut conn = self.connection()?;
        let transaction = conn
            .transaction()
            .map_err(|error| db_error(&self.path, &error))?;
        {
            let mut upsert = transaction
                .prepare(
                    "INSERT INTO snapshot_meta (key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                )
                .map_err(|error| db_error(&self.path, &error))?;
            for (key, value) in [
                ("format", meta.format.clone()),
                ("schema_version", meta.schema_version.to_string()),
                ("revision", meta.revision.clone()),
                ("files", meta.files.to_string()),
                ("scope", meta.scope.clone()),
                ("updated_at", meta.updated_at.clone()),
            ] {
                upsert
                    .execute(params![key, value])
                    .map_err(|error| db_error(&self.path, &error))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| db_error(&self.path, &error))
    }

    fn reset(&self) -> Result<(), CacheError> {
        let conn = self.connection()?;
        conn.execute_batch("DELETE FROM snapshot_files; DELETE FROM snapshot_meta;")
            .map_err(|error| db_error(&self.path, &error))
    }
}

impl ParseCache for SqliteSnapshotCache {
    fn get(&self, key: &ParseKey) -> Result<Option<String>, CacheError> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT payload FROM parse_cache \
                 WHERE parser_version = ?1 AND language = ?2 AND path = ?3 AND content_sha256 = ?4",
            )
            .map_err(|error| db_error(&self.path, &error))?;
        let payload = statement
            .query_row(
                params![
                    key.parser_version,
                    key.language,
                    key.path,
                    key.content_sha256
                ],
                |row| row.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_error(&self.path, &other)),
            })?;
        Ok(payload)
    }

    fn put(&self, key: &ParseKey, payload: &str, created_at: &str) -> Result<(), CacheError> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO parse_cache \
               (parser_version, language, path, content_sha256, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(parser_version, language, path, content_sha256) DO UPDATE SET \
               payload = excluded.payload, created_at = excluded.created_at",
            params![
                key.parser_version,
                key.language,
                key.path,
                key.content_sha256,
                payload,
                created_at
            ],
        )
        .map_err(|error| db_error(&self.path, &error))?;
        Ok(())
    }

    fn len(&self) -> Result<usize, CacheError> {
        let conn = self.connection()?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM parse_cache", [], |row| row.get(0))
            .map_err(|error| db_error(&self.path, &error))?;
        Ok(count as usize)
    }

    fn clear(&self) -> Result<(), CacheError> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM parse_cache", [])
            .map_err(|error| db_error(&self.path, &error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ukma-snapshot-cache-{tag}-{}.db",
            std::process::id()
        ))
    }

    fn row(path: &str, content: &str) -> FileRow {
        FileRow {
            path: path.into(),
            size: content.len() as u64,
            mtime_ns: 7,
            sha256: storage_ports::snapshot::sha256_hex(content),
            content: content.into(),
        }
    }

    #[test]
    fn an_empty_store_has_no_meta_and_no_rows() {
        let path = temp_path("empty");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSnapshotCache::open(&path).expect("open");
        assert_eq!(store.meta().expect("meta"), None);
        assert!(store.rows().expect("rows").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rows_and_meta_round_trip() {
        let path = temp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSnapshotCache::open(&path).expect("open");
        store
            .apply(&[row("b.rs", "b"), row("a.rs", "a")], &[])
            .expect("apply");
        let rows = store.rows().expect("rows");
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["a.rs", "b.rs"],
            "rows come back ordered by path"
        );

        let meta = SnapshotMeta {
            format: SNAPSHOT_FORMAT.into(),
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            revision: "a".repeat(40),
            files: 2,
            scope: "scope".into(),
            updated_at: "2026-10-26T00:00:00Z".into(),
        };
        store.set_meta(&meta).expect("meta");
        assert_eq!(store.meta().expect("meta"), Some(meta));

        store.apply(&[], &["a.rs".to_string()]).expect("remove");
        assert_eq!(store.rows().expect("rows").len(), 1);
        store.reset().expect("reset");
        assert!(store.rows().expect("rows").is_empty());
        assert_eq!(store.meta().expect("meta"), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_reset() {
        let path = temp_path("version");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).expect("raw open");
            conn.pragma_update(None, "user_version", SNAPSHOT_SCHEMA_VERSION + 1)
                .expect("bump");
        }
        let error = SqliteSnapshotCache::open(&path).expect_err("a newer schema is refused");
        assert!(matches!(error, CacheError::Version { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_foreign_format_is_refused() {
        let path = temp_path("format");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSnapshotCache::open(&path).expect("open");
        store
            .set_meta(&SnapshotMeta {
                format: "other.tool".into(),
                schema_version: SNAPSHOT_SCHEMA_VERSION,
                revision: String::new(),
                files: 0,
                scope: String::new(),
                updated_at: String::new(),
            })
            .expect("meta");
        let error = store.meta().expect_err("a foreign format is refused");
        assert!(matches!(error, CacheError::Version { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_non_database_file_is_a_typed_error() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"this is not a database").expect("write");
        let error = SqliteSnapshotCache::open(&path).expect_err("a corrupt file is refused");
        assert!(matches!(error, CacheError::Db { .. }), "{error}");
        assert!(error.to_string().contains("snapshot cache"));
        let _ = std::fs::remove_file(&path);
    }

    fn key(path: &str, sha: &str, version: &str) -> ParseKey {
        ParseKey {
            parser_version: version.into(),
            language: "rust".into(),
            path: path.into(),
            content_sha256: sha.into(),
        }
    }

    #[test]
    fn a_parse_payload_round_trips_and_is_keyed_by_path_and_version() {
        let path = temp_path("parse");
        let _ = std::fs::remove_file(&path);
        let store = SqliteSnapshotCache::open(&path).expect("open");
        assert_eq!(store.len().expect("len"), 0);
        assert!(store
            .get(&key("a.rs", "sha-a", "1"))
            .expect("get")
            .is_none());

        store
            .put(&key("a.rs", "sha-a", "1"), "{\"symbols\":[]}", "now")
            .expect("put");
        assert_eq!(
            store.get(&key("a.rs", "sha-a", "1")).expect("get"),
            Some("{\"symbols\":[]}".to_string())
        );
        assert!(
            store
                .get(&key("b.rs", "sha-a", "1"))
                .expect("get")
                .is_none(),
            "the same content in another path is another parse"
        );
        assert!(
            store
                .get(&key("a.rs", "sha-a", "2"))
                .expect("get")
                .is_none(),
            "an older parser version does not answer"
        );

        // Overwriting the same key replaces the payload, not the row count.
        store
            .put(&key("a.rs", "sha-a", "1"), "{\"symbols\":[1]}", "later")
            .expect("put");
        assert_eq!(store.len().expect("len"), 1);
        assert_eq!(
            store.get(&key("a.rs", "sha-a", "1")).expect("get"),
            Some("{\"symbols\":[1]}".to_string())
        );

        store.clear().expect("clear");
        assert!(store.is_empty().expect("empty"));
        let _ = std::fs::remove_file(&path);
    }
}
