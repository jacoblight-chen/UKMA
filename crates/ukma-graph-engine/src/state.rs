//! Shared mutable state for the store adapters (in-memory + file-backed).
//!
//! Database-agnostic: no database dependency. `StoreInner` holds the hot
//! HashMaps; `Snapshot` is the serde-serializable form used by the file
//! adapter's atomic flush/load.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::CanonicalId;
use crate::store::ScopeKey;
use crate::types::{GraphEdge, GraphNode};

type NodeKey = (ScopeKey, CanonicalId);
type EdgeKey = (ScopeKey, CanonicalId);

/// Serialized store state (file adapter persistence format, v1).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// format tag for forward compatibility.
    pub format: u32,
    /// (scope, node) pairs.
    pub nodes: Vec<(ScopeKey, GraphNode)>,
    /// (scope, edge) pairs.
    pub edges: Vec<(ScopeKey, GraphEdge)>,
}

/// Current persistence format version.
pub const SNAPSHOT_FORMAT_V1: u32 = 1;

fn sorted<T: Ord>(v: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut v: Vec<T> = v.into_iter().collect();
    v.sort();
    v
}

/// Shared interior state (interior-mutable under a `Mutex` in each adapter).
#[derive(Default)]
pub struct StoreInner {
    nodes: HashMap<NodeKey, GraphNode>,
    edges: HashMap<EdgeKey, GraphEdge>,
    /// node id -> outgoing edge ids.
    outgoing: HashMap<NodeKey, Vec<CanonicalId>>,
    /// node id -> incoming edge ids.
    incoming: HashMap<NodeKey, Vec<CanonicalId>>,
}

impl StoreInner {
    /// Number of nodes in scope.
    pub fn node_count(&self, scope: &ScopeKey) -> usize {
        self.nodes.iter().filter(|((s, _), _)| *s == *scope).count()
    }

    /// Number of edges in scope.
    pub fn edge_count(&self, scope: &ScopeKey) -> usize {
        self.edges.iter().filter(|((s, _), _)| *s == *scope).count()
    }

    /// Upsert a node (blind replace; concurrency enforced by the engine).
    pub fn put_node(&mut self, scope: &ScopeKey, node: &GraphNode) {
        self.nodes
            .insert((scope.clone(), node.id.clone()), node.clone());
    }

    /// Fetch a node in scope.
    pub fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Option<GraphNode> {
        self.nodes.get(&(scope.clone(), id.clone())).cloned()
    }

    /// Delete a node and all incident edges (cascade — preserved upstream
    /// semantics). Idempotent at the storage level.
    pub fn delete_node(&mut self, scope: &ScopeKey, id: &CanonicalId) {
        if self.nodes.remove(&(scope.clone(), id.clone())).is_none() {
            return;
        }
        let incident: Vec<CanonicalId> = self
            .outgoing
            .get(&(scope.clone(), id.clone()))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .chain(
                self.incoming
                    .get(&(scope.clone(), id.clone()))
                    .cloned()
                    .unwrap_or_default(),
            )
            .collect();
        for edge_id in incident {
            if let Some(edge) = self.edges.remove(&(scope.clone(), edge_id.clone())) {
                if let Some(list) = self
                    .outgoing
                    .get_mut(&(scope.clone(), edge.source_id.clone()))
                {
                    list.retain(|e| e != &edge_id);
                }
                if let Some(list) = self
                    .incoming
                    .get_mut(&(scope.clone(), edge.target_id.clone()))
                {
                    list.retain(|e| e != &edge_id);
                }
            }
        }
        self.outgoing.remove(&(scope.clone(), id.clone()));
        self.incoming.remove(&(scope.clone(), id.clone()));
    }

    /// All node ids in scope, sorted (deterministic).
    pub fn node_ids(&self, scope: &ScopeKey) -> Vec<CanonicalId> {
        sorted(
            self.nodes
                .iter()
                .filter(|((s, _), _)| *s == *scope)
                .map(|((_, id), _)| id.clone()),
        )
    }

    /// Upsert an edge and update adjacency lists.
    pub fn put_edge(&mut self, scope: &ScopeKey, edge: &GraphEdge) {
        self.edges
            .insert((scope.clone(), edge.id.clone()), edge.clone());
        self.outgoing
            .entry((scope.clone(), edge.source_id.clone()))
            .or_default()
            .push(edge.id.clone());
        self.incoming
            .entry((scope.clone(), edge.target_id.clone()))
            .or_default()
            .push(edge.id.clone());
    }

    /// Fetch an edge in scope.
    pub fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Option<GraphEdge> {
        self.edges.get(&(scope.clone(), id.clone())).cloned()
    }

    /// Delete an edge and update adjacency lists (idempotent).
    pub fn delete_edge(&mut self, scope: &ScopeKey, id: &CanonicalId) {
        if let Some(edge) = self.edges.remove(&(scope.clone(), id.clone())) {
            if let Some(list) = self
                .outgoing
                .get_mut(&(scope.clone(), edge.source_id.clone()))
            {
                list.retain(|e| e != id);
            }
            if let Some(list) = self
                .incoming
                .get_mut(&(scope.clone(), edge.target_id.clone()))
            {
                list.retain(|e| e != id);
            }
        }
    }

    /// All edge ids in scope, sorted (deterministic).
    pub fn edge_ids(&self, scope: &ScopeKey) -> Vec<CanonicalId> {
        sorted(
            self.edges
                .iter()
                .filter(|((s, _), _)| *s == *scope)
                .map(|((_, id), _)| id.clone()),
        )
    }

    /// Ids of edges outgoing from `node`, sorted (deterministic).
    pub fn outgoing_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Vec<CanonicalId> {
        sorted(
            self.outgoing
                .get(&(scope.clone(), node.clone()))
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// Ids of edges incoming to `node`, sorted (deterministic).
    pub fn incoming_edge_ids(&self, scope: &ScopeKey, node: &CanonicalId) -> Vec<CanonicalId> {
        sorted(
            self.incoming
                .get(&(scope.clone(), node.clone()))
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// Snapshot for serialization (deterministic order: sorted by scope+id).
    pub fn snapshot(&self) -> Snapshot {
        let mut nodes: Vec<(ScopeKey, GraphNode)> = self
            .nodes
            .iter()
            .map(|((s, _), n)| (s.clone(), n.clone()))
            .collect();
        let mut edges: Vec<(ScopeKey, GraphEdge)> = self
            .edges
            .iter()
            .map(|((s, _), e)| (s.clone(), e.clone()))
            .collect();
        // ScopeKey has no Ord; sort by field strings + id (deterministic).
        nodes.sort_by(|a, b| {
            (
                a.0.tenant_id.as_str(),
                a.0.project_id.as_str(),
                a.1.id.as_str(),
            )
                .cmp(&(
                    b.0.tenant_id.as_str(),
                    b.0.project_id.as_str(),
                    b.1.id.as_str(),
                ))
        });
        edges.sort_by(|a, b| {
            (
                a.0.tenant_id.as_str(),
                a.0.project_id.as_str(),
                a.1.id.as_str(),
            )
                .cmp(&(
                    b.0.tenant_id.as_str(),
                    b.0.project_id.as_str(),
                    b.1.id.as_str(),
                ))
        });
        Snapshot {
            format: SNAPSHOT_FORMAT_V1,
            nodes,
            edges,
        }
    }

    /// Load a snapshot (replaces all state; rebuilds adjacency lists).
    pub fn load(&mut self, snap: &Snapshot) {
        self.nodes.clear();
        self.edges.clear();
        self.outgoing.clear();
        self.incoming.clear();
        for (scope, node) in &snap.nodes {
            self.nodes
                .insert((scope.clone(), node.id.clone()), node.clone());
        }
        for (scope, edge) in &snap.edges {
            self.edges
                .insert((scope.clone(), edge.id.clone()), edge.clone());
            self.outgoing
                .entry((scope.clone(), edge.source_id.clone()))
                .or_default()
                .push(edge.id.clone());
            self.incoming
                .entry((scope.clone(), edge.target_id.clone()))
                .or_default()
                .push(edge.id.clone());
        }
    }
}
