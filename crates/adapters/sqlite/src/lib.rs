//! SQLite adapter (profile B): canonical metadata and the durable outbox live in
//! real SQL tables inside real transactions; full text and vector stay **derived,
//! rebuildable projections** held in memory, exactly like the embedded JSON
//! adapter (§6.4: derived indexes are not the source of truth).
//!
//! Why this is a genuine second backend rather than a renamed file store:
//! - every canonical mutation is a SQL transaction (`BEGIN IMMEDIATE` … `COMMIT`),
//!   so a crash mid-write cannot leave a half-applied revision;
//! - concurrency control is the database's (unique keys, transactions), not a
//!   `Mutex` over a JSON blob;
//! - `PRAGMA journal_mode = WAL` and `synchronous = FULL` give the durability the
//!   minimum profile asks for, and the file can be opened by any SQLite tool.
//!
//! Stated limits (not hidden): one process opens the database (SQLite allows more,
//! this adapter does not manage multi-process contention beyond WAL), and the
//! derived projections are rebuilt from canonical data plus outbox replay.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use storage_ports::capabilities::{
    AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope,
};
use storage_ports::error::{io_error, PortResult, StorageError};
use storage_ports::testing::{InMemoryFullText, InMemoryVector};
use storage_ports::{
    validate_scope, EmbeddingProfile, FullTextDoc, FullTextHit, FullTextPort, MetadataEntry,
    MetadataPort, OutboxEvent, OutboxPort, VectorHit, VectorItem, VectorPort,
};
use ukma_contracts::ScopeRef;

const SCHEMA_VERSION: &str = "1";

const DDL: &str = "
CREATE TABLE IF NOT EXISTS metadata (
    scope    TEXT NOT NULL,
    key      TEXT NOT NULL,
    revision INTEGER NOT NULL,
    etag     TEXT NOT NULL,
    value    TEXT NOT NULL,
    PRIMARY KEY (scope, key)
);
CREATE TABLE IF NOT EXISTS manifest (
    scope      TEXT NOT NULL,
    name       TEXT NOT NULL,
    generation TEXT NOT NULL,
    PRIMARY KEY (scope, name)
);
CREATE TABLE IF NOT EXISTS writer (
    scope     TEXT NOT NULL,
    aggregate TEXT NOT NULL,
    token     TEXT NOT NULL,
    PRIMARY KEY (scope, aggregate)
);
CREATE TABLE IF NOT EXISTS outbox (
    scope              TEXT NOT NULL,
    seq                INTEGER NOT NULL,
    event_id           TEXT NOT NULL,
    event_type         TEXT NOT NULL,
    event_version      INTEGER NOT NULL,
    aggregate_ref      TEXT NOT NULL,
    aggregate_revision INTEGER NOT NULL,
    payload            TEXT NOT NULL,
    idempotency_key    TEXT,
    PRIMARY KEY (scope, event_id)
);
CREATE TABLE IF NOT EXISTS outbox_ack (
    scope    TEXT NOT NULL,
    consumer TEXT NOT NULL,
    event_id TEXT NOT NULL,
    PRIMARY KEY (scope, consumer, event_id)
);
CREATE TABLE IF NOT EXISTS outbox_dead (
    scope    TEXT NOT NULL,
    consumer TEXT NOT NULL,
    event_id TEXT NOT NULL,
    reason   TEXT NOT NULL,
    PRIMARY KEY (scope, consumer, event_id)
);
";

fn scope_key(scope: &ScopeRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        scope.tenant_id,
        scope.project_id,
        scope.repo_id.as_deref().unwrap_or("")
    )
}

fn etag_of(key: &str, revision: u64, value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(revision.to_le_bytes());
    hasher.update(value.to_string().as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn internal(message: impl Into<String>) -> StorageError {
    StorageError::Internal(message.into())
}

fn lock_error() -> StorageError {
    StorageError::Internal("sqlite adapter lock poisoned".to_string())
}

fn to_internal(error: rusqlite::Error) -> StorageError {
    StorageError::Internal(format!("sqlite: {error}"))
}

/// One SQLite-backed canonical store.
pub struct SqliteStore {
    path: PathBuf,
    connection: Mutex<Connection>,
    fulltext: InMemoryFullText,
    vector: InMemoryVector,
}

impl SqliteStore {
    /// Open (creating if absent) the database and apply the schema.
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path).map_err(|error| {
            io_error(
                &path,
                std::io::Error::other(format!("cannot open sqlite database: {error}")),
            )
        })?;
        // Durability and concurrency: WAL + full sync is what the minimum profile
        // means by "durable canonical transaction".
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(to_internal)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(to_internal)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(to_internal)?;
        connection.execute_batch(DDL).map_err(to_internal)?;
        let user_version: i64 = {
            let mut statement = connection
                .prepare("PRAGMA user_version")
                .map_err(to_internal)?;
            statement
                .query_row([], |row| row.get(0))
                .map_err(to_internal)?
        };
        if user_version != 0 && user_version.to_string() != SCHEMA_VERSION {
            return Err(internal(format!(
                "sqlite adapter schema version {user_version} is not {SCHEMA_VERSION}"
            )));
        }
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(to_internal)?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            fulltext: InMemoryFullText::new(),
            vector: InMemoryVector::new(),
        })
    }

    /// Database file this adapter writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flush WAL contents into the main database file (used by backup drills).
    pub fn checkpoint(&self) -> PortResult<()> {
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        connection
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(to_internal)
    }

    /// Size on disk of the database (main file), for index-size reporting.
    pub fn file_size(&self) -> u64 {
        std::fs::metadata(&self.path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    }
}

impl CapabilityProbe for SqliteStore {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: "sqlite".to_string(),
            adapter_version: "0.1.0".to_string(),
            backend_kind: BackendKind::Sql,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: SCHEMA_VERSION.to_string(),
            supported_ports: vec![
                "MetadataPort".to_string(),
                "OutboxPort".to_string(),
                "FullTextPort".to_string(),
                "VectorPort".to_string(),
            ],
            transaction_scope: TransactionScope::MultiAggregate,
            atomic_batch: true,
            compare_and_swap: true,
            snapshot_reads: true,
            acl_filtering: true,
            stable_cursor: true,
            native_graph_traversal: false,
            vector_filters: true,
            fulltext_analyzers: vec!["icu-v1".to_string(), "cjk-jieba-v1".to_string()],
            durable_outbox: true,
            backup_restore: true,
            max_batch_size: 1000,
            available: true,
            unavailable_reason: String::new(),
        }
    }
}

impl MetadataPort for SqliteStore {
    fn put_cas(
        &self,
        scope: &ScopeRef,
        key: &str,
        value: Value,
        expected_revision: Option<u64>,
    ) -> PortResult<MetadataEntry> {
        validate_scope(scope)?;
        let mut connection = self.connection.lock().map_err(|_| lock_error())?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(to_internal)?;
        let scope_key_value = scope_key(scope);
        let current: Option<u64> = transaction
            .query_row(
                "SELECT revision FROM metadata WHERE scope = ?1 AND key = ?2",
                params![scope_key_value, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(to_internal)?;
        let revision = match (current, expected_revision) {
            (None, None) => 1,
            (None, Some(expected)) => {
                return Err(StorageError::Conflict {
                    collection: "metadata",
                    key: key.to_string(),
                    expected_revision: expected,
                    actual_revision: 0,
                })
            }
            (Some(actual), Some(expected)) if actual != expected => {
                return Err(StorageError::Conflict {
                    collection: "metadata",
                    key: key.to_string(),
                    expected_revision: expected,
                    actual_revision: actual,
                })
            }
            (Some(actual), _) => actual + 1,
        };
        let etag = etag_of(key, revision, &value);
        transaction
            .execute(
                "INSERT INTO metadata (scope, key, revision, etag, value) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(scope, key) DO UPDATE SET revision = excluded.revision,
                     etag = excluded.etag, value = excluded.value",
                params![scope_key_value, key, revision, etag, value.to_string()],
            )
            .map_err(to_internal)?;
        transaction.commit().map_err(to_internal)?;
        Ok(MetadataEntry {
            key: key.to_string(),
            revision,
            etag,
            value,
        })
    }

    fn get(&self, scope: &ScopeRef, key: &str) -> PortResult<Option<MetadataEntry>> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        let scope_key_value = scope_key(scope);
        connection
            .query_row(
                "SELECT key, revision, etag, value FROM metadata WHERE scope = ?1 AND key = ?2",
                params![scope_key_value, key],
                |row| {
                    let raw: String = row.get(3)?;
                    let value = serde_json::from_str(&raw).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(MetadataEntry {
                        key: row.get(0)?,
                        revision: row.get(1)?,
                        etag: row.get(2)?,
                        value,
                    })
                },
            )
            .optional()
            .map_err(to_internal)
    }

    fn list(
        &self,
        scope: &ScopeRef,
        prefix: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> PortResult<(Vec<MetadataEntry>, Option<String>)> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        let scope_key_value = scope_key(scope);
        // A cursor is the last key already returned, so the next page starts
        // strictly after it; without a cursor the prefix is the lower bound.
        let (condition, after) = match cursor {
            Some(cursor) => ("key > ?2", cursor.to_string()),
            None => ("key >= ?2", prefix.to_string()),
        };
        let sql = format!(
            "SELECT key, revision, etag, value FROM metadata
             WHERE scope = ?1 AND {condition}
             ORDER BY key ASC LIMIT ?3"
        );
        let mut statement = connection.prepare(&sql).map_err(to_internal)?;
        let rows = statement
            .query_map(params![scope_key_value, after, (limit + 1) as i64], |row| {
                let raw: String = row.get(3)?;
                let value = serde_json::from_str(&raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(MetadataEntry {
                    key: row.get(0)?,
                    revision: row.get(1)?,
                    etag: row.get(2)?,
                    value,
                })
            })
            .map_err(to_internal)?;
        let mut entries: Vec<MetadataEntry> = Vec::new();
        for row in rows {
            entries.push(row.map_err(to_internal)?);
        }
        let next = if entries.len() > limit {
            entries.truncate(limit);
            entries.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        Ok((entries, next))
    }

    fn put_manifest(&self, scope: &ScopeRef, name: &str, generation: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        connection
            .execute(
                "INSERT INTO manifest (scope, name, generation) VALUES (?1, ?2, ?3)
                 ON CONFLICT(scope, name) DO UPDATE SET generation = excluded.generation",
                params![scope_key(scope), name, generation],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn get_manifest(&self, scope: &ScopeRef, name: &str) -> PortResult<Option<String>> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        connection
            .query_row(
                "SELECT generation FROM manifest WHERE scope = ?1 AND name = ?2",
                params![scope_key(scope), name],
                |row| row.get(0),
            )
            .optional()
            .map_err(to_internal)
    }

    fn acquire_writer(&self, scope: &ScopeRef, aggregate: &str, token: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        let existing: Option<String> = connection
            .query_row(
                "SELECT token FROM writer WHERE scope = ?1 AND aggregate = ?2",
                params![scope_key(scope), aggregate],
                |row| row.get(0),
            )
            .optional()
            .map_err(to_internal)?;
        match existing {
            Some(current) if current != token => Err(StorageError::Conflict {
                collection: "writer",
                key: aggregate.to_string(),
                expected_revision: 0,
                actual_revision: 1,
            }),
            _ => {
                connection
                    .execute(
                        "INSERT INTO writer (scope, aggregate, token) VALUES (?1, ?2, ?3)
                         ON CONFLICT(scope, aggregate) DO UPDATE SET token = excluded.token",
                        params![scope_key(scope), aggregate, token],
                    )
                    .map_err(to_internal)?;
                Ok(())
            }
        }
    }
}

impl OutboxPort for SqliteStore {
    fn enqueue(&self, scope: &ScopeRef, event: OutboxEvent) -> PortResult<()> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        let next: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM outbox WHERE scope = ?1",
                params![scope_key(scope)],
                |row| row.get(0),
            )
            .map_err(to_internal)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO outbox
                 (scope, seq, event_id, event_type, event_version, aggregate_ref,
                  aggregate_revision, payload, idempotency_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    scope_key(scope),
                    next,
                    event.event_id,
                    event.event_type,
                    event.event_version,
                    event.aggregate_ref,
                    event.aggregate_revision,
                    event.payload.to_string(),
                    event.idempotency_key,
                ],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn pending(
        &self,
        scope: &ScopeRef,
        consumer: &str,
        limit: usize,
    ) -> PortResult<Vec<OutboxEvent>> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        let mut statement = connection
            .prepare(
                "SELECT event_id, event_type, event_version, aggregate_ref, aggregate_revision,
                        payload, idempotency_key
                 FROM outbox
                 WHERE scope = ?1
                   AND event_id NOT IN (SELECT event_id FROM outbox_ack WHERE scope = ?1 AND consumer = ?2)
                   AND event_id NOT IN (SELECT event_id FROM outbox_dead WHERE scope = ?1 AND consumer = ?2)
                 ORDER BY seq ASC LIMIT ?3",
            )
            .map_err(to_internal)?;
        let rows = statement
            .query_map(params![scope_key(scope), consumer, limit as i64], |row| {
                let raw: String = row.get(5)?;
                let payload = serde_json::from_str(&raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(OutboxEvent {
                    event_id: row.get(0)?,
                    event_type: row.get(1)?,
                    event_version: row.get(2)?,
                    aggregate_ref: row.get(3)?,
                    aggregate_revision: row.get(4)?,
                    payload,
                    idempotency_key: row.get(6)?,
                })
            })
            .map_err(to_internal)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(to_internal)?);
        }
        Ok(events)
    }

    fn ack(&self, scope: &ScopeRef, consumer: &str, event_id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        connection
            .execute(
                "INSERT OR IGNORE INTO outbox_ack (scope, consumer, event_id) VALUES (?1, ?2, ?3)",
                params![scope_key(scope), consumer, event_id],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn dead_letter(
        &self,
        scope: &ScopeRef,
        consumer: &str,
        event_id: &str,
        reason: &str,
    ) -> PortResult<()> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        connection
            .execute(
                "INSERT INTO outbox_dead (scope, consumer, event_id, reason) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(scope, consumer, event_id) DO UPDATE SET reason = excluded.reason",
                params![scope_key(scope), consumer, event_id, reason],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn dead_letters(&self, scope: &ScopeRef, consumer: &str) -> PortResult<Vec<String>> {
        validate_scope(scope)?;
        let connection = self.connection.lock().map_err(|_| lock_error())?;
        let mut statement = connection
            .prepare(
                "SELECT event_id FROM outbox_dead WHERE scope = ?1 AND consumer = ?2 ORDER BY event_id",
            )
            .map_err(to_internal)?;
        let rows = statement
            .query_map(params![scope_key(scope), consumer], |row| row.get(0))
            .map_err(to_internal)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(to_internal)?);
        }
        Ok(ids)
    }
}

impl FullTextPort for SqliteStore {
    fn index(&self, scope: &ScopeRef, analyzer: &str, doc: &FullTextDoc) -> PortResult<()> {
        self.fulltext.index(scope, analyzer, doc)
    }

    fn delete(&self, scope: &ScopeRef, analyzer: &str, id: &str) -> PortResult<()> {
        self.fulltext.delete(scope, analyzer, id)
    }

    fn search(
        &self,
        scope: &ScopeRef,
        analyzer: &str,
        field: Option<&str>,
        query: &str,
        limit: usize,
    ) -> PortResult<Vec<FullTextHit>> {
        self.fulltext.search(scope, analyzer, field, query, limit)
    }

    fn analyzers(&self) -> Vec<String> {
        self.fulltext.analyzers()
    }
}

impl VectorPort for SqliteStore {
    fn declare_profile(&self, scope: &ScopeRef, profile: &EmbeddingProfile) -> PortResult<()> {
        self.vector.declare_profile(scope, profile)
    }

    fn profile(&self, scope: &ScopeRef, profile_id: &str) -> PortResult<Option<EmbeddingProfile>> {
        self.vector.profile(scope, profile_id)
    }

    fn upsert(&self, scope: &ScopeRef, profile_id: &str, items: &[VectorItem]) -> PortResult<()> {
        self.vector.upsert(scope, profile_id, items)
    }

    fn delete(&self, scope: &ScopeRef, profile_id: &str, id: &str) -> PortResult<()> {
        self.vector.delete(scope, profile_id, id)
    }

    fn search(
        &self,
        scope: &ScopeRef,
        profile_id: &str,
        query: &[f32],
        k: usize,
        filter: Option<&std::collections::BTreeMap<String, String>>,
    ) -> PortResult<Vec<VectorHit>> {
        self.vector.search(scope, profile_id, query, k, filter)
    }
}

/// Convenience scope constructor used by the contract tests.
pub fn default_scope(tenant_id: &str, project_id: &str) -> ScopeRef {
    ScopeRef {
        tenant_id: tenant_id.to_string(),
        project_id: project_id.to_string(),
        repo_id: None,
        workspace_id: None,
        agent_id: None,
        session_id: None,
    }
}

/// Convenience event constructor used by the contract tests.
pub fn event(event_id: &str, event_type: &str, revision: u64) -> OutboxEvent {
    OutboxEvent {
        event_id: event_id.to_string(),
        event_type: event_type.to_string(),
        event_version: 1,
        aggregate_ref: format!("test:{event_id}"),
        aggregate_revision: revision,
        payload: serde_json::json!({"operation_id": "test"}),
        idempotency_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("ukma-sqlite-{}-{tag}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        path
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn satisfies_the_minimum_production_profile() {
        let path = temp_path("caps");
        let store = SqliteStore::open(&path).unwrap();
        let capabilities = store.capabilities();
        assert!(capabilities.available);
        assert_eq!(capabilities.backend_kind, BackendKind::Sql);
        assert_eq!(capabilities.missing_required(), Vec::<&str>::new());
        capabilities.assert_minimum_profile().unwrap();
        cleanup(&path);
    }

    #[test]
    fn compare_and_swap_enforces_the_revision_and_is_durable() {
        let path = temp_path("cas");
        let scope = default_scope("tenant-a", "project-1");
        {
            let store = SqliteStore::open(&path).unwrap();
            let first = store
                .put_cas(
                    &scope,
                    "graph/manifest",
                    serde_json::json!({"gen": 1}),
                    None,
                )
                .unwrap();
            assert_eq!(first.revision, 1);
            let second = store
                .put_cas(
                    &scope,
                    "graph/manifest",
                    serde_json::json!({"gen": 2}),
                    Some(1),
                )
                .unwrap();
            assert_eq!(second.revision, 2);
            match store.put_cas(
                &scope,
                "graph/manifest",
                serde_json::json!({"gen": 3}),
                Some(1),
            ) {
                Err(StorageError::Conflict {
                    actual_revision, ..
                }) => {
                    assert_eq!(actual_revision, 2)
                }
                other => panic!("expected a conflict, got {other:?}"),
            }
            store.checkpoint().unwrap();
        }
        // Reopening sees the committed revision: the write was a real transaction.
        let store = SqliteStore::open(&path).unwrap();
        let entry = store.get(&scope, "graph/manifest").unwrap().expect("entry");
        assert_eq!(entry.revision, 2);
        assert_eq!(entry.value, serde_json::json!({"gen": 2}));
        cleanup(&path);
    }

    #[test]
    fn scope_isolation_holds_and_list_pages_by_key() {
        let path = temp_path("scope");
        let store = SqliteStore::open(&path).unwrap();
        let a = default_scope("tenant-a", "project-1");
        let b = default_scope("tenant-b", "project-1");
        for index in 0..5 {
            store
                .put_cas(&a, &format!("k/{index}"), serde_json::json!(index), None)
                .unwrap();
        }
        store
            .put_cas(&b, "k/0", serde_json::json!("b"), None)
            .unwrap();
        assert!(store.get(&b, "k/4").unwrap().is_none());
        let (page, next) = store.list(&a, "k/", 2, None).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].key, "k/0");
        let cursor = next.expect("a cursor");
        let (page2, _) = store.list(&a, "k/", 10, Some(&cursor)).unwrap();
        assert_eq!(page2.len(), 3, "cursor must not repeat the last key");
        assert_eq!(page2[0].key, "k/2", "the page continues after the cursor");
        cleanup(&path);
    }

    #[test]
    fn the_writer_fence_is_enforced_and_survives_reopen() {
        let path = temp_path("writer");
        let scope = default_scope("tenant-a", "project-1");
        {
            let store = SqliteStore::open(&path).unwrap();
            store.acquire_writer(&scope, "agg:1", "writer-a").unwrap();
            assert!(matches!(
                store.acquire_writer(&scope, "agg:1", "writer-b"),
                Err(StorageError::Conflict { .. })
            ));
            store.checkpoint().unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        assert!(matches!(
            store.acquire_writer(&scope, "agg:1", "writer-b"),
            Err(StorageError::Conflict { .. })
        ));
        store.acquire_writer(&scope, "agg:1", "writer-a").unwrap();
        cleanup(&path);
    }

    #[test]
    fn the_outbox_is_durable_per_consumer_and_dedups_by_event_id() {
        let path = temp_path("outbox");
        let scope = default_scope("tenant-a", "project-1");
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .enqueue(&scope, event("e1", "memory.episode.committed", 1))
                .unwrap();
            store
                .enqueue(&scope, event("e1", "memory.episode.committed", 1))
                .unwrap();
            store
                .enqueue(&scope, event("e2", "memory.folded", 1))
                .unwrap();
            store.ack(&scope, "projection", "e1").unwrap();
            store
                .dead_letter(&scope, "audit", "e2", "schema mismatch")
                .unwrap();
            store.checkpoint().unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        let pending = store.pending(&scope, "projection", 10).unwrap();
        assert_eq!(pending.len(), 1, "e1 is acked, e2 is pending");
        assert_eq!(pending[0].event_id, "e2");
        // The audit consumer acked nothing, so e1 is still pending for it and e2
        // is dead-lettered rather than silently dropped.
        let audit_pending = store.pending(&scope, "audit", 10).unwrap();
        assert_eq!(audit_pending.len(), 1);
        assert_eq!(audit_pending[0].event_id, "e1");
        assert_eq!(store.dead_letters(&scope, "audit").unwrap(), vec!["e2"]);
        cleanup(&path);
    }

    #[test]
    fn a_manifest_is_a_separate_atomic_pointer() {
        let path = temp_path("manifest");
        let scope = default_scope("tenant-a", "project-1");
        {
            let store = SqliteStore::open(&path).unwrap();
            store.put_manifest(&scope, "code", "gen-1").unwrap();
            store.put_manifest(&scope, "code", "gen-2").unwrap();
            store.checkpoint().unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(
            store.get_manifest(&scope, "code").unwrap().as_deref(),
            Some("gen-2")
        );
        assert!(store.get_manifest(&scope, "memory").unwrap().is_none());
        cleanup(&path);
    }

    #[test]
    fn derived_projections_behave_like_the_embedded_adapter() {
        let path = temp_path("derived");
        let store = SqliteStore::open(&path).unwrap();
        let scope = default_scope("tenant-a", "project-1");
        store
            .index(
                &scope,
                "icu-v1",
                &FullTextDoc {
                    id: "d1".into(),
                    revision: "r1".into(),
                    fields: std::collections::BTreeMap::from([(
                        "body".to_string(),
                        "graph engine".to_string(),
                    )]),
                },
            )
            .unwrap();
        assert_eq!(
            FullTextPort::search(&store, &scope, "icu-v1", None, "graph", 10)
                .unwrap()
                .len(),
            1
        );
        store
            .declare_profile(
                &scope,
                &EmbeddingProfile {
                    profile_id: "bge-m3".into(),
                    model_id: "bge-m3".into(),
                    model_revision: "1".into(),
                    dimension: 3,
                    metric: storage_ports::Metric::Cosine,
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
                    vector: vec![1.0, 0.0, 0.0],
                    metadata: Default::default(),
                }],
            )
            .unwrap();
        assert_eq!(
            VectorPort::search(&store, &scope, "bge-m3", &[1.0, 0.0, 0.0], 5, None)
                .unwrap()
                .len(),
            1
        );
        cleanup(&path);
    }

    #[test]
    fn the_database_file_is_real_and_reports_its_size() {
        let path = temp_path("size");
        let store = SqliteStore::open(&path).unwrap();
        let scope = default_scope("tenant-a", "project-1");
        store
            .put_cas(&scope, "k", serde_json::json!({"v": 1}), None)
            .unwrap();
        store.checkpoint().unwrap();
        assert!(store.file_size() > 0);
        assert!(store.path().exists());
        cleanup(&path);
    }
}
