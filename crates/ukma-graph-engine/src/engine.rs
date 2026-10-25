//! The UKMA Graph Engine — public API (contract-first, database-agnostic).
//!
//! Wraps a `GraphStore` port with: scope validation, canonical id
//! assignment (ADR-031 D1), optimistic concurrency (revision + ETag),
//! idempotent mutations, cascading deletes (preserved upstream semantics),
//! bounded traversals (PROP-03), and explicit capability probes (ADR-022).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::{Arc, Mutex};

use crate::algorithms;
use crate::error::{GraphError, Result};
use crate::ids::{CanonicalId, DefaultIdGenerator, IdGenerator, IdKind};
use crate::memory_store::InMemoryGraphStore;
use crate::store::{GraphStore, ScopeKey, StoreCapabilities};
use crate::types::{
    compute_etag, Direction, EdgeType, GraphEdge, GraphNode, NodeType, PropertyMap,
};

/// Length-prefixed field writer (avoids separator ambiguity).
fn write_field(h: &mut DefaultHasher, s: &str) {
    h.write_u64(s.len() as u64);
    h.write(s.as_bytes());
}

/// Stable string form of a property value for fingerprinting.
fn value_str(v: &crate::types::PropertyValue) -> String {
    match v {
        crate::types::PropertyValue::String(s) => s.clone(),
        crate::types::PropertyValue::Int(i) => i.to_string(),
        crate::types::PropertyValue::Float(f) => f.to_bits().to_string(),
        crate::types::PropertyValue::Bool(b) => b.to_string(),
        crate::types::PropertyValue::StringList(l) => l.join("\u{1C}"),
        crate::types::PropertyValue::IntList(l) => l
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\u{1C}"),
        crate::types::PropertyValue::Null => "null".into(),
    }
}

fn fingerprint_node(t: &NodeType, props: &PropertyMap) -> u64 {
    let mut h = DefaultHasher::new();
    write_field(&mut h, t.wire_name());
    for (k, v) in props.iter_sorted() {
        write_field(&mut h, k);
        write_field(&mut h, &value_str(v));
    }
    h.finish()
}

fn fingerprint_edge(t: &EdgeType, s: &CanonicalId, d: &CanonicalId, props: &PropertyMap) -> u64 {
    let mut h = DefaultHasher::new();
    write_field(&mut h, t.wire_name());
    write_field(&mut h, s.as_str());
    write_field(&mut h, d.as_str());
    for (k, v) in props.iter_sorted() {
        write_field(&mut h, k);
        write_field(&mut h, &value_str(v));
    }
    h.finish()
}

/// The graph engine (shared, `Send + Sync`).
pub struct GraphEngine {
    store: Arc<dyn GraphStore>,
    id_gen: Mutex<Box<dyn IdGenerator>>,
    /// (scope, idempotency key) -> (payload fingerprint, entity id).
    idempotency: Mutex<HashMap<(ScopeKey, String), (u64, CanonicalId)>>,
}

impl GraphEngine {
    /// Engine backed by an in-memory store (test + development).
    pub fn new_memory() -> Self {
        Self::with_store(Arc::new(InMemoryGraphStore::new()))
    }

    /// Engine backed by an arbitrary `GraphStore` adapter.
    pub fn with_store(store: Arc<dyn GraphStore>) -> Self {
        Self {
            store,
            id_gen: Mutex::new(Box::new(DefaultIdGenerator::default())),
            idempotency: Mutex::new(HashMap::new()),
        }
    }

    /// Engine backed by an arbitrary `GraphStore` adapter and id generator
    /// (deterministic generators for contract tests).
    pub fn with_store_and_ids(store: Arc<dyn GraphStore>, id_gen: Box<dyn IdGenerator>) -> Self {
        Self {
            store,
            id_gen: Mutex::new(id_gen),
            idempotency: Mutex::new(HashMap::new()),
        }
    }

    fn validate_scope(scope: &ScopeKey) -> Result<()> {
        if scope.tenant_id.trim().is_empty() {
            return Err(GraphError::InvalidArgument {
                field: "tenant_id",
                reason: "must be non-empty".into(),
            });
        }
        if scope.project_id.trim().is_empty() {
            return Err(GraphError::InvalidArgument {
                field: "project_id",
                reason: "must be non-empty".into(),
            });
        }
        Ok(())
    }

    // ---------- nodes ----------

    /// Create a node (engine-assigned canonical id). Idempotent when a key is
    /// supplied: same key + same payload returns the original; same key +
    /// different payload yields `IDEMPOTENCY_CONFLICT`.
    pub fn add_node(
        &self,
        scope: &ScopeKey,
        node_type: NodeType,
        properties: PropertyMap,
        idempotency_key: Option<String>,
    ) -> Result<GraphNode> {
        Self::validate_scope(scope)?;
        if let Some(key) = &idempotency_key {
            let guard = self.idempotency.lock().expect("idem lock poisoned");
            if let Some((fp, id)) = guard.get(&(scope.clone(), key.clone())) {
                if *fp == fingerprint_node(&node_type, &properties) {
                    return self
                        .store
                        .get_node(scope, id)?
                        .ok_or_else(|| GraphError::NotFound {
                            entity: "node",
                            id: id.to_string(),
                        });
                }
                return Err(GraphError::IdempotencyConflict { id: id.to_string() });
            }
        }
        let id = self
            .id_gen
            .lock()
            .expect("id gen lock poisoned")
            .next(IdKind::Node);
        let node = GraphNode::new(id.clone(), node_type, properties.clone());
        self.store.put_node(scope, &node)?;
        if let Some(key) = idempotency_key {
            self.idempotency.lock().expect("idem lock poisoned").insert(
                (scope.clone(), key),
                (fingerprint_node(&node_type, &properties), id.clone()),
            );
        }
        Ok(node)
    }

    /// Fetch a node in scope. `NotFound` for absent or foreign-scope ids
    /// (existence never leaks).
    pub fn get_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<GraphNode> {
        Self::validate_scope(scope)?;
        self.store
            .get_node(scope, id)?
            .ok_or_else(|| GraphError::NotFound {
                entity: "node",
                id: id.to_string(),
            })
    }

    /// Replace a node's properties at an expected revision (optimistic
    /// concurrency; revision +1, ETag recomputed).
    pub fn update_node(
        &self,
        scope: &ScopeKey,
        id: &CanonicalId,
        expected_revision: u64,
        new_properties: PropertyMap,
    ) -> Result<GraphNode> {
        Self::validate_scope(scope)?;
        let current = self
            .store
            .get_node(scope, id)?
            .ok_or_else(|| GraphError::NotFound {
                entity: "node",
                id: id.to_string(),
            })?;
        if current.revision != expected_revision {
            return Err(GraphError::Conflict {
                entity: "node",
                id: id.to_string(),
                expected_revision,
                actual_revision: current.revision,
            });
        }
        let revision = current.revision + 1;
        let etag = compute_etag(revision, current.node_type.wire_name(), &new_properties);
        let node = GraphNode {
            id: current.id,
            node_type: current.node_type,
            properties: new_properties,
            revision,
            etag,
        };
        self.store.put_node(scope, &node)?;
        Ok(node)
    }

    /// Delete a node; incident edges are removed (cascading — preserved
    /// upstream semantics).
    pub fn delete_node(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        Self::validate_scope(scope)?;
        if self.store.get_node(scope, id)?.is_none() {
            return Err(GraphError::NotFound {
                entity: "node",
                id: id.to_string(),
            });
        }
        self.store.delete_node(scope, id)
    }

    /// All node ids in scope (sorted).
    pub fn node_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Self::validate_scope(scope)?;
        self.store.node_ids(scope)
    }

    /// Number of nodes in scope.
    pub fn node_count(&self, scope: &ScopeKey) -> Result<usize> {
        Self::validate_scope(scope)?;
        Ok(self.store.node_ids(scope)?.len())
    }

    // ---------- edges ----------

    /// Create an edge; endpoints must exist in scope. Idempotent when a key
    /// is supplied (same semantics as `add_node`).
    pub fn add_edge(
        &self,
        scope: &ScopeKey,
        source_id: &CanonicalId,
        target_id: &CanonicalId,
        edge_type: EdgeType,
        properties: PropertyMap,
        idempotency_key: Option<String>,
    ) -> Result<GraphEdge> {
        Self::validate_scope(scope)?;
        // Endpoint validation (the engine's contract; the store is agnostic).
        if self.store.get_node(scope, source_id)?.is_none() {
            return Err(GraphError::NotFound {
                entity: "node",
                id: source_id.to_string(),
            });
        }
        if self.store.get_node(scope, target_id)?.is_none() {
            return Err(GraphError::NotFound {
                entity: "node",
                id: target_id.to_string(),
            });
        }
        if let Some(key) = &idempotency_key {
            let fp = fingerprint_edge(&edge_type, source_id, target_id, &properties);
            let guard = self.idempotency.lock().expect("idem lock poisoned");
            if let Some((saved_fp, id)) = guard.get(&(scope.clone(), key.clone())) {
                if *saved_fp == fp {
                    return self
                        .store
                        .get_edge(scope, id)?
                        .ok_or_else(|| GraphError::NotFound {
                            entity: "edge",
                            id: id.to_string(),
                        });
                }
                return Err(GraphError::IdempotencyConflict { id: id.to_string() });
            }
        }
        let id = self
            .id_gen
            .lock()
            .expect("id gen lock poisoned")
            .next(IdKind::Edge);
        let edge = GraphEdge::new(
            id.clone(),
            source_id.clone(),
            target_id.clone(),
            edge_type,
            properties.clone(),
        );
        self.store.put_edge(scope, &edge)?;
        if let Some(key) = idempotency_key {
            let fp = fingerprint_edge(&edge_type, source_id, target_id, &properties);
            self.idempotency
                .lock()
                .expect("idem lock poisoned")
                .insert((scope.clone(), key), (fp, id));
        }
        Ok(edge)
    }

    /// Fetch an edge in scope.
    pub fn get_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<GraphEdge> {
        Self::validate_scope(scope)?;
        self.store
            .get_edge(scope, id)?
            .ok_or_else(|| GraphError::NotFound {
                entity: "edge",
                id: id.to_string(),
            })
    }

    /// Delete an edge (idempotent at the storage level; engine reports
    /// `NotFound` when the id never existed in scope).
    pub fn delete_edge(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<()> {
        Self::validate_scope(scope)?;
        if self.store.get_edge(scope, id)?.is_none() {
            return Err(GraphError::NotFound {
                entity: "edge",
                id: id.to_string(),
            });
        }
        self.store.delete_edge(scope, id)
    }

    /// All edge ids in scope (sorted).
    pub fn edge_ids(&self, scope: &ScopeKey) -> Result<Vec<CanonicalId>> {
        Self::validate_scope(scope)?;
        self.store.edge_ids(scope)
    }

    /// Number of edges in scope.
    pub fn edge_count(&self, scope: &ScopeKey) -> Result<usize> {
        Self::validate_scope(scope)?;
        Ok(self.store.edge_ids(scope)?.len())
    }

    // ---------- topology ----------

    /// Neighbors of a node in the given direction (sorted — PROP-03).
    pub fn neighbors(
        &self,
        scope: &ScopeKey,
        id: &CanonicalId,
        direction: Direction,
    ) -> Result<Vec<(CanonicalId, EdgeType)>> {
        Self::validate_scope(scope)?;
        algorithms::get_neighbors(self.store.as_ref(), scope, id, direction)
    }

    // ---------- traversal (bounded — PROP-03) ----------

    /// Breadth-first search (start excluded, depth-limited, bounded).
    pub fn bfs(
        &self,
        scope: &ScopeKey,
        start: &CanonicalId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<Vec<CanonicalId>> {
        Self::validate_scope(scope)?;
        algorithms::bfs(self.store.as_ref(), scope, start, max_depth, max_nodes)
    }

    /// Depth-first search (bounded).
    pub fn dfs(
        &self,
        scope: &ScopeKey,
        start: &CanonicalId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<Vec<CanonicalId>> {
        Self::validate_scope(scope)?;
        algorithms::dfs(self.store.as_ref(), scope, start, max_depth, max_nodes)
    }

    /// Strongly connected components (size > 1 only).
    pub fn strongly_connected_components(&self, scope: &ScopeKey) -> Result<Vec<Vec<CanonicalId>>> {
        Self::validate_scope(scope)?;
        algorithms::strongly_connected_components(self.store.as_ref(), scope)
    }

    /// All simple paths between two nodes (depth-bounded, bounded count).
    pub fn find_all_paths(
        &self,
        scope: &ScopeKey,
        start: &CanonicalId,
        end: &CanonicalId,
        max_depth: usize,
        max_paths: Option<usize>,
    ) -> Result<Vec<Vec<CanonicalId>>> {
        Self::validate_scope(scope)?;
        algorithms::find_all_paths(self.store.as_ref(), scope, start, end, max_depth, max_paths)
    }

    // ---------- capabilities (ADR-022: probed, never assumed) ----------

    /// Probed capabilities of the backing store.
    pub fn capabilities(&self) -> StoreCapabilities {
        self.store.capabilities()
    }

    /// Durable flush; `CAPABILITY_UNAVAILABLE` on non-persistent stores.
    pub fn flush(&self) -> Result<()> {
        self.store.flush()
    }
}

impl Default for GraphEngine {
    fn default() -> Self {
        Self::new_memory()
    }
}
