//! UKMA storage ports (§6.2) and adapter capability negotiation (§6.3).
//!
//! **Scope of this crate:** contracts only — no database dependency, no vendor
//! SDK (ADR-021). Adapters live in `crates/adapters/*`.
//!
//! Six ports, one contract each:
//! - [`GraphPort`] — re-exported from `graph_engine::GraphStore` so there is
//!   exactly one graph contract in the workspace (ADR-022).
//! - [`ArtifactPort`] — immutable, content-addressed (`sha256:<hex>`).
//! - [`MetadataPort`] — CAS, manifests (atomic generation pointer) and
//!   single-writer fencing.
//! - [`FullTextPort`] — fielded BM25 search, analyzer-versioned.
//! - [`OutboxPort`] — durable at-least-once with `(event_id, consumer)` dedup.
//! - [`VectorPort`] — profile-isolated k-NN; dimension/metric mismatches are
//!   rejected with `MODEL_PROFILE_MISMATCH` (DB-004, PROP-07).
//!
//! Capability negotiation is fail-closed: an adapter lacking a capability the
//! minimum profile requires must refuse to start (§6.3, §40.5, ADR-038).

pub mod capabilities;
pub mod error;
pub mod migration;
pub mod migrations;
pub mod ports;
pub mod snapshot;
pub mod testing;

pub use capabilities::{AdapterCapabilities, BackendKind, CapabilityProbe, TransactionScope};
pub use error::{io_error, PortResult, StorageError};
pub use migration::{
    migrate, Migration, MigrationReport, MigrationStore, SchemaVersionPort,
    CANONICAL_SCHEMA_VERSION,
};
pub use migrations::canonical_migrations;
pub use ports::{
    validate_scope, ArtifactPort, ArtifactRecord, EmbeddingProfile, FullTextDoc, FullTextHit,
    FullTextPort, GraphPort, MetadataEntry, MetadataPort, Metric, OutboxEvent, OutboxPort,
    Retention, VectorHit, VectorItem, VectorPort,
};
pub use snapshot::{
    cache_db_error, cache_io_error, scope_fingerprint, sha256_hex, CacheError, FileRow, ParseCache,
    ParseKey, SnapshotMeta, SnapshotStore, SNAPSHOT_FORMAT, SNAPSHOT_SCHEMA_VERSION,
};
