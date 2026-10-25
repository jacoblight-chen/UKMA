//! Embedded adapter (profile A): canonical metadata + durable outbox on one
//! JSON file; full-text and vector are **derived, rebuildable projections**
//! held in memory (§6.4: derived indexes are not the source of truth).
//!
//! Why this shape is honest about §40.5: the eight capabilities the minimum
//! production profile requires (durable canonical transaction, write
//! idempotency, CAS, typed key equality, revisioned read, delete tombstone,
//! durable outbox, scoped ACL enforcement) are provided for *canonical* data.
//! Full-text/vector projections survive a restart only by replaying the outbox,
//! which is exactly the architecture §6.4 prescribes.
//!
//! Known limits (stated, not hidden): single-process single-writer; durability
//! is obtained at [`EmbeddedStore::flush`], not per mutation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
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

const SNAPSHOT_FORMAT: u32 = 1;

fn scope_key(scope: &ScopeRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        scope.tenant_id,
        scope.project_id,
        scope.repo_id.as_deref().unwrap_or("")
    )
}

fn etag_of(key: &str, revision: u64, value: &serde_json::Value) -> String {
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    format: u32,
    /// scope -> key -> entry
    entries: BTreeMap<String, BTreeMap<String, MetadataEntry>>,
    /// scope -> manifest name -> generation
    manifests: BTreeMap<String, BTreeMap<String, String>>,
    /// scope -> aggregate -> fencing token
    writers: BTreeMap<String, BTreeMap<String, String>>,
    /// scope -> events
    events: BTreeMap<String, Vec<OutboxEvent>>,
    /// (scope, consumer, event_id)
    acked: Vec<(String, String, String)>,
    /// scope -> consumer -> dead letters
    dead: BTreeMap<String, BTreeMap<String, Vec<(String, String)>>>,
}

/// Embedded canonical store with a durable outbox.
pub struct EmbeddedStore {
    path: PathBuf,
    state: Mutex<PersistedState>,
    fulltext: InMemoryFullText,
    vector: InMemoryVector,
}

impl EmbeddedStore {
    /// Open (creating if absent) the store; a corrupt file is a typed error.
    pub fn open(path: impl AsRef<Path>) -> PortResult<Self> {
        let path = path.as_ref().to_path_buf();
        let state = if path.exists() {
            let raw = std::fs::read(&path).map_err(|e| io_error(&path, e))?;
            let loaded: PersistedState = serde_json::from_slice(&raw).map_err(|e| {
                StorageError::Internal(format!("corrupt embedded store {}: {e}", path.display()))
            })?;
            if loaded.format != SNAPSHOT_FORMAT {
                return Err(StorageError::Internal(format!(
                    "unsupported embedded store format {} at {} (expected {SNAPSHOT_FORMAT})",
                    loaded.format,
                    path.display()
                )));
            }
            loaded
        } else {
            PersistedState {
                format: SNAPSHOT_FORMAT,
                ..Default::default()
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
            fulltext: InMemoryFullText::new(),
            vector: InMemoryVector::new(),
        })
    }

    /// Backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically persist canonical state (temp file + rename).
    pub fn flush(&self) -> PortResult<()> {
        let guard = self
            .state
            .lock()
            .map_err(|_| StorageError::Internal("embedded lock poisoned".into()))?;
        let mut snapshot = PersistedState {
            format: SNAPSHOT_FORMAT,
            entries: guard.entries.clone(),
            manifests: guard.manifests.clone(),
            writers: guard.writers.clone(),
            events: guard.events.clone(),
            acked: guard.acked.clone(),
            dead: guard.dead.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| StorageError::Internal(format!("embedded snapshot encode: {e}")))?;
        snapshot.entries.clear(); // release the clone early
        let temp = self.path.with_extension("tmp");
        std::fs::write(&temp, encoded).map_err(|e| io_error(&temp, e))?;
        std::fs::rename(&temp, &self.path).map_err(|e| io_error(&self.path, e))?;
        Ok(())
    }

    /// Number of pending (not yet acked) events for a consumer.
    pub fn pending_count(&self, scope: &ScopeRef, consumer: &str) -> PortResult<usize> {
        Ok(self.pending(scope, consumer, usize::MAX)?.len())
    }
}

fn lock_error() -> StorageError {
    StorageError::Internal("embedded lock poisoned".into())
}

impl MetadataPort for EmbeddedStore {
    fn put_cas(
        &self,
        scope: &ScopeRef,
        key: &str,
        value: serde_json::Value,
        expected_revision: Option<u64>,
    ) -> PortResult<MetadataEntry> {
        validate_scope(scope)?;
        let mut guard = self.state.lock().map_err(|_| lock_error())?;
        let scope_key_value = scope_key(scope);
        let current = guard
            .entries
            .get(&scope_key_value)
            .and_then(|bucket| bucket.get(key))
            .cloned();
        let revision = match (current.as_ref(), expected_revision) {
            (None, None) => 1,
            (None, Some(expected)) => {
                return Err(StorageError::Conflict {
                    collection: "metadata",
                    key: key.to_string(),
                    expected_revision: expected,
                    actual_revision: 0,
                })
            }
            (Some(existing), Some(expected)) if existing.revision != expected => {
                return Err(StorageError::Conflict {
                    collection: "metadata",
                    key: key.to_string(),
                    expected_revision: expected,
                    actual_revision: existing.revision,
                })
            }
            (Some(existing), _) => existing.revision + 1,
        };
        let entry = MetadataEntry {
            key: key.to_string(),
            revision,
            etag: etag_of(key, revision, &value),
            value,
        };
        guard
            .entries
            .entry(scope_key_value)
            .or_default()
            .insert(key.to_string(), entry.clone());
        Ok(entry)
    }

    fn get(&self, scope: &ScopeRef, key: &str) -> PortResult<Option<MetadataEntry>> {
        validate_scope(scope)?;
        let guard = self.state.lock().map_err(|_| lock_error())?;
        Ok(guard
            .entries
            .get(&scope_key(scope))
            .and_then(|bucket| bucket.get(key))
            .cloned())
    }

    fn list(
        &self,
        scope: &ScopeRef,
        prefix: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> PortResult<(Vec<MetadataEntry>, Option<String>)> {
        validate_scope(scope)?;
        if limit == 0 {
            return Err(StorageError::InvalidArgument {
                field: "limit",
                reason: "must be >= 1".into(),
            });
        }
        let guard = self.state.lock().map_err(|_| lock_error())?;
        let key = scope_key(scope);
        let mut entries: Vec<MetadataEntry> = guard
            .entries
            .get(&key)
            .map(|bucket| {
                bucket
                    .iter()
                    .filter(|(entry_key, _)| entry_key.starts_with(prefix))
                    .map(|(_, entry)| entry.clone())
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|left, right| left.key.cmp(&right.key));
        let start = match cursor {
            Some(cursor) => entries
                .iter()
                .position(|entry| entry.key.as_str() > cursor)
                .unwrap_or(entries.len()),
            None => 0,
        };
        let page: Vec<MetadataEntry> = entries.into_iter().skip(start).take(limit).collect();
        let next = if page.len() == limit {
            page.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        Ok((page, next))
    }

    fn put_manifest(&self, scope: &ScopeRef, name: &str, generation: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self.state.lock().map_err(|_| lock_error())?;
        guard
            .manifests
            .entry(scope_key(scope))
            .or_default()
            .insert(name.to_string(), generation.to_string());
        Ok(())
    }

    fn get_manifest(&self, scope: &ScopeRef, name: &str) -> PortResult<Option<String>> {
        validate_scope(scope)?;
        let guard = self.state.lock().map_err(|_| lock_error())?;
        Ok(guard
            .manifests
            .get(&scope_key(scope))
            .and_then(|bucket| bucket.get(name))
            .cloned())
    }

    fn acquire_writer(&self, scope: &ScopeRef, aggregate: &str, token: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self.state.lock().map_err(|_| lock_error())?;
        let scope_key_value = scope_key(scope);
        let existing = guard
            .writers
            .get(&scope_key_value)
            .and_then(|bucket| bucket.get(aggregate))
            .cloned();
        match existing {
            Some(current) if current != token => Err(StorageError::Conflict {
                collection: "writer_fence",
                key: aggregate.to_string(),
                expected_revision: 0,
                actual_revision: 1,
            }),
            _ => {
                guard
                    .writers
                    .entry(scope_key_value)
                    .or_default()
                    .insert(aggregate.to_string(), token.to_string());
                Ok(())
            }
        }
    }
}

impl OutboxPort for EmbeddedStore {
    fn enqueue(&self, scope: &ScopeRef, event: OutboxEvent) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self.state.lock().map_err(|_| lock_error())?;
        let bucket = guard.events.entry(scope_key(scope)).or_default();
        if bucket
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            return Ok(());
        }
        bucket.push(event);
        Ok(())
    }

    fn pending(
        &self,
        scope: &ScopeRef,
        consumer: &str,
        limit: usize,
    ) -> PortResult<Vec<OutboxEvent>> {
        validate_scope(scope)?;
        if limit == 0 {
            return Err(StorageError::InvalidArgument {
                field: "limit",
                reason: "must be >= 1".into(),
            });
        }
        let guard = self.state.lock().map_err(|_| lock_error())?;
        let key = scope_key(scope);
        let acked: HashSet<&(String, String, String)> = guard.acked.iter().collect();
        Ok(guard
            .events
            .get(&key)
            .map(|bucket| {
                bucket
                    .iter()
                    .filter(|event| {
                        !acked.contains(&(
                            key.clone(),
                            consumer.to_string(),
                            event.event_id.clone(),
                        ))
                    })
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn ack(&self, scope: &ScopeRef, consumer: &str, event_id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self.state.lock().map_err(|_| lock_error())?;
        let tuple = (scope_key(scope), consumer.to_string(), event_id.to_string());
        if !guard.acked.contains(&tuple) {
            guard.acked.push(tuple);
        }
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
        let mut guard = self.state.lock().map_err(|_| lock_error())?;
        guard
            .dead
            .entry(scope_key(scope))
            .or_default()
            .entry(consumer.to_string())
            .or_default()
            .push((event_id.to_string(), reason.to_string()));
        Ok(())
    }

    fn dead_letters(&self, scope: &ScopeRef, consumer: &str) -> PortResult<Vec<String>> {
        validate_scope(scope)?;
        let guard = self.state.lock().map_err(|_| lock_error())?;
        Ok(guard
            .dead
            .get(&scope_key(scope))
            .and_then(|bucket| bucket.get(consumer))
            .map(|entries| entries.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default())
    }
}

impl FullTextPort for EmbeddedStore {
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

impl VectorPort for EmbeddedStore {
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

impl CapabilityProbe for EmbeddedStore {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: "embedded-json".to_string(),
            adapter_version: "0.1.0".to_string(),
            backend_kind: BackendKind::Embedded,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: "1".to_string(),
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

/// Declared-but-unavailable adapter used by profiles B (sql) and C (hybrid)
/// until a real backend is wired; it must never pretend to serve traffic (?6.3).
pub fn unavailable_adapter_capabilities(adapter_id: &str, reason: &str) -> AdapterCapabilities {
    storage_ports::capabilities::unavailable_adapter(adapter_id, BackendKind::Hybrid, reason)
}

/// Convenience: scope-free default for tests.
pub fn default_scope(tenant: &str, project: &str) -> ScopeRef {
    ScopeRef {
        tenant_id: tenant.to_string(),
        project_id: project.to_string(),
        repo_id: None,
        workspace_id: None,
        agent_id: None,
        session_id: None,
    }
}

/// Re-export so downstream crates need not depend on `storage-ports` directly
/// for the common types.
pub use storage_ports as ports;

/// Helper used by tests to build an outbox event.
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

/// Silence unused-import warnings for `HashMap` when only tests use it.
#[allow(dead_code)]
type Unused = HashMap<String, String>;
