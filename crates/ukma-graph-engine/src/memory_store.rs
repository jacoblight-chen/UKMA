//! In-memory `GraphStore` adapter (test + development).
//!
//! Database-agnostic: no RocksDB, no filesystem, no external state. The
//! adapter is interior-mutable (`Mutex`) so the engine can share it via
//! `Arc<dyn GraphStore>`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{GraphError, Result};
use crate::ids::CanonicalId;
use crate::store::{GraphStore, ScopeKey, StoreCapabilities};
use crate::types::{GraphEdge, GraphNode};

type NodeKey = (ScopeKey, CanonicalId);
type EdgeKey = (ScopeKey, CanonicalId);

#[derive(Default)]
struct Inner {
    nodes: HashMap<NodeKey, GraphNode>,
    edges: HashMap<EdgeKey, GraphEdge>,
    /// node id -> outgoing edge ids.
    outgoing: HashMap<NodeKey, Vec<CanonicalId>>,
    /// node id -> incoming edge ids.
    incoming: HashMap<NodeKey, Vec<CanonicalId>>,
}

/// In-memory graph store (non-persistent).
#[derive(Default)]
pub struct InMemoryGraphStore {
    inner: Mutex<Inner>,
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
            .nodes
            .iter()
            .filter(|((s, _), _)| *s == *scope)
            .count()
    }

    /// Number of edges in scope (test helper).
    pub fn edge_count(&self, scope: &ScopeKey) -> usize {
        self.inner
            .lock()
            .expect("store lock poisoned")
            .edges
            .iter()
            .filter(|((s, _), _)| *s == *scope)
            .count()
    }
}

fn sorted<T: Ord>(v: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut v: Vec<T> = v.into_iter().collect();
    v.sort();
    v
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
            .nodes
            .insert((scope.clone(), node.id.clone()), node.clone());
        Ok(())
    }

    fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphNode>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .nodes
            .get(&(scope.clone(), id.clone()))
            .cloned())
    }

    fn delete_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        let mut inner = self.inner.lock().expect("store lock poisoned");
        if inner.nodes.remove(&(scope.clone(), id.clone())).is_none() {
            return Ok(()); // idempotent at the storage level
        }
        // Cascade: delete all incident edges (preserved upstream semantics).
        let incident: Vec<CanonicalId> = inner
            .outgoing
            .get(&(scope.clone(), id.clone()))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .chain(
                inner
                    .incoming
                    .get(&(scope.clone(), id.clone()))
                    .cloned()
                    .unwrap_or_default(),
            )
            .collect();
        for edge_id in incident {
            if let Some(edge) = inner.edges.remove(&(scope.clone(), edge_id.clone())) {
                if let Some(list) = inner
                    .outgoing
                    .get_mut(&(scope.clone(), edge.source_id.clone()))
                {
                    list.retain(|e| e != &edge_id);
                }
                if let Some(list) = inner
                    .incoming
                    .get_mut(&(scope.clone(), edge.target_id.clone()))
                {
                    list.retain(|e| e != &edge_id);
                }
            }
        }
        inner.outgoing.remove(&(scope.clone(), id.clone()));
        inner.incoming.remove(&(scope.clone(), id.clone()));
        Ok(())
    }

    fn node_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Ok(sorted(
            self.inner
                .lock()
                .expect("store lock poisoned")
                .nodes
                .iter()
                .filter(|((s, _), _)| *s == *scope)
                .map(|((_, id), _)| id.clone()),
        ))
    }

    fn put_edge(&self, scope: &ScopeKey, edge: &GraphEdge) -> Result<()> {
        let mut inner = self.inner.lock().expect("store lock poisoned");
        inner
            .edges
            .insert((scope.clone(), edge.id.clone()), edge.clone());
        inner
            .outgoing
            .entry((scope.clone(), edge.source_id.clone()))
            .or_default()
            .push(edge.id.clone());
        inner
            .incoming
            .entry((scope.clone(), edge.target_id.clone()))
            .or_default()
            .push(edge.id.clone());
        Ok(())
    }

    fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphEdge>> {
        Ok(self
            .inner
            .lock()
            .expect("store lock poisoned")
            .edges
            .get(&(scope.clone(), id.clone()))
            .cloned())
    }

    fn delete_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        let mut inner = self.inner.lock().expect("store lock poisoned");
        if let Some(edge) = inner.edges.remove(&(scope.clone(), id.clone())) {
            if let Some(list) = inner
                .outgoing
                .get_mut(&(scope.clone(), edge.source_id.clone()))
            {
                list.retain(|e| e != id);
            }
            if let Some(list) = inner
                .incoming
                .get_mut(&(scope.clone(), edge.target_id.clone()))
            {
                list.retain(|e| e != id);
            }
        }
        Ok(())
    }

    fn edge_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Ok(sorted(
            self.inner
                .lock()
                .expect("store lock poisoned")
                .edges
                .iter()
                .filter(|((s, _), _)| *s == *scope)
                .map(|((_, id), _)| id.clone()),
        ))
    }

    fn outgoing_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        Ok(sorted(
            self.inner
                .lock()
                .expect("store lock poisoned")
                .outgoing
                .get(&(scope.clone(), node.clone()))
                .cloned()
                .unwrap_or_default(),
        ))
    }

    fn incoming_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Result<Vec<CanonicalId>> {
        Ok(sorted(
            self.inner
                .lock()
                .expect("store lock poisoned")
                .incoming
                .get(&(scope.clone(), node.clone()))
                .cloned()
                .unwrap_or_default(),
        ))
    }

    fn flush(&self) -> Result<()> {
        // Non-persistent store: explicit capability refusal (ADR-022), never
        // a silent success.
        Err(GraphError::CapabilityUnavailable {
            capability: "durable_flush",
        })
    }
}
