//! UKMA Phase 7 S1 — Graph Engine vertical slice.
//!
//! Contract-first native refactoring ported from CodeGraph Graph Core
//! (upstream SHA `590c2ca7dd7b37bd1d41caf45fac535b6522f969`, Apache-2.0;
//! attribution preserved per `docs/migration/license-audit.md` and the source
//! migration manifest in `docs/migration/source-map.yaml`).
//!
//! Guarantees (per ADR-021..024, ADR-029, ADR-031 and spec PROP-02/PROP-03):
//! - **Database-agnostic**: the `GraphStore` port has no dependency on any
//!   database. RocksDB, if ever added, is an optional feature-gated adapter
//!   outside the Domain — never a Domain dependency.
//! - **Canonical IDs** are engine-assigned, persisted, and stable across
//!   adapter substitution (ADR-031 D1); vendor `u64` counters are never exposed.
//! - **Traversal** is deterministic and bounded (PROP-03): sorted-adjacency
//!   ordering plus mandatory depth/node/path bounds.
//! - **Tenant isolation**: every record is keyed by `(tenant, project)`; a
//!   cross-scope read returns `NOT_FOUND` (existence is never leaked).
//! - **Versioning**: optimistic concurrency via revision + ETag
//!   (`ETAG_MISMATCH` on stale expected revision).
//! - **Capability negotiation**: operations the active store cannot provide
//!   return `CAPABILITY_UNAVAILABLE` (probed, never assumed).
//!
//! Ported algorithm semantics (bfs/dfs/Tarjan-SCC/find_all_paths) are preserved;
//! UKMA-mandated changes are recorded in the source migration manifest.

pub mod algorithms;
pub mod engine;
pub mod error;
pub mod ids;
pub mod memory_store;
pub mod store;
pub mod types;

pub use engine::GraphEngine;
pub use error::{GraphError, Result};
pub use ids::{
    CanonicalId, DefaultIdGenerator, DeterministicIdGenerator, IdGenerator, IdKind, Ulid,
};
pub use memory_store::InMemoryGraphStore;
pub use store::{GraphStore, ScopeKey, StoreCapabilities};
pub use types::{Direction, EdgeType, GraphEdge, GraphNode, NodeType, PropertyMap, PropertyValue};
