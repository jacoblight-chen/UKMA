//! §7.2 graph query facade: bounded traversal with **per-hop authorization**.
//!
//! Differences from the raw `GraphStore`:
//! - every visited node and every returned edge passes an [`Authorizer`] check;
//! - `visited_count` counts only authorized entities, so it can never leak the
//!   existence of hidden data (§7.2, PROP-01, §40.7);
//! - traversal is bounded by depth, node count and edge count; exceeding a bound
//!   is `RESOURCE_LIMIT`, never a silent truncation (§31.4, PROP-03).

use std::sync::Arc;

use crate::error::{GraphError, Result};
use crate::ids::CanonicalId;
use crate::store::{GraphStore, ScopeKey};
use crate::types::{Direction, EdgeType, GraphEdge, GraphNode};

/// Authorization decision for one entity.
pub trait Authorizer: Send + Sync {
    /// Whether the caller may see `id` in `scope`.
    fn may_read(&self, scope: &ScopeKey, id: &CanonicalId) -> bool;

    /// Whether the caller may traverse the edge `id`.
    fn may_traverse(&self, scope: &ScopeKey, id: &CanonicalId) -> bool;
}

/// Authorizer that permits everything (single-tenant tests, internal jobs).
#[derive(Debug, Default)]
pub struct AllowAll;

impl Authorizer for AllowAll {
    fn may_read(&self, _scope: &ScopeKey, _id: &CanonicalId) -> bool {
        true
    }

    fn may_traverse(&self, _scope: &ScopeKey, _id: &CanonicalId) -> bool {
        true
    }
}

/// Mandatory traversal bounds (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraversalBounds {
    /// Maximum hops from the start node.
    pub max_depth: u32,
    /// Maximum authorized nodes to visit (excluding the start node).
    pub max_nodes: u32,
    /// Maximum edges to traverse.
    pub max_edges: u32,
}

impl Default for TraversalBounds {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_nodes: 1_000,
            max_edges: 4_000,
        }
    }
}

impl TraversalBounds {
    /// Reject degenerate bounds so a caller cannot accidentally request an
    /// unbounded walk.
    pub fn validate(&self) -> Result<()> {
        if self.max_depth == 0 {
            return Err(GraphError::InvalidArgument {
                field: "max_depth",
                reason: "must be >= 1".into(),
            });
        }
        if self.max_nodes == 0 || self.max_edges == 0 {
            return Err(GraphError::InvalidArgument {
                field: "max_nodes",
                reason: "max_nodes and max_edges must be >= 1".into(),
            });
        }
        Ok(())
    }
}

/// Result of a bounded traversal.
#[derive(Debug, Clone, PartialEq)]
pub struct TraversalResult {
    /// Authorized nodes reached (start node excluded), sorted.
    pub nodes: Vec<GraphNode>,
    /// Authorized edges traversed.
    pub edges: Vec<GraphEdge>,
    /// Count of authorized nodes only (§7.2).
    pub visited_count: usize,
}

/// Relation filter for [`GraphQuery::query_relations`].
#[derive(Debug, Clone, Default)]
pub struct RelationFilter {
    /// Restrict to these edge types.
    pub edge_types: Vec<EdgeType>,
    /// Restrict to edges touching this node.
    pub touching: Option<CanonicalId>,
    /// Restrict to edges from this node.
    pub source: Option<CanonicalId>,
    /// Restrict to edges to this node.
    pub target: Option<CanonicalId>,
    /// Maximum edges returned.
    pub limit: Option<usize>,
}

/// Graph query facade (§7.2).
pub struct GraphQuery {
    store: Arc<dyn GraphStore>,
    authorizer: Arc<dyn Authorizer>,
}

impl GraphQuery {
    /// Build a facade over a store with an authorization policy.
    pub fn new(store: Arc<dyn GraphStore>, authorizer: Arc<dyn Authorizer>) -> Self {
        Self { store, authorizer }
    }

    /// Store behind the facade.
    pub fn store(&self) -> &Arc<dyn GraphStore> {
        &self.store
    }

    /// Batch upsert of nodes and edges (`upsert_batch`, §7.2).
    ///
    /// Edges whose endpoints are missing in scope are rejected before anything is
    /// written, so a batch never leaves a dangling edge behind.
    pub fn upsert_batch(
        &self,
        scope: &ScopeKey,
        nodes: &[GraphNode],
        edges: &[GraphEdge],
    ) -> Result<usize> {
        let mut known: Vec<CanonicalId> = self.store.node_ids(scope)?;
        for node in nodes {
            if !known.contains(&node.id) {
                known.push(node.id.clone());
            }
        }
        for edge in edges {
            if !known.contains(&edge.source_id) || !known.contains(&edge.target_id) {
                return Err(GraphError::NotFound {
                    entity: "node",
                    id: if known.contains(&edge.source_id) {
                        edge.target_id.to_string()
                    } else {
                        edge.source_id.to_string()
                    },
                });
            }
        }
        let mut written = 0usize;
        for node in nodes {
            self.store.put_node(scope, node)?;
            written += 1;
        }
        for edge in edges {
            self.store.put_edge(scope, edge)?;
            written += 1;
        }
        Ok(written)
    }

    /// Read one entity, subject to authorization (`get_entity`).
    pub fn get_entity(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphNode>> {
        if !self.authorizer.may_read(scope, id) {
            // Do not reveal whether it exists (§40.7).
            return Ok(None);
        }
        self.store.get_node(scope, id)
    }

    /// Read one relation, subject to authorization (`get_relation`).
    pub fn get_relation(&self, scope: &ScopeKey, id: &CanonicalId) -> Result<Option<GraphEdge>> {
        if !self.authorizer.may_traverse(scope, id) {
            return Ok(None);
        }
        self.store.get_edge(scope, id)
    }

    /// Bounded breadth-first traversal (`traverse_bounded`).
    ///
    /// Every hop re-checks authorization for the edge and the target node; a
    /// hidden node is neither returned nor counted. Exceeding `max_nodes` or
    /// `max_edges` is `RESOURCE_LIMIT`.
    pub fn traverse_bounded(
        &self,
        scope: &ScopeKey,
        start: &CanonicalId,
        direction: Direction,
        edge_types: &[EdgeType],
        bounds: TraversalBounds,
    ) -> Result<TraversalResult> {
        bounds.validate()?;
        if self.store.get_node(scope, start)?.is_none() {
            return Err(GraphError::NotFound {
                entity: "node",
                id: start.to_string(),
            });
        }

        let mut visited: Vec<CanonicalId> = vec![start.clone()];
        let mut nodes: Vec<GraphNode> = Vec::new();
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut frontier: Vec<(CanonicalId, u32)> = vec![(start.clone(), 0)];
        let mut head = 0usize;

        while head < frontier.len() {
            let (current, depth) = frontier[head].clone();
            head += 1;
            if depth >= bounds.max_depth {
                continue;
            }

            let mut candidates: Vec<(CanonicalId, bool)> = Vec::new();
            if matches!(direction, Direction::Outgoing | Direction::Both) {
                for edge_id in self.store.outgoing_edge_ids(scope, &current)? {
                    candidates.push((edge_id, true));
                }
            }
            if matches!(direction, Direction::Incoming | Direction::Both) {
                for edge_id in self.store.incoming_edge_ids(scope, &current)? {
                    candidates.push((edge_id, false));
                }
            }

            for (edge_id, outgoing) in candidates {
                if !self.authorizer.may_traverse(scope, &edge_id) {
                    continue;
                }
                let Some(edge) = self.store.get_edge(scope, &edge_id)? else {
                    continue;
                };
                if !edge_types.is_empty() && !edge_types.contains(&edge.edge_type) {
                    continue;
                }
                let next = if outgoing {
                    edge.target_id.clone()
                } else {
                    edge.source_id.clone()
                };
                if !self.authorizer.may_read(scope, &next) {
                    continue;
                }
                if edges.len() >= bounds.max_edges as usize {
                    return Err(GraphError::ResourceLimit {
                        limit: u64::from(bounds.max_edges),
                    });
                }
                if !edges.contains(&edge) {
                    edges.push(edge);
                }
                if visited.contains(&next) {
                    continue;
                }
                let Some(node) = self.store.get_node(scope, &next)? else {
                    continue;
                };
                if nodes.len() >= bounds.max_nodes as usize {
                    return Err(GraphError::ResourceLimit {
                        limit: u64::from(bounds.max_nodes),
                    });
                }
                visited.push(next.clone());
                nodes.push(node);
                frontier.push((next, depth + 1));
            }
        }

        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        edges.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(TraversalResult {
            visited_count: nodes.len(),
            nodes,
            edges,
        })
    }

    /// All simple paths between two authorized nodes (`find_paths`).
    pub fn find_paths(
        &self,
        scope: &ScopeKey,
        start: &CanonicalId,
        end: &CanonicalId,
        max_depth: usize,
        max_paths: usize,
    ) -> Result<Vec<Vec<CanonicalId>>> {
        if !self.authorizer.may_read(scope, start) || !self.authorizer.may_read(scope, end) {
            return Ok(Vec::new());
        }
        let mut blocked = Vec::new();
        for id in self.store.node_ids(scope)? {
            if !self.authorizer.may_read(scope, &id) {
                blocked.push(id);
            }
        }
        // Delegate to the S1 implementation which already enforces bounds and
        // deterministic ordering; hidden nodes are excluded by re-filtering.
        let paths = crate::algorithms::find_all_paths(
            self.store.as_ref(),
            scope,
            start,
            end,
            max_depth,
            Some(max_paths),
        )?;
        Ok(paths
            .into_iter()
            .filter(|path| path.iter().all(|id| !blocked.contains(id)))
            .collect())
    }

    /// Query relations matching a filter (`query_relations`).
    pub fn query_relations(
        &self,
        scope: &ScopeKey,
        filter: &RelationFilter,
    ) -> Result<Vec<GraphEdge>> {
        let mut results: Vec<GraphEdge> = Vec::new();
        for edge_id in self.store.edge_ids(scope)? {
            if !self.authorizer.may_traverse(scope, &edge_id) {
                continue;
            }
            let Some(edge) = self.store.get_edge(scope, &edge_id)? else {
                continue;
            };
            if !filter.edge_types.is_empty() && !filter.edge_types.contains(&edge.edge_type) {
                continue;
            }
            if let Some(touching) = &filter.touching {
                if edge.source_id != *touching && edge.target_id != *touching {
                    continue;
                }
            }
            if let Some(source) = &filter.source {
                if edge.source_id != *source {
                    continue;
                }
            }
            if let Some(target) = &filter.target {
                if edge.target_id != *target {
                    continue;
                }
            }
            if !self.authorizer.may_read(scope, &edge.source_id)
                || !self.authorizer.may_read(scope, &edge.target_id)
            {
                continue;
            }
            results.push(edge);
        }
        results.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(limit) = filter.limit {
            if results.len() > limit {
                return Err(GraphError::ResourceLimit {
                    limit: limit as u64,
                });
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::GraphEngine;
    use crate::memory_store::InMemoryGraphStore;
    use crate::types::{NodeType, PropertyMap};
    use std::collections::HashSet;

    struct DenyList(HashSet<String>);

    impl Authorizer for DenyList {
        fn may_read(&self, _scope: &ScopeKey, id: &CanonicalId) -> bool {
            !self.0.contains(id.as_str())
        }

        fn may_traverse(&self, _scope: &ScopeKey, id: &CanonicalId) -> bool {
            !self.0.contains(id.as_str())
        }
    }

    fn setup() -> (Arc<InMemoryGraphStore>, ScopeKey, GraphEngine) {
        let store = Arc::new(InMemoryGraphStore::new());
        let scope = ScopeKey::new("tenant", "project");
        let engine = GraphEngine::with_store(store.clone());
        (store, scope, engine)
    }

    fn node(engine: &GraphEngine, scope: &ScopeKey, name: &str) -> GraphNode {
        engine
            .add_node(
                scope,
                NodeType::Function,
                PropertyMap::new().with("name", name.to_string()),
                None,
            )
            .unwrap()
    }

    #[test]
    fn bounded_traversal_counts_only_authorized_nodes() {
        let (store, scope, engine) = setup();
        let a = node(&engine, &scope, "a");
        let b = node(&engine, &scope, "b");
        let hidden = node(&engine, &scope, "hidden");
        let c = node(&engine, &scope, "c");
        engine
            .add_edge(
                &scope,
                &a.id,
                &b.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();
        engine
            .add_edge(
                &scope,
                &b.id,
                &hidden.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();
        engine
            .add_edge(
                &scope,
                &hidden.id,
                &c.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();

        let deny = Arc::new(DenyList(HashSet::from([hidden.id.as_str().to_string()])));
        let query = GraphQuery::new(store.clone(), deny);
        let result = query
            .traverse_bounded(
                &scope,
                &a.id,
                Direction::Outgoing,
                &[EdgeType::Calls],
                TraversalBounds {
                    max_depth: 5,
                    max_nodes: 100,
                    max_edges: 100,
                },
            )
            .unwrap();

        assert_eq!(
            result.visited_count, 1,
            "only b is reachable and authorized"
        );
        assert_eq!(result.nodes[0].id, b.id);
        // the edge into the hidden node must not be returned either
        assert!(result.edges.iter().all(|edge| edge.target_id != hidden.id));
        assert!(!result.nodes.iter().any(|node| node.id == hidden.id));
        assert!(!result.nodes.iter().any(|node| node.id == c.id));
    }

    #[test]
    fn depth_and_node_bounds_are_enforced() {
        let (store, scope, engine) = setup();
        let a = node(&engine, &scope, "a");
        let b = node(&engine, &scope, "b");
        let c = node(&engine, &scope, "c");
        engine
            .add_edge(
                &scope,
                &a.id,
                &b.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();
        engine
            .add_edge(
                &scope,
                &b.id,
                &c.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();
        let query = GraphQuery::new(store.clone(), Arc::new(AllowAll));

        let shallow = query
            .traverse_bounded(
                &scope,
                &a.id,
                Direction::Outgoing,
                &[],
                TraversalBounds {
                    max_depth: 1,
                    max_nodes: 100,
                    max_edges: 100,
                },
            )
            .unwrap();
        assert_eq!(shallow.visited_count, 1, "depth 1 reaches b only");

        let tight = query.traverse_bounded(
            &scope,
            &a.id,
            Direction::Outgoing,
            &[],
            TraversalBounds {
                max_depth: 5,
                max_nodes: 1,
                max_edges: 100,
            },
        );
        assert_eq!(tight.unwrap_err().code(), "RESOURCE_LIMIT");
    }

    #[test]
    fn invalid_bounds_and_missing_start_are_rejected() {
        let (store, scope, engine) = setup();
        let a = node(&engine, &scope, "a");
        let query = GraphQuery::new(store, Arc::new(AllowAll));
        let error = query
            .traverse_bounded(
                &scope,
                &a.id,
                Direction::Outgoing,
                &[],
                TraversalBounds {
                    max_depth: 0,
                    max_nodes: 10,
                    max_edges: 10,
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), "INVALID_ARGUMENT");

        let missing = CanonicalId::parse("graph.node.00000000000000000000000001").unwrap();
        let error = query
            .traverse_bounded(
                &scope,
                &missing,
                Direction::Outgoing,
                &[],
                TraversalBounds::default(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "NOT_FOUND");
    }

    #[test]
    fn get_entity_hides_unauthorized_nodes_without_leaking_existence() {
        let (store, scope, engine) = setup();
        let hidden = node(&engine, &scope, "hidden");
        let deny = Arc::new(DenyList(HashSet::from([hidden.id.as_str().to_string()])));
        let query = GraphQuery::new(store, deny);
        assert!(query.get_entity(&scope, &hidden.id).unwrap().is_none());
    }

    #[test]
    fn query_relations_filters_and_respects_authorization() {
        let (store, scope, engine) = setup();
        let a = node(&engine, &scope, "a");
        let b = node(&engine, &scope, "b");
        let calls = engine
            .add_edge(
                &scope,
                &a.id,
                &b.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();
        engine
            .add_edge(
                &scope,
                &a.id,
                &b.id,
                EdgeType::Contains,
                PropertyMap::new(),
                None,
            )
            .unwrap();

        let query = GraphQuery::new(store.clone(), Arc::new(AllowAll));
        let calls_only = query
            .query_relations(
                &scope,
                &RelationFilter {
                    edge_types: vec![EdgeType::Calls],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(calls_only.len(), 1);
        assert_eq!(calls_only[0].id, calls.id);

        let deny = Arc::new(DenyList(HashSet::from([calls.id.as_str().to_string()])));
        let restricted = GraphQuery::new(store, deny);
        let visible = restricted
            .query_relations(&scope, &RelationFilter::default())
            .unwrap();
        assert!(visible.iter().all(|edge| edge.id != calls.id));
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn upsert_batch_rejects_dangling_edges_before_writing() {
        let (store, scope, _engine) = setup();
        let query = GraphQuery::new(store.clone(), Arc::new(AllowAll));
        let node_a = GraphNode::new(
            CanonicalId::parse("graph.node.0000000000000000000000000A").unwrap(),
            NodeType::Function,
            PropertyMap::new(),
        );
        let ghost = CanonicalId::parse("graph.node.0000000000000000000000000B").unwrap();
        let dangling = GraphEdge::new(
            CanonicalId::parse("graph.edge.0000000000000000000000000C").unwrap(),
            node_a.id.clone(),
            ghost,
            EdgeType::Calls,
            PropertyMap::new(),
        );
        let error = query
            .upsert_batch(&scope, std::slice::from_ref(&node_a), &[dangling])
            .unwrap_err();
        assert_eq!(error.code(), "NOT_FOUND");
        assert!(
            store.node_ids(&scope).unwrap().is_empty(),
            "a rejected batch must not write anything"
        );
    }

    #[test]
    fn upsert_batch_writes_nodes_and_edges() {
        let (store, scope, _engine) = setup();
        let query = GraphQuery::new(store.clone(), Arc::new(AllowAll));
        let a = GraphNode::new(
            CanonicalId::parse("graph.node.0000000000000000000000000A").unwrap(),
            NodeType::Function,
            PropertyMap::new(),
        );
        let b = GraphNode::new(
            CanonicalId::parse("graph.node.0000000000000000000000000B").unwrap(),
            NodeType::Class,
            PropertyMap::new(),
        );
        let edge = GraphEdge::new(
            CanonicalId::parse("graph.edge.0000000000000000000000000C").unwrap(),
            a.id.clone(),
            b.id.clone(),
            EdgeType::Contains,
            PropertyMap::new(),
        );
        let written = query.upsert_batch(&scope, &[a, b], &[edge]).expect("batch");
        assert_eq!(written, 3);
        assert_eq!(store.node_ids(&scope).unwrap().len(), 2);
        assert_eq!(store.edge_ids(&scope).unwrap().len(), 1);
    }

    #[test]
    fn find_paths_excludes_hidden_nodes() {
        let (store, scope, engine) = setup();
        let a = node(&engine, &scope, "a");
        let hidden = node(&engine, &scope, "hidden");
        let c = node(&engine, &scope, "c");
        engine
            .add_edge(
                &scope,
                &a.id,
                &hidden.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();
        engine
            .add_edge(
                &scope,
                &hidden.id,
                &c.id,
                EdgeType::Calls,
                PropertyMap::new(),
                None,
            )
            .unwrap();

        let deny = Arc::new(DenyList(HashSet::from([hidden.id.as_str().to_string()])));
        let query = GraphQuery::new(store, deny);
        let paths = query.find_paths(&scope, &a.id, &c.id, 5, 10).unwrap();
        assert!(paths.is_empty(), "the only path runs through a hidden node");
    }
}
