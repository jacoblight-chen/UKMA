//! The five non-graph storage ports (§6.2) plus the graph port re-export.
//!
//! Every port is scope-keyed (`tenant_id`/`project_id` at minimum), is
//! interior-mutable (`&self`) so adapters can be shared behind `Arc<dyn _>`,
//! and MUST report capability gaps instead of degrading silently.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ukma_contracts::ScopeRef;

use crate::error::{PortResult, StorageError};

/// The graph port is the S1 `GraphStore` contract — one contract, one name
/// (ADR-022). `graph_engine::GraphStore` is what adapters implement.
pub use graph_engine::GraphStore as GraphPort;

/// Reject scopes that cannot isolate data.
pub fn validate_scope(scope: &ScopeRef) -> PortResult<()> {
    if scope.tenant_id.trim().is_empty() {
        return Err(StorageError::InvalidArgument {
            field: "tenant_id",
            reason: "must be non-empty".into(),
        });
    }
    if scope.project_id.trim().is_empty() {
        return Err(StorageError::InvalidArgument {
            field: "project_id",
            reason: "must be non-empty".into(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Artifact port — immutable, content-addressed, tamper-evident (§6.2, §30)
// ---------------------------------------------------------------------------

/// Retention policy of a stored artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    /// Retained until explicitly deleted by policy.
    KeepForever,
    /// Retained until the given RFC3339 instant.
    ExpireAt(String),
}

/// Metadata of a stored artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// `sha256:<hex>` content address (§31.3 `Hash`).
    pub hash: String,
    /// Declared media type.
    pub media_type: String,
    /// Exact byte length.
    pub byte_len: u64,
    /// Retention policy.
    pub retention: Retention,
}

/// Immutable artifact storage.
pub trait ArtifactPort: Send + Sync {
    /// Store bytes; identical bytes always map to the same hash (idempotent).
    fn put_immutable(
        &self,
        scope: &ScopeRef,
        bytes: &[u8],
        media_type: &str,
        retention: Retention,
    ) -> PortResult<ArtifactRecord>;

    /// Read bytes back by content address.
    fn get(&self, scope: &ScopeRef, hash: &str) -> PortResult<Vec<u8>>;

    /// Recompute the hash and compare (tamper detection).
    fn verify(&self, scope: &ScopeRef, hash: &str) -> PortResult<bool>;

    /// Whether the artifact exists in scope.
    fn exists(&self, scope: &ScopeRef, hash: &str) -> PortResult<bool>;
}

// ---------------------------------------------------------------------------
// Metadata port — CAS, manifests, single-writer fencing (§6.2, §40.5)
// ---------------------------------------------------------------------------

/// One versioned metadata record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataEntry {
    /// Logical key inside the scope.
    pub key: String,
    /// Monotonic revision (starts at 1).
    pub revision: u64,
    /// Opaque ETag of this revision.
    pub etag: String,
    /// Stored JSON value.
    pub value: serde_json::Value,
}

/// Authoritative metadata store; also owns generation manifests.
pub trait MetadataPort: Send + Sync {
    /// Compare-and-swap write. `None` means "create only".
    fn put_cas(
        &self,
        scope: &ScopeRef,
        key: &str,
        value: serde_json::Value,
        expected_revision: Option<u64>,
    ) -> PortResult<MetadataEntry>;

    /// Read one record.
    fn get(&self, scope: &ScopeRef, key: &str) -> PortResult<Option<MetadataEntry>>;

    /// List records by key prefix, stable order `(key asc)`, cursor-bound paging.
    fn list(
        &self,
        scope: &ScopeRef,
        prefix: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> PortResult<(Vec<MetadataEntry>, Option<String>)>;

    /// Atomically point a manifest name at a generation (§36.3).
    fn put_manifest(&self, scope: &ScopeRef, name: &str, generation: &str) -> PortResult<()>;

    /// Read the currently published generation for a manifest name.
    fn get_manifest(&self, scope: &ScopeRef, name: &str) -> PortResult<Option<String>>;

    /// Acquire/renew the single-writer fencing token of one aggregate (§40.5).
    /// A different live token for the same aggregate is rejected.
    fn acquire_writer(&self, scope: &ScopeRef, aggregate: &str, token: &str) -> PortResult<()>;
}

// ---------------------------------------------------------------------------
// Full-text port — fielded search, analyzer-versioned (§6.2, §24.2)
// ---------------------------------------------------------------------------

/// One indexed document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullTextDoc {
    /// Logical document id.
    pub id: String,
    /// Source revision this document was built from.
    pub revision: String,
    /// Field name -> text.
    pub fields: BTreeMap<String, String>,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FullTextHit {
    /// Document id.
    pub id: String,
    /// BM25 score (raw; never compared across analyzers, §24.2).
    pub score: f64,
    /// Source revision of the hit.
    pub revision: String,
}

/// Full-text index.
pub trait FullTextPort: Send + Sync {
    /// Index or replace a document under a specific analyzer version.
    fn index(&self, scope: &ScopeRef, analyzer: &str, doc: &FullTextDoc) -> PortResult<()>;

    /// Remove a document.
    fn delete(&self, scope: &ScopeRef, analyzer: &str, id: &str) -> PortResult<()>;

    /// Fielded search; deterministic ordering `(score desc, id asc)`.
    fn search(
        &self,
        scope: &ScopeRef,
        analyzer: &str,
        field: Option<&str>,
        query: &str,
        limit: usize,
    ) -> PortResult<Vec<FullTextHit>>;

    /// Analyzer versions this adapter can serve.
    fn analyzers(&self) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// Outbox port — durable at-least-once with per-consumer dedup (§6.2, §40.6)
// ---------------------------------------------------------------------------

/// One durable event awaiting projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxEvent {
    /// Globally unique event id (dedup key).
    pub event_id: String,
    /// Registered event type (§40.6).
    pub event_type: String,
    /// Event schema version.
    pub event_version: u32,
    /// Aggregate this event belongs to.
    pub aggregate_ref: String,
    /// Aggregate revision after the change.
    pub aggregate_revision: u64,
    /// Closed payload (§40.6: never raw sensitive content).
    pub payload: serde_json::Value,
    /// Idempotency key of the originating operation.
    pub idempotency_key: Option<String>,
}

/// Durable outbox.
pub trait OutboxPort: Send + Sync {
    /// Enqueue; replaying the same `event_id` is a no-op.
    fn enqueue(&self, scope: &ScopeRef, event: OutboxEvent) -> PortResult<()>;

    /// Pending events for a consumer, ordered by insertion. Events already
    /// acked by this consumer are excluded, so delivery is at-least-once with
    /// `(event_id, consumer)` dedup.
    fn pending(
        &self,
        scope: &ScopeRef,
        consumer: &str,
        limit: usize,
    ) -> PortResult<Vec<OutboxEvent>>;

    /// Acknowledge successful projection by one consumer.
    fn ack(&self, scope: &ScopeRef, consumer: &str, event_id: &str) -> PortResult<()>;

    /// Move an event to the dead-letter list for a consumer.
    fn dead_letter(
        &self,
        scope: &ScopeRef,
        consumer: &str,
        event_id: &str,
        reason: &str,
    ) -> PortResult<()>;

    /// Dead-lettered event ids for a consumer.
    fn dead_letters(&self, scope: &ScopeRef, consumer: &str) -> PortResult<Vec<String>>;
}

// ---------------------------------------------------------------------------
// Vector port — profile-isolated ANN (§6.2, §25.2, DB-004)
// ---------------------------------------------------------------------------

/// Distance metric of a vector space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Cosine similarity.
    Cosine,
    /// Dot product.
    Dot,
    /// Euclidean distance (lower is closer).
    L2,
}

/// Embedding profile identity; vectors are only comparable inside one profile
/// (PROP-07).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProfile {
    /// Stable profile id.
    pub profile_id: String,
    /// Model identity.
    pub model_id: String,
    /// Model revision.
    pub model_revision: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Metric.
    pub metric: Metric,
    /// Normalization policy (e.g. `l2`).
    pub normalization: String,
}

/// One stored vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorItem {
    /// Logical item id.
    pub id: String,
    /// Source revision.
    pub revision: String,
    /// Dense vector; length MUST equal the profile dimension.
    pub vector: Vec<f32>,
    /// Metadata used for filtering.
    pub metadata: BTreeMap<String, String>,
}

/// One vector hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorHit {
    /// Item id.
    pub id: String,
    /// Similarity score (higher is better for cosine/dot).
    pub score: f64,
    /// Source revision.
    pub revision: String,
}

/// Vector index.
pub trait VectorPort: Send + Sync {
    /// Register a profile in scope; re-declaring the same id is idempotent.
    fn declare_profile(&self, scope: &ScopeRef, profile: &EmbeddingProfile) -> PortResult<()>;

    /// Look up a declared profile.
    fn profile(&self, scope: &ScopeRef, profile_id: &str) -> PortResult<Option<EmbeddingProfile>>;

    /// Upsert vectors; a dimension/metric mismatch is `MODEL_PROFILE_MISMATCH`.
    fn upsert(&self, scope: &ScopeRef, profile_id: &str, items: &[VectorItem]) -> PortResult<()>;

    /// Delete one vector.
    fn delete(&self, scope: &ScopeRef, profile_id: &str, id: &str) -> PortResult<()>;

    /// k-NN search inside a single profile, optionally metadata-filtered.
    fn search(
        &self,
        scope: &ScopeRef,
        profile_id: &str,
        query: &[f32],
        k: usize,
        filter: Option<&BTreeMap<String, String>>,
    ) -> PortResult<Vec<VectorHit>>;
}
