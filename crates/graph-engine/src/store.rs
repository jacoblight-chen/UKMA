//! The UKMA `GraphStore` port (gap report §Gap 1): a database-agnostic,
//! scope-keyed storage contract. No database dependency exists or may exist
//! in this crate (ADR-021..024; Database-Agnostic rule).

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ids::CanonicalId;
use crate::types::{GraphEdge, GraphNode};

/// Tenant/project scope key (mirrors `ukma_contracts::ScopeRef`'s required
/// fields; the engine validates non-emptiness).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeKey {
    /// Tenant identifier.
    pub tenant_id: String,
    /// Project identifier.
    pub project_id: String,
}

impl ScopeKey {
    /// New scope key (validated by the engine, not here).
    pub fn new(tenant_id: impl Into<String>, project_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            project_id: project_id.into(),
        }
    }
}

/// Probed capabilities of a store (ADR-022: values from real probes, never
/// config claims).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCapabilities {
    /// Writes survive process restart.
    pub persistent: bool,
    /// Batches are applied atomically.
    pub atomic_batches: bool,
}

/// Database-agnostic graph storage port.
///
/// Contract:
/// - every record is keyed by `(scope, canonical id)` — tenant isolation is a
///   storage invariant, not an afterthought;
/// - reads of a foreign scope yield `None` (existence is never leaked);
/// - iteration is deterministic (sorted canonical ids);
/// - `delete_node` cascades to incident edges (preserved upstream semantics);
/// - `flush` on a non-persistent store yields `CAPABILITY_UNAVAILABLE`.
pub trait GraphStore: Send + Sync {
    /// Probed capabilities of this store.
    fn capabilities(&self) -> StoreCapabilities;

    /// Upsert a node (blind replace at the storage level; concurrency is
    /// enforced by the engine via revision/ETag).
    fn put_node(&self, scope: &ScopeKey, node: &GraphNode) -> Result<()>;

    /// Fetch a node in scope; `None` when absent (including foreign scopes).
    fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphNode>>;

    /// Delete a node and all incident edges (cascading — upstream semantics).
    fn delete_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()>;

    /// All node ids in scope, sorted (deterministic).
    fn node_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>>;

    /// Upsert an edge.
    fn put_edge(&self, scope: &ScopeKey, edge: &GraphEdge) -> Result<()>;

    /// Fetch an edge in scope; `None` when absent.
    fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphEdge>>;

    /// Delete an edge (idempotent at the storage level).
    fn delete_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()>;

    /// All edge ids in scope, sorted (deterministic).
    fn edge_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>>;

    /// Ids of edges outgoing from `node`, sorted (deterministic).
    fn outgoing_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>>;

    /// Ids of edges incoming to `node`, sorted (deterministic).
    fn incoming_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>>;

    /// Durable flush. Non-persistent stores must return
    /// `CAPABILITY_UNAVAILABLE` (never a silent success).
    fn flush(&self) -> Result<()>;
}
