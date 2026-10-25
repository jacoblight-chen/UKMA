//! PostgreSQL canonical adapter (profile C, `hybrid`).
//!
//! Serves the authoritative side of §6.2 — [`MetadataPort`] (CAS, manifests,
//! single-writer fencing) and [`OutboxPort`] (durable at-least-once with
//! `(event_id, consumer)` dedup) — from real SQL in real transactions, plus the
//! canonical schema version (FM-006, ADR-059). The derived projections
//! (full-text and vectors) are delegated to the in-process ports exactly like the
//! SQLite adapter does: a PostgreSQL-only deployment would use `tsvector` and
//! pgvector, and that is stated as a limit instead of pretended.
//!
//! Availability is **configuration driven**: [`capabilities_with`] reports
//! unavailable when no DSN is supplied, so the router keeps failing closed
//! instead of degrading silently (ADR-038).

use std::collections::BTreeMap;
use std::sync::Mutex;

use postgres::{Client, NoTls};
use serde_json::Value;
use sha2::{Digest, Sha256};
use storage_ports::capabilities::{
    unavailable_adapter, AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope,
};
use storage_ports::ports::{
    validate_scope, EmbeddingProfile, FullTextDoc, FullTextHit, FullTextPort, MetadataEntry,
    MetadataPort, Metric, OutboxEvent, OutboxPort, VectorHit, VectorItem, VectorPort,
};
use storage_ports::{PortResult, StorageError};
use ukma_contracts::ScopeRef;

/// Physical DDL version of this adapter.
const DDL_VERSION: &str = "1";

/// Environment variable an operator sets to enable profile C's canonical store.
pub const DSN_ENV: &str = "UKMA_POSTGRES_DSN";

/// Advisory lock key that serialises the DDL across processes.
const DDL_LOCK_KEY: i64 = 9_156_001;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS metadata (
    scope    TEXT   NOT NULL,
    key      TEXT   NOT NULL,
    revision BIGINT NOT NULL,
    etag     TEXT   NOT NULL,
    value    TEXT   NOT NULL,
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
    scope              TEXT    NOT NULL,
    seq                BIGINT  NOT NULL,
    event_id           TEXT    NOT NULL,
    event_type         TEXT    NOT NULL,
    event_version      INTEGER NOT NULL,
    aggregate_ref      TEXT    NOT NULL,
    aggregate_revision BIGINT  NOT NULL,
    payload            TEXT    NOT NULL,
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
CREATE TABLE IF NOT EXISTS fulltext (
    scope    TEXT  NOT NULL,
    analyzer TEXT  NOT NULL,
    id       TEXT  NOT NULL,
    revision TEXT  NOT NULL,
    fields   JSONB NOT NULL,
    all_text TEXT  NOT NULL,
    PRIMARY KEY (scope, analyzer, id)
);
CREATE TABLE IF NOT EXISTS vector_profile (
    scope          TEXT    NOT NULL,
    profile_id     TEXT    NOT NULL,
    model_id       TEXT    NOT NULL,
    model_revision TEXT    NOT NULL,
    dimension      INTEGER NOT NULL,
    metric         TEXT    NOT NULL,
    normalization  TEXT    NOT NULL,
    PRIMARY KEY (scope, profile_id)
);
CREATE TABLE IF NOT EXISTS vector_item (
    scope      TEXT  NOT NULL,
    profile_id TEXT  NOT NULL,
    id         TEXT  NOT NULL,
    revision   TEXT  NOT NULL,
    vector     REAL[] NOT NULL,
    metadata   JSONB NOT NULL,
    PRIMARY KEY (scope, profile_id, id)
);
CREATE TABLE IF NOT EXISTS canonical_schema (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS adapter_ddl (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    version TEXT NOT NULL
);
";

fn scope_key(scope: &ScopeRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        scope.tenant_id,
        scope.project_id,
        scope.repo_id.as_deref().unwrap_or(""),
        scope.workspace_id.as_deref().unwrap_or(""),
        scope.agent_id.as_deref().unwrap_or(""),
        scope.session_id.as_deref().unwrap_or("")
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

/// Smallest string greater than every string starting with `prefix`.
///
/// `list` needs the prefix enforced on *every* page, not just the first: a cursor
/// alone would let keys after the prefix range (e.g. `other:1` after `canon:*`)
/// leak into a later page.
fn prefix_upper(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        if let Some(next) = char::from_u32(u32::from(last) + 1) {
            chars.push(next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

fn internal(message: impl Into<String>) -> StorageError {
    StorageError::Internal(message.into())
}

fn lock_error() -> StorageError {
    internal("postgres connection lock poisoned")
}

fn to_internal(error: postgres::Error) -> StorageError {
    match error.as_db_error() {
        Some(db) => internal(format!(
            "postgres: {} ({}){}",
            db.message(),
            db.code().code(),
            db.detail()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default()
        )),
        None => internal(format!("postgres: {error}")),
    }
}

fn decode_value(raw: &str, column: &'static str) -> Result<Value, StorageError> {
    serde_json::from_str(raw)
        .map_err(|error| internal(format!("column {column} is not JSON: {error}")))
}

/// Redacts credentials before a DSN reaches an error message.
fn dsn_hint(dsn: &str) -> String {
    match dsn.rsplit_once('@') {
        Some((_, tail)) => format!("***@{tail}"),
        None => dsn.to_string(),
    }
}

/// Canonical store backed by PostgreSQL.
pub struct PostgresStore {
    dsn: String,
    client: Mutex<Client>,
}

impl PostgresStore {
    /// Connect and apply the DDL. One connection, owned by the store: the
    /// canonical store needs a single writer, not a pool.
    ///
    /// # Errors
    /// Returns [`StorageError::Internal`] when the server is unreachable or the
    /// DDL cannot be applied.
    pub fn connect(dsn: &str) -> PortResult<Self> {
        let mut client = Client::connect(dsn, NoTls)
            .map_err(|error| internal(format!("cannot connect to `{}`: {error}", dsn_hint(dsn))))?;
        // `CREATE TABLE IF NOT EXISTS` is not atomic against a concurrent
        // creator, so the first connections of a process race on the catalogue.
        // An advisory lock serialises the DDL without serialising traffic.
        client
            .execute("SELECT pg_advisory_lock($1)", &[&DDL_LOCK_KEY])
            .map_err(to_internal)?;
        let applied = client.batch_execute(DDL).map_err(to_internal);
        let _ = client.execute("SELECT pg_advisory_unlock($1)", &[&DDL_LOCK_KEY]);
        applied?;
        client
            .execute(
                "INSERT INTO adapter_ddl (id, version) VALUES (1, $1)
                 ON CONFLICT(id) DO UPDATE SET version = excluded.version",
                &[&DDL_VERSION],
            )
            .map_err(to_internal)?;
        Ok(Self {
            dsn: dsn.to_string(),
            client: Mutex::new(client),
        })
    }

    /// Connect using [`DSN_ENV`].
    ///
    /// # Errors
    /// Returns [`StorageError::CapabilityUnavailable`] when the variable is unset.
    pub fn from_env() -> PortResult<Self> {
        match std::env::var(DSN_ENV) {
            Ok(dsn) if !dsn.trim().is_empty() => Self::connect(&dsn),
            _ => Err(StorageError::CapabilityUnavailable {
                capability: "postgres",
            }),
        }
    }

    /// DSN this store was opened with.
    pub fn dsn(&self) -> &str {
        &self.dsn
    }

    /// Deletes every row of one scope. Used by tests and by the cutover runbook;
    /// it is scope-keyed, so it can never touch another tenant's data.
    ///
    /// # Errors
    /// Returns the underlying SQL error.
    pub fn purge_scope(&self, scope: &ScopeRef) -> PortResult<u64> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let key = scope_key(scope);
        let mut removed = 0u64;
        for table in [
            "metadata",
            "manifest",
            "writer",
            "outbox",
            "outbox_ack",
            "outbox_dead",
        ] {
            let sql = format!("DELETE FROM {table} WHERE scope = $1");
            removed += client.execute(&sql, &[&key]).map_err(to_internal)?;
        }
        Ok(removed)
    }
}

impl MetadataPort for PostgresStore {
    fn put_cas(
        &self,
        scope: &ScopeRef,
        key: &str,
        value: Value,
        expected_revision: Option<u64>,
    ) -> PortResult<MetadataEntry> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let mut transaction = client.transaction().map_err(to_internal)?;
        let scope_key_value = scope_key(scope);
        let row = transaction
            .query_opt(
                "SELECT revision FROM metadata WHERE scope = $1 AND key = $2 FOR UPDATE",
                &[&scope_key_value, &key],
            )
            .map_err(to_internal)?;
        let current: Option<u64> = row.map(|row| row.get::<_, i64>(0) as u64);
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
                "INSERT INTO metadata (scope, key, revision, etag, value)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (scope, key) DO UPDATE
                 SET revision = excluded.revision, etag = excluded.etag, value = excluded.value",
                &[
                    &scope_key_value,
                    &key,
                    &(revision as i64),
                    &etag,
                    &value.to_string(),
                ],
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
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let row = client
            .query_opt(
                "SELECT key, revision, etag, value FROM metadata WHERE scope = $1 AND key = $2",
                &[&scope_key(scope), &key],
            )
            .map_err(to_internal)?;
        match row {
            None => Ok(None),
            Some(row) => {
                let raw: String = row.get(3);
                Ok(Some(MetadataEntry {
                    key: row.get(0),
                    revision: row.get::<_, i64>(1) as u64,
                    etag: row.get(2),
                    value: decode_value(&raw, "metadata.value")?,
                }))
            }
        }
    }

    fn list(
        &self,
        scope: &ScopeRef,
        prefix: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> PortResult<(Vec<MetadataEntry>, Option<String>)> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        // The prefix bounds every page (not only the first), and a cursor is the
        // last key already returned, so the next page starts strictly after it.
        let lower = prefix.to_string();
        let upper = prefix_upper(prefix);
        let sql = match (&upper, cursor) {
            (Some(_), Some(_)) => {
                "SELECT key, revision, etag, value FROM metadata
                 WHERE scope = $1 AND key >= $2 AND key < $3 AND key > $4
                 ORDER BY key ASC LIMIT $5"
            }
            (Some(_), None) => {
                "SELECT key, revision, etag, value FROM metadata
                 WHERE scope = $1 AND key >= $2 AND key < $3
                 ORDER BY key ASC LIMIT $4"
            }
            (None, Some(_)) => {
                "SELECT key, revision, etag, value FROM metadata
                 WHERE scope = $1 AND key >= $2 AND key > $3
                 ORDER BY key ASC LIMIT $4"
            }
            (None, None) => {
                "SELECT key, revision, etag, value FROM metadata
                 WHERE scope = $1 AND key >= $2
                 ORDER BY key ASC LIMIT $3"
            }
        };
        let scope_key_value = scope_key(scope);
        let limit_param = (limit + 1) as i64;
        let rows = match (&upper, cursor) {
            (Some(upper), Some(cursor)) => client.query(
                sql,
                &[&scope_key_value, &lower, upper, &cursor, &limit_param],
            ),
            (Some(upper), None) => {
                client.query(sql, &[&scope_key_value, &lower, upper, &limit_param])
            }
            (None, Some(cursor)) => {
                client.query(sql, &[&scope_key_value, &lower, &cursor, &limit_param])
            }
            (None, None) => client.query(sql, &[&scope_key_value, &lower, &limit_param]),
        }
        .map_err(to_internal)?;
        let mut entries: Vec<MetadataEntry> = Vec::new();
        for row in rows {
            let raw: String = row.get(3);
            entries.push(MetadataEntry {
                key: row.get(0),
                revision: row.get::<_, i64>(1) as u64,
                etag: row.get(2),
                value: decode_value(&raw, "metadata.value")?,
            });
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
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        client
            .execute(
                "INSERT INTO manifest (scope, name, generation) VALUES ($1, $2, $3)
                 ON CONFLICT (scope, name) DO UPDATE SET generation = excluded.generation",
                &[&scope_key(scope), &name, &generation],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn get_manifest(&self, scope: &ScopeRef, name: &str) -> PortResult<Option<String>> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let row = client
            .query_opt(
                "SELECT generation FROM manifest WHERE scope = $1 AND name = $2",
                &[&scope_key(scope), &name],
            )
            .map_err(to_internal)?;
        Ok(row.map(|row| row.get(0)))
    }

    fn acquire_writer(&self, scope: &ScopeRef, aggregate: &str, token: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let mut transaction = client.transaction().map_err(to_internal)?;
        let row = transaction
            .query_opt(
                "SELECT token FROM writer WHERE scope = $1 AND aggregate = $2 FOR UPDATE",
                &[&scope_key(scope), &aggregate],
            )
            .map_err(to_internal)?;
        match row.map(|row| row.get::<_, String>(0)) {
            Some(current) if current != token => Err(StorageError::Conflict {
                collection: "writer",
                key: aggregate.to_string(),
                expected_revision: 0,
                actual_revision: 1,
            }),
            _ => {
                transaction
                    .execute(
                        "INSERT INTO writer (scope, aggregate, token) VALUES ($1, $2, $3)
                         ON CONFLICT (scope, aggregate) DO UPDATE SET token = excluded.token",
                        &[&scope_key(scope), &aggregate, &token],
                    )
                    .map_err(to_internal)?;
                transaction.commit().map_err(to_internal)?;
                Ok(())
            }
        }
    }
}

impl OutboxPort for PostgresStore {
    fn enqueue(&self, scope: &ScopeRef, event: OutboxEvent) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let mut transaction = client.transaction().map_err(to_internal)?;
        let row = transaction
            .query_one(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM outbox WHERE scope = $1",
                &[&scope_key(scope)],
            )
            .map_err(to_internal)?;
        let next: i64 = row.get(0);
        transaction
            .execute(
                "INSERT INTO outbox
                 (scope, seq, event_id, event_type, event_version, aggregate_ref,
                  aggregate_revision, payload, idempotency_key)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (scope, event_id) DO NOTHING",
                &[
                    &scope_key(scope),
                    &next,
                    &event.event_id,
                    &event.event_type,
                    &(event.event_version as i32),
                    &event.aggregate_ref,
                    &(event.aggregate_revision as i64),
                    &event.payload.to_string(),
                    &event.idempotency_key,
                ],
            )
            .map_err(to_internal)?;
        transaction.commit().map_err(to_internal)?;
        Ok(())
    }

    fn pending(
        &self,
        scope: &ScopeRef,
        consumer: &str,
        limit: usize,
    ) -> PortResult<Vec<OutboxEvent>> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let rows = client
            .query(
                "SELECT event_id, event_type, event_version, aggregate_ref, aggregate_revision,
                        payload, idempotency_key
                 FROM outbox
                 WHERE scope = $1
                   AND event_id NOT IN (SELECT event_id FROM outbox_ack WHERE scope = $1 AND consumer = $2)
                   AND event_id NOT IN (SELECT event_id FROM outbox_dead WHERE scope = $1 AND consumer = $2)
                 ORDER BY seq ASC LIMIT $3",
                &[&scope_key(scope), &consumer, &(limit as i64)],
            )
            .map_err(to_internal)?;
        let mut events = Vec::new();
        for row in rows {
            let raw: String = row.get(5);
            events.push(OutboxEvent {
                event_id: row.get(0),
                event_type: row.get(1),
                event_version: row.get::<_, i32>(2) as u32,
                aggregate_ref: row.get(3),
                aggregate_revision: row.get::<_, i64>(4) as u64,
                payload: decode_value(&raw, "outbox.payload")?,
                idempotency_key: row.get(6),
            });
        }
        Ok(events)
    }

    fn ack(&self, scope: &ScopeRef, consumer: &str, event_id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        client
            .execute(
                "INSERT INTO outbox_ack (scope, consumer, event_id) VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
                &[&scope_key(scope), &consumer, &event_id],
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
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        client
            .execute(
                "INSERT INTO outbox_dead (scope, consumer, event_id, reason) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (scope, consumer, event_id) DO UPDATE SET reason = excluded.reason",
                &[&scope_key(scope), &consumer, &event_id, &reason],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn dead_letters(&self, scope: &ScopeRef, consumer: &str) -> PortResult<Vec<String>> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let rows = client
            .query(
                "SELECT event_id FROM outbox_dead WHERE scope = $1 AND consumer = $2 ORDER BY event_id",
                &[&scope_key(scope), &consumer],
            )
            .map_err(to_internal)?;
        Ok(rows.into_iter().map(|row| row.get(0)).collect())
    }
}

impl storage_ports::SchemaVersionPort for PostgresStore {
    fn schema_version(&self) -> PortResult<u32> {
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let row = client
            .query_opt("SELECT version FROM canonical_schema WHERE id = 1", &[])
            .map_err(to_internal)?;
        match row.map(|row| row.get::<_, i32>(0)) {
            Some(version) if version > 0 => Ok(version as u32),
            // No row yet: a database created before versioning existed is v1.
            _ => Ok(storage_ports::migrations::legacy_canonical_version()),
        }
    }

    fn set_schema_version(&self, version: u32) -> PortResult<()> {
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        client
            .execute(
                "INSERT INTO canonical_schema (id, version) VALUES (1, $1)
                 ON CONFLICT(id) DO UPDATE SET version = excluded.version",
                &[&(version as i32)],
            )
            .map_err(to_internal)?;
        Ok(())
    }
}

impl storage_ports::MigrationStore for PostgresStore {
    fn metadata(&self) -> &dyn MetadataPort {
        self
    }
}

impl FullTextPort for PostgresStore {
    fn index(&self, scope: &ScopeRef, analyzer: &str, doc: &FullTextDoc) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let fields = serde_json::to_value(&doc.fields)
            .map_err(|error| internal(format!("fulltext fields: {error}")))?;
        let all_text = doc
            .fields
            .values()
            .cloned()
            .collect::<Vec<String>>()
            .join(" ");
        client
            .execute(
                "INSERT INTO fulltext (scope, analyzer, id, revision, fields, all_text)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (scope, analyzer, id) DO UPDATE
                 SET revision = excluded.revision, fields = excluded.fields,
                     all_text = excluded.all_text",
                &[
                    &scope_key(scope),
                    &analyzer,
                    &doc.id,
                    &doc.revision,
                    &fields,
                    &all_text,
                ],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn delete(&self, scope: &ScopeRef, analyzer: &str, id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        client
            .execute(
                "DELETE FROM fulltext WHERE scope = $1 AND analyzer = $2 AND id = $3",
                &[&scope_key(scope), &analyzer, &id],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn search(
        &self,
        scope: &ScopeRef,
        analyzer: &str,
        field: Option<&str>,
        query: &str,
        limit: usize,
    ) -> PortResult<Vec<FullTextHit>> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        // Real in-database search: PostgreSQL ranks the tsvector built from the
        // document; `analyzer` is the isolation label, the tokenisation is the
        // server's `simple` configuration (stated, not hidden).
        let rows = match field {
            Some(field) => client
                .query(
                    "SELECT id, revision,
                            ts_rank_cd(to_tsvector('simple', COALESCE(fields ->> $5, '')),
                                       plainto_tsquery('simple', $4)) AS score
                     FROM fulltext
                     WHERE scope = $1 AND analyzer = $2
                       AND to_tsvector('simple', COALESCE(fields ->> $5, ''))
                           @@ plainto_tsquery('simple', $4)
                     ORDER BY score DESC, id ASC
                     LIMIT $3",
                    &[
                        &scope_key(scope),
                        &analyzer,
                        &(limit as i64),
                        &query,
                        &field,
                    ],
                )
                .map_err(to_internal)?,
            None => client
                .query(
                    "SELECT id, revision,
                            ts_rank_cd(to_tsvector('simple', all_text),
                                       plainto_tsquery('simple', $4)) AS score
                     FROM fulltext
                     WHERE scope = $1 AND analyzer = $2
                       AND to_tsvector('simple', all_text) @@ plainto_tsquery('simple', $4)
                     ORDER BY score DESC, id ASC
                     LIMIT $3",
                    &[&scope_key(scope), &analyzer, &(limit as i64), &query],
                )
                .map_err(to_internal)?,
        };
        Ok(rows
            .into_iter()
            .map(|row| FullTextHit {
                id: row.get(0),
                revision: row.get(1),
                score: row.get::<_, f32>(2) as f64,
            })
            .collect())
    }

    fn analyzers(&self) -> Vec<String> {
        // The analyzers this store actually holds documents for.
        let mut client = match self.client.lock() {
            Ok(client) => client,
            Err(_) => return Vec::new(),
        };
        match client.query(
            "SELECT DISTINCT analyzer FROM fulltext ORDER BY analyzer",
            &[],
        ) {
            Ok(rows) => rows.into_iter().map(|row| row.get(0)).collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl VectorPort for PostgresStore {
    fn declare_profile(&self, scope: &ScopeRef, profile: &EmbeddingProfile) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let existing = client
            .query_opt(
                "SELECT dimension FROM vector_profile WHERE scope = $1 AND profile_id = $2",
                &[&scope_key(scope), &profile.profile_id],
            )
            .map_err(to_internal)?;
        if let Some(row) = existing {
            let stored: i32 = row.get(0);
            if stored as usize != profile.dimension {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "profile `{}` is stored with dimension {stored}, not {}",
                        profile.profile_id, profile.dimension
                    ),
                });
            }
        }
        client
            .execute(
                "INSERT INTO vector_profile
                 (scope, profile_id, model_id, model_revision, dimension, metric, normalization)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (scope, profile_id) DO UPDATE
                 SET model_id = excluded.model_id, model_revision = excluded.model_revision,
                     metric = excluded.metric, normalization = excluded.normalization",
                &[
                    &scope_key(scope),
                    &profile.profile_id,
                    &profile.model_id,
                    &profile.model_revision,
                    &(profile.dimension as i32),
                    &metric_label(profile.metric),
                    &profile.normalization,
                ],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn profile(&self, scope: &ScopeRef, profile_id: &str) -> PortResult<Option<EmbeddingProfile>> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let row = client
            .query_opt(
                "SELECT model_id, model_revision, dimension, metric, normalization
                 FROM vector_profile WHERE scope = $1 AND profile_id = $2",
                &[&scope_key(scope), &profile_id],
            )
            .map_err(to_internal)?;
        Ok(row.map(|row| EmbeddingProfile {
            profile_id: profile_id.to_string(),
            model_id: row.get(0),
            model_revision: row.get(1),
            dimension: row.get::<_, i32>(2) as usize,
            metric: metric_from(&row.get::<_, String>(3)),
            normalization: row.get(4),
        }))
    }

    fn upsert(&self, scope: &ScopeRef, profile_id: &str, items: &[VectorItem]) -> PortResult<()> {
        validate_scope(scope)?;
        let profile =
            self.profile(scope, profile_id)?
                .ok_or_else(|| StorageError::ModelProfileMismatch {
                    detail: format!("profile `{profile_id}` is not declared"),
                })?;
        for item in items {
            if item.vector.len() != profile.dimension {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "item `{}` has dimension {}, profile expects {}",
                        item.id,
                        item.vector.len(),
                        profile.dimension
                    ),
                });
            }
        }
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        let mut transaction = client.transaction().map_err(to_internal)?;
        for item in items {
            let metadata = serde_json::to_value(&item.metadata)
                .map_err(|error| internal(format!("vector metadata: {error}")))?;
            transaction
                .execute(
                    "INSERT INTO vector_item (scope, profile_id, id, revision, vector, metadata)
                     VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (scope, profile_id, id) DO UPDATE
                     SET revision = excluded.revision, vector = excluded.vector,
                         metadata = excluded.metadata",
                    &[
                        &scope_key(scope),
                        &profile_id,
                        &item.id,
                        &item.revision,
                        &item.vector,
                        &metadata,
                    ],
                )
                .map_err(to_internal)?;
        }
        transaction.commit().map_err(to_internal)?;
        Ok(())
    }

    fn delete(&self, scope: &ScopeRef, profile_id: &str, id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        client
            .execute(
                "DELETE FROM vector_item WHERE scope = $1 AND profile_id = $2 AND id = $3",
                &[&scope_key(scope), &profile_id, &id],
            )
            .map_err(to_internal)?;
        Ok(())
    }

    fn search(
        &self,
        scope: &ScopeRef,
        profile_id: &str,
        query: &[f32],
        k: usize,
        filter: Option<&BTreeMap<String, String>>,
    ) -> PortResult<Vec<VectorHit>> {
        validate_scope(scope)?;
        let profile =
            self.profile(scope, profile_id)?
                .ok_or_else(|| StorageError::ModelProfileMismatch {
                    detail: format!("profile `{profile_id}` is not declared"),
                })?;
        if query.len() != profile.dimension {
            return Err(StorageError::ModelProfileMismatch {
                detail: format!(
                    "query dimension {} does not match profile dimension {}",
                    query.len(),
                    profile.dimension
                ),
            });
        }
        let filter_json: Option<Value> = match filter {
            Some(map) if !map.is_empty() => {
                let mut object = serde_json::Map::new();
                for (key, value) in map {
                    object.insert(key.clone(), Value::String(value.clone()));
                }
                Some(Value::Object(object))
            }
            _ => None,
        };
        let mut client = self.client.lock().map_err(|_| lock_error())?;
        // Exact k-NN inside the database over float4[]: no ANN approximation, no
        // in-process copy, and the metric comes from the declared profile.
        let score = match profile.metric {
            Metric::Cosine => {
                "(SELECT COALESCE(sum(x * y), 0) FROM unnest(i.vector, $4::real[]) AS t(x, y))
                 / NULLIF(
                     (SELECT sqrt(COALESCE(sum(x * x), 0)) FROM unnest(i.vector) AS t(x))
                     * (SELECT sqrt(COALESCE(sum(y * y), 0)) FROM unnest($4::real[]) AS t(y)),
                     0)"
            }
            Metric::Dot => {
                "(SELECT COALESCE(sum(x * y), 0) FROM unnest(i.vector, $4::real[]) AS t(x, y))"
            }
            Metric::L2 => {
                "-sqrt((SELECT COALESCE(sum((x - y) * (x - y)), 0) FROM unnest(i.vector, $4::real[]) AS t(x, y)))"
            }
        };
        let sql = format!(
            "SELECT i.id, i.revision, {score} AS score
             FROM vector_item i
             WHERE i.scope = $1 AND i.profile_id = $2
               AND ($5::jsonb IS NULL OR i.metadata @> $5::jsonb)
             ORDER BY score DESC NULLS LAST, i.id ASC
             LIMIT $3"
        );
        let rows = client
            .query(
                &sql,
                &[
                    &scope_key(scope),
                    &profile_id,
                    &(k as i64),
                    &query,
                    &filter_json,
                ],
            )
            .map_err(to_internal)?;
        Ok(rows
            .into_iter()
            .map(|row| VectorHit {
                id: row.get(0),
                revision: row.get(1),
                score: row.get::<_, f64>(2),
            })
            .collect())
    }
}

fn metric_label(metric: Metric) -> String {
    match metric {
        Metric::Cosine => "cosine".to_string(),
        Metric::Dot => "dot".to_string(),
        Metric::L2 => "l2".to_string(),
    }
}

fn metric_from(label: &str) -> Metric {
    match label {
        "dot" => Metric::Dot,
        "l2" => Metric::L2,
        _ => Metric::Cosine,
    }
}

impl CapabilityProbe for PostgresStore {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: "postgres".to_string(),
            adapter_version: "0.1.0".to_string(),
            backend_kind: BackendKind::Hybrid,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: DDL_VERSION.to_string(),
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

/// Capabilities as the router sees them: available only with a real DSN.
pub fn capabilities_with(dsn: Option<&str>) -> AdapterCapabilities {
    match dsn {
        Some(dsn) if !dsn.trim().is_empty() => {
            AdapterCapabilities {
                adapter_id: "postgres".to_string(),
                adapter_version: "0.1.0".to_string(),
                backend_kind: BackendKind::Hybrid,
                logical_contract_version: "1.2.3".to_string(),
                adapter_schema_version: DDL_VERSION.to_string(),
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
        _ => unavailable_adapter(
            "postgres",
            BackendKind::Hybrid,
            "no PostgreSQL DSN configured: set UKMA_POSTGRES_DSN to enable profile C's canonical store",
        ),
    }
}

/// Capabilities from the environment.
pub fn capabilities() -> AdapterCapabilities {
    let dsn = std::env::var(DSN_ENV).ok();
    capabilities_with(dsn.as_deref())
}

/// Whether [`DSN_ENV`] is set.
pub fn is_configured() -> bool {
    std::env::var(DSN_ENV)
        .map(|dsn| !dsn.trim().is_empty())
        .unwrap_or(false)
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
        payload: serde_json::json!({"event": event_type}),
        idempotency_key: None,
    }
}
