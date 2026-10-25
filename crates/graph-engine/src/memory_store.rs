//! In-memory `GraphStore` adapter (test + development).
//!
//! Database-agnostic: no database, no filesystem, no external state. The
//! adapter is interior-mutable (`Mutex`) so the engine can share it via
//! `Arc<dyn GraphStore>`.

use std::sync::Mutex;

use crate::error::{GraphError, Result};
use crate::ids::CanonicalId;
use crate::state::StoreInner;
use crate::store::{GraphStore, ScopeKey, StoreCapabilities};
use crate::types::{GraphEdge, GraphNode};

/// In-memory graph store (non-persistent).
#[derive(Default)]
pub struct InMemoryGraphStore {
    inner: Mutex<StoreInner>,
}

impl InMemoryGraphStore {
    /// New empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of nodes in scope (test helper).
    pub fn node_count(&self, scope: &ScopeKey) -> usize {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .node_count(scope)
    }

    /// Number of edges in scope (test helper).
    pub fn edge_count(&self, scope: &ScopeKey) -> usize {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .edge_count(scope)
    }
}

impl GraphStore for InMemoryGraphStore {
    fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            persistent: false,
            atomic_batches: true,
        }
    }

    fn put_node(&self, scope: &ScopeKey, node: &GraphNode) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .put_node(scope, node);
        Ok(())
    }

    fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphNode>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .get_node(scope, id))
    }

    fn delete_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .delete_node(scope, id);
        Ok(())
    }

    fn node_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .node_ids(scope))
    }

    fn put_edge(&self, scope: &ScopeKey, edge: &GraphEdge) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .put_edge(scope, edge);
        Ok(())
    }

    fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphEdge>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .get_edge(scope, id))
    }

    fn delete_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .delete_edge(scope, id);
        Ok(())
    }

    fn edge_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .edge_ids(scope))
    }

    fn outgoing_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .outgoing_edge_ids(scope, node))
    }

    fn incoming_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .incoming_edge_ids(scope, node))
    }

    fn flush(&self) -> Result<()> {
        // Non-persistent store: explicit capability refusal (ADR-022), never
        // a silent success.
        Err(GraphError::CapabilityUnavailable {
            capability: "durable_flush",
        })
    }
}
