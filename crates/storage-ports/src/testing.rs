//! In-memory implementations of every port (test + development only).
//!
//! These are the reference semantics each adapter MUST reproduce; they are not
//! production backends (no durability, no cross-process safety).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use ukma_contracts::ScopeRef;

use crate::capabilities::{AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope};
use crate::error::{PortResult, StorageError};
use crate::migration::{MigrationStore, SchemaVersionPort};
use crate::ports::{
    validate_scope, ArtifactPort, ArtifactRecord, EmbeddingProfile, FullTextDoc, FullTextHit,
    FullTextPort, MetadataEntry, MetadataPort, Metric, OutboxEvent, OutboxPort, Retention,
    VectorHit, VectorItem, VectorPort,
};

/// `sha256:<hex>` content address of `bytes` (§31.3 `Hash`).
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(71);
    hex.push_str("sha256:");
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Scope identity used as the isolation key inside the in-memory maps.
fn scope_key(scope: &ScopeRef) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        scope.tenant_id,
        scope.project_id,
        scope.repo_id.as_deref().unwrap_or(""),
        scope.session_id.as_deref().unwrap_or("")
    )
}

fn internal(message: impl Into<String>) -> StorageError {
    StorageError::Internal(message.into())
}

// ---------------------------------------------------------------------------
// Artifact
// ---------------------------------------------------------------------------

type ArtifactKey = (String, String);
type ArtifactEntry = (Vec<u8>, ArtifactRecord);
type ArtifactMap = HashMap<ArtifactKey, ArtifactEntry>;
type DeadLetterKey = (String, String);
type DeadLetterList = Vec<(String, String)>;
type DeadLetterMap = HashMap<DeadLetterKey, DeadLetterList>;

/// Content-addressed in-memory artifact store.
#[derive(Default)]
pub struct InMemoryArtifact {
    inner: Mutex<ArtifactMap>,
}

impl InMemoryArtifact {
    /// New empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ArtifactPort for InMemoryArtifact {
    fn put_immutable(
        &self,
        scope: &ScopeRef,
        bytes: &[u8],
        media_type: &str,
        retention: Retention,
    ) -> PortResult<ArtifactRecord> {
        validate_scope(scope)?;
        let hash = content_hash(bytes);
        let record = ArtifactRecord {
            hash: hash.clone(),
            media_type: media_type.to_string(),
            byte_len: bytes.len() as u64,
            retention,
        };
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| internal("artifact lock poisoned"))?;
        guard
            .entry((scope_key(scope), hash))
            .or_insert_with(|| (bytes.to_vec(), record.clone()));
        Ok(record)
    }

    fn get(&self, scope: &ScopeRef, hash: &str) -> PortResult<Vec<u8>> {
        validate_scope(scope)?;
        let guard = self
            .inner
            .lock()
            .map_err(|_| internal("artifact lock poisoned"))?;
        guard
            .get(&(scope_key(scope), hash.to_string()))
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| StorageError::NotFound {
                collection: "artifact",
                key: hash.to_string(),
            })
    }

    fn verify(&self, scope: &ScopeRef, hash: &str) -> PortResult<bool> {
        let bytes = self.get(scope, hash)?;
        Ok(content_hash(&bytes) == hash)
    }

    fn exists(&self, scope: &ScopeRef, hash: &str) -> PortResult<bool> {
        validate_scope(scope)?;
        let guard = self
            .inner
            .lock()
            .map_err(|_| internal("artifact lock poisoned"))?;
        Ok(guard.contains_key(&(scope_key(scope), hash.to_string())))
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Authoritative in-memory metadata store with CAS + manifests + fencing.
pub struct InMemoryMetadata {
    entries: Mutex<HashMap<(String, String), MetadataEntry>>,
    manifests: Mutex<HashMap<(String, String), String>>,
    writers: Mutex<HashMap<(String, String), String>>,
    schema_version: AtomicU32,
}

impl Default for InMemoryMetadata {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
            writers: Mutex::new(HashMap::new()),
            // A fresh store is a v1 canonical store.
            schema_version: AtomicU32::new(1),
        }
    }
}

impl SchemaVersionPort for InMemoryMetadata {
    fn schema_version(&self) -> PortResult<u32> {
        Ok(self.schema_version.load(Ordering::SeqCst))
    }

    fn set_schema_version(&self, version: u32) -> PortResult<()> {
        // 0 is the legitimate "pre-versioning" state of a legacy file.
        self.schema_version.store(version, Ordering::SeqCst);
        Ok(())
    }
}

impl MigrationStore for InMemoryMetadata {
    fn metadata(&self) -> &dyn MetadataPort {
        self
    }
}

impl InMemoryMetadata {
    /// New empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

fn entry_etag(key: &str, revision: u64, value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(revision.to_le_bytes());
    hasher.update(value.to_string().as_bytes());
    let digest = hasher.finalize();
    format!(
        "{:016x}",
        u64::from_be_bytes([
            digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
        ])
    )
}

impl MetadataPort for InMemoryMetadata {
    fn put_cas(
        &self,
        scope: &ScopeRef,
        key: &str,
        value: serde_json::Value,
        expected_revision: Option<u64>,
    ) -> PortResult<MetadataEntry> {
        validate_scope(scope)?;
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| internal("metadata lock poisoned"))?;
        let map_key = (scope_key(scope), key.to_string());
        let current = guard.get(&map_key).cloned();
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
            (Some(existing), Some(_)) => existing.revision + 1,
            (Some(existing), None) => existing.revision + 1,
        };
        let entry = MetadataEntry {
            key: key.to_string(),
            revision,
            etag: entry_etag(key, revision, &value),
            value,
        };
        guard.insert(map_key, entry.clone());
        Ok(entry)
    }

    fn get(&self, scope: &ScopeRef, key: &str) -> PortResult<Option<MetadataEntry>> {
        validate_scope(scope)?;
        let guard = self
            .entries
            .lock()
            .map_err(|_| internal("metadata lock poisoned"))?;
        Ok(guard.get(&(scope_key(scope), key.to_string())).cloned())
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
        let guard = self
            .entries
            .lock()
            .map_err(|_| internal("metadata lock poisoned"))?;
        let key = scope_key(scope);
        let mut keys: Vec<&String> = guard
            .keys()
            .filter(|(existing, entry_key)| *existing == key && entry_key.starts_with(prefix))
            .map(|(_, entry_key)| entry_key)
            .collect();
        keys.sort();
        let start = match cursor {
            Some(cursor) => keys
                .iter()
                .position(|candidate| candidate.as_str() > cursor)
                .unwrap_or(keys.len()),
            None => 0,
        };
        let page: Vec<MetadataEntry> = keys
            .into_iter()
            .skip(start)
            .take(limit)
            .filter_map(|entry_key| guard.get(&(key.clone(), entry_key.clone())).cloned())
            .collect();
        let next = if page.len() == limit {
            page.last().map(|entry| entry.key.clone())
        } else {
            None
        };
        Ok((page, next))
    }

    fn put_manifest(&self, scope: &ScopeRef, name: &str, generation: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .manifests
            .lock()
            .map_err(|_| internal("manifest lock poisoned"))?;
        guard.insert((scope_key(scope), name.to_string()), generation.to_string());
        Ok(())
    }

    fn get_manifest(&self, scope: &ScopeRef, name: &str) -> PortResult<Option<String>> {
        validate_scope(scope)?;
        let guard = self
            .manifests
            .lock()
            .map_err(|_| internal("manifest lock poisoned"))?;
        Ok(guard.get(&(scope_key(scope), name.to_string())).cloned())
    }

    fn acquire_writer(&self, scope: &ScopeRef, aggregate: &str, token: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .writers
            .lock()
            .map_err(|_| internal("writer lock poisoned"))?;
        let key = (scope_key(scope), aggregate.to_string());
        match guard.get(&key) {
            Some(existing) if existing != token => Err(StorageError::Conflict {
                collection: "writer_fence",
                key: aggregate.to_string(),
                expected_revision: 0,
                actual_revision: 1,
            }),
            _ => {
                guard.insert(key, token.to_string());
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Full text (BM25)
// ---------------------------------------------------------------------------

/// In-memory BM25 full-text index (§24.2 constants).
#[derive(Default)]
pub struct InMemoryFullText {
    /// (scope, analyzer) -> id -> doc
    docs: Mutex<HashMap<(String, String), BTreeMap<String, FullTextDoc>>>,
}

impl InMemoryFullText {
    /// New empty index.
    pub fn new() -> Self {
        Self::default()
    }
}

/// BM25 `k1` per §29.2.
pub const BM25_K1: f64 = 1.5;
/// BM25 `b` per §29.2.
pub const BM25_B: f64 = 0.75;

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

impl FullTextPort for InMemoryFullText {
    fn index(&self, scope: &ScopeRef, analyzer: &str, doc: &FullTextDoc) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .docs
            .lock()
            .map_err(|_| internal("fts lock poisoned"))?;
        guard
            .entry((scope_key(scope), analyzer.to_string()))
            .or_default()
            .insert(doc.id.clone(), doc.clone());
        Ok(())
    }

    fn delete(&self, scope: &ScopeRef, analyzer: &str, id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .docs
            .lock()
            .map_err(|_| internal("fts lock poisoned"))?;
        if let Some(docs) = guard.get_mut(&(scope_key(scope), analyzer.to_string())) {
            docs.remove(id);
        }
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
        if limit == 0 {
            return Err(StorageError::InvalidArgument {
                field: "limit",
                reason: "must be >= 1".into(),
            });
        }
        let guard = self
            .docs
            .lock()
            .map_err(|_| internal("fts lock poisoned"))?;
        let Some(docs) = guard.get(&(scope_key(scope), analyzer.to_string())) else {
            return Ok(Vec::new());
        };
        let terms = tokenize(query);
        if terms.is_empty() || docs.is_empty() {
            return Ok(Vec::new());
        }

        let field_of = |doc: &FullTextDoc| -> String {
            match field {
                Some(name) => doc.fields.get(name).cloned().unwrap_or_default(),
                None => doc.fields.values().cloned().collect::<Vec<_>>().join(" "),
            }
        };
        let tokenized: BTreeMap<&String, Vec<String>> = docs
            .iter()
            .map(|(id, doc)| (id, tokenize(&field_of(doc))))
            .collect();
        let total_len: usize = tokenized.values().map(Vec::len).sum();
        let doc_count = tokenized.len();
        let average_length = if doc_count == 0 {
            0.0
        } else {
            total_len as f64 / doc_count as f64
        };

        let mut hits: Vec<FullTextHit> = Vec::new();
        for (id, tokens) in &tokenized {
            let doc_len = tokens.len() as f64;
            let mut score = 0.0;
            for term in &terms {
                let frequency = tokens.iter().filter(|token| *token == term).count() as f64;
                if frequency == 0.0 {
                    continue;
                }
                let containing = tokenized
                    .values()
                    .filter(|candidate| candidate.iter().any(|token| token == term))
                    .count() as f64;
                let idf = ((doc_count as f64 - containing + 0.5) / (containing + 0.5) + 1.0).ln();
                let denominator = frequency
                    + BM25_K1 * (1.0 - BM25_B + BM25_B * doc_len / average_length.max(1.0));
                score += idf * frequency * (BM25_K1 + 1.0) / denominator;
            }
            if score > 0.0 {
                hits.push(FullTextHit {
                    id: (*id).clone(),
                    score,
                    revision: docs
                        .get(*id)
                        .map(|doc| doc.revision.clone())
                        .unwrap_or_default(),
                });
            }
        }
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.id.cmp(&right.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn analyzers(&self) -> Vec<String> {
        vec!["icu-v1".to_string(), "cjk-jieba-v1".to_string()]
    }
}

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

/// Durable-outbox semantics in memory: at-least-once with per-consumer dedup.
#[derive(Default)]
pub struct InMemoryOutbox {
    events: Mutex<HashMap<String, Vec<OutboxEvent>>>,
    acked: Mutex<HashSet<(String, String, String)>>,
    dead: Mutex<DeadLetterMap>,
}

impl InMemoryOutbox {
    /// New empty outbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total enqueued events in scope (test helper).
    pub fn len(&self, scope: &ScopeRef) -> usize {
        self.events
            .lock()
            .map(|guard| guard.get(&scope_key(scope)).map(Vec::len).unwrap_or(0))
            .unwrap_or(0)
    }

    /// Whether the outbox holds no events in scope.
    pub fn is_empty(&self, scope: &ScopeRef) -> bool {
        self.len(scope) == 0
    }
}

impl OutboxPort for InMemoryOutbox {
    fn enqueue(&self, scope: &ScopeRef, event: OutboxEvent) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .events
            .lock()
            .map_err(|_| internal("outbox lock poisoned"))?;
        let bucket = guard.entry(scope_key(scope)).or_default();
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
        let key = scope_key(scope);
        let events = self
            .events
            .lock()
            .map_err(|_| internal("outbox lock poisoned"))?;
        let acked = self
            .acked
            .lock()
            .map_err(|_| internal("outbox lock poisoned"))?;
        let bucket = events.get(&key).cloned().unwrap_or_default();
        Ok(bucket
            .into_iter()
            .filter(|event| {
                !acked.contains(&(key.clone(), consumer.to_string(), event.event_id.clone()))
            })
            .take(limit)
            .collect())
    }

    fn ack(&self, scope: &ScopeRef, consumer: &str, event_id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .acked
            .lock()
            .map_err(|_| internal("outbox lock poisoned"))?;
        guard.insert((scope_key(scope), consumer.to_string(), event_id.to_string()));
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
        let mut guard = self
            .dead
            .lock()
            .map_err(|_| internal("outbox lock poisoned"))?;
        guard
            .entry((scope_key(scope), consumer.to_string()))
            .or_default()
            .push((event_id.to_string(), reason.to_string()));
        Ok(())
    }

    fn dead_letters(&self, scope: &ScopeRef, consumer: &str) -> PortResult<Vec<String>> {
        validate_scope(scope)?;
        let guard = self
            .dead
            .lock()
            .map_err(|_| internal("outbox lock poisoned"))?;
        Ok(guard
            .get(&(scope_key(scope), consumer.to_string()))
            .map(|entries| entries.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Vector
// ---------------------------------------------------------------------------

/// Exact k-NN vector store with profile isolation.
#[derive(Default)]
pub struct InMemoryVector {
    profiles: Mutex<HashMap<(String, String), EmbeddingProfile>>,
    items: Mutex<HashMap<(String, String), BTreeMap<String, VectorItem>>>,
}

impl InMemoryVector {
    /// New empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

fn similarity(metric: Metric, left: &[f32], right: &[f32]) -> f64 {
    match metric {
        Metric::Cosine => {
            let mut dot = 0.0f64;
            let mut left_norm = 0.0f64;
            let mut right_norm = 0.0f64;
            for (a, b) in left.iter().zip(right.iter()) {
                dot += f64::from(*a) * f64::from(*b);
                left_norm += f64::from(*a) * f64::from(*a);
                right_norm += f64::from(*b) * f64::from(*b);
            }
            if left_norm == 0.0 || right_norm == 0.0 {
                0.0
            } else {
                dot / (left_norm.sqrt() * right_norm.sqrt())
            }
        }
        Metric::Dot => left
            .iter()
            .zip(right.iter())
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum(),
        Metric::L2 => {
            let sum: f64 = left
                .iter()
                .zip(right.iter())
                .map(|(a, b)| {
                    let delta = f64::from(*a) - f64::from(*b);
                    delta * delta
                })
                .sum();
            sum.sqrt()
        }
    }
}

impl VectorPort for InMemoryVector {
    fn declare_profile(&self, scope: &ScopeRef, profile: &EmbeddingProfile) -> PortResult<()> {
        validate_scope(scope)?;
        if profile.dimension == 0 {
            return Err(StorageError::InvalidArgument {
                field: "dimension",
                reason: "must be >= 1".into(),
            });
        }
        let mut guard = self
            .profiles
            .lock()
            .map_err(|_| internal("vector lock poisoned"))?;
        let key = (scope_key(scope), profile.profile_id.clone());
        if let Some(existing) = guard.get(&key) {
            if existing.dimension != profile.dimension || existing.metric != profile.metric {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "profile `{}` already declared with dimension {} / {:?}",
                        profile.profile_id, existing.dimension, existing.metric
                    ),
                });
            }
        }
        guard.insert(key, profile.clone());
        Ok(())
    }

    fn profile(&self, scope: &ScopeRef, profile_id: &str) -> PortResult<Option<EmbeddingProfile>> {
        validate_scope(scope)?;
        let guard = self
            .profiles
            .lock()
            .map_err(|_| internal("vector lock poisoned"))?;
        Ok(guard
            .get(&(scope_key(scope), profile_id.to_string()))
            .cloned())
    }

    fn upsert(&self, scope: &ScopeRef, profile_id: &str, items: &[VectorItem]) -> PortResult<()> {
        validate_scope(scope)?;
        let profile =
            self.profile(scope, profile_id)?
                .ok_or_else(|| StorageError::ModelProfileMismatch {
                    detail: format!("profile `{profile_id}` is not declared in scope"),
                })?;
        for item in items {
            if item.vector.len() != profile.dimension {
                return Err(StorageError::ModelProfileMismatch {
                    detail: format!(
                        "item `{}` has dimension {} but profile `{}` requires {}",
                        item.id,
                        item.vector.len(),
                        profile_id,
                        profile.dimension
                    ),
                });
            }
        }
        let mut guard = self
            .items
            .lock()
            .map_err(|_| internal("vector lock poisoned"))?;
        let bucket = guard
            .entry((scope_key(scope), profile_id.to_string()))
            .or_default();
        for item in items {
            bucket.insert(item.id.clone(), item.clone());
        }
        Ok(())
    }

    fn delete(&self, scope: &ScopeRef, profile_id: &str, id: &str) -> PortResult<()> {
        validate_scope(scope)?;
        let mut guard = self
            .items
            .lock()
            .map_err(|_| internal("vector lock poisoned"))?;
        if let Some(bucket) = guard.get_mut(&(scope_key(scope), profile_id.to_string())) {
            bucket.remove(id);
        }
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
        if k == 0 {
            return Err(StorageError::InvalidArgument {
                field: "k",
                reason: "must be >= 1".into(),
            });
        }
        let profile =
            self.profile(scope, profile_id)?
                .ok_or_else(|| StorageError::ModelProfileMismatch {
                    detail: format!("profile `{profile_id}` is not declared in scope"),
                })?;
        if query.len() != profile.dimension {
            return Err(StorageError::ModelProfileMismatch {
                detail: format!(
                    "query dimension {} != profile `{profile_id}` dimension {}",
                    query.len(),
                    profile.dimension
                ),
            });
        }
        let guard = self
            .items
            .lock()
            .map_err(|_| internal("vector lock poisoned"))?;
        let bucket = match guard.get(&(scope_key(scope), profile_id.to_string())) {
            Some(bucket) => bucket,
            None => return Ok(Vec::new()),
        };
        let mut hits: Vec<VectorHit> = bucket
            .values()
            .filter(|item| match filter {
                Some(required) => required
                    .iter()
                    .all(|(key, value)| item.metadata.get(key) == Some(value)),
                None => true,
            })
            .map(|item| VectorHit {
                id: item.id.clone(),
                score: similarity(profile.metric, query, &item.vector),
                revision: item.revision.clone(),
            })
            .collect();
        // L2 is a distance (lower is better); cosine/dot are similarities.
        match profile.metric {
            Metric::L2 => hits.sort_by(|left, right| {
                left.score
                    .partial_cmp(&right.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.id.cmp(&right.id))
            }),
            _ => hits.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.id.cmp(&right.id))
            }),
        }
        hits.truncate(k);
        Ok(hits)
    }
}

// ---------------------------------------------------------------------------
// Capability probe for the in-memory reference adapter
// ---------------------------------------------------------------------------

impl CapabilityProbe for InMemoryMetadata {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: "memory-metadata".to_string(),
            adapter_version: "0.1.0".to_string(),
            backend_kind: BackendKind::Embedded,
            logical_contract_version: "1.2.3".to_string(),
            adapter_schema_version: "1".to_string(),
            supported_ports: vec![
                "MetadataPort".to_string(),
                "ArtifactPort".to_string(),
                "FullTextPort".to_string(),
                "VectorPort".to_string(),
                "OutboxPort".to_string(),
                "GraphPort".to_string(),
            ],
            transaction_scope: TransactionScope::MultiAggregate,
            atomic_batch: true,
            compare_and_swap: true,
            snapshot_reads: true,
            acl_filtering: true,
            stable_cursor: true,
            native_graph_traversal: true,
            vector_filters: true,
            fulltext_analyzers: vec!["icu-v1".to_string()],
            durable_outbox: false,
            backup_restore: false,
            max_batch_size: 1000,
            available: true,
            unavailable_reason: String::new(),
        }
    }
}
