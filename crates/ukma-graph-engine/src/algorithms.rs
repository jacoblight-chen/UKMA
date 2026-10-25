//! Graph traversal algorithms — ported from upstream `codegraph/src/graph/
//! algorithms.rs` with recorded UKMA changes:
//! - inputs/outputs use canonical string ids and a scope key (upstream used
//!   vendor u64 ids with no scope);
//! - `get_neighbors` output is sorted (upstream returned hash-ordered vectors;
//!   ordering was never part of the upstream contract — PROP-03 determinism);
//! - `strongly_connected_components` iterates node ids in sorted order and
//!   returns sorted members/SCC list (upstream order was hash-dependent);
//! - bounded `max_nodes`/`max_paths` parameters (Phase 6R PROP-03 contract;
//!   upstream relied on caller depth limits only);
//! - BFS/DFS semantics preserved: start node excluded from results,
//!   depth-limited, visited-set deduplication.

use std::collections::{HashMap, HashSet};

use crate::error::{GraphError, Result};
use crate::ids::CanonicalId;
use crate::store::{GraphStore, ScopeKey};
use crate::types::{Direction, EdgeType};

/// Default upper bound for unbounded `find_all_paths`.
const DEFAULT_MAX_PATHS: usize = 10_000;

/// Default upper bound for unbounded traversals.
const DEFAULT_MAX_NODES: usize = 10_000;

/// Neighbors of a node (ported `get_neighbors` — upstream semantics, sorted).
pub fn get_neighbors(
    store: &dyn GraphStore,
    scope: &ScopeKey,
    node: &CanonicalId,
    direction: Direction,
) -> Result<Vec<(CanonicalId, EdgeType)>> {
    fn one_dir(
        store: &dyn GraphStore,
        scope: &ScopeKey,
        node: &CanonicalId,
        outgoing: bool,
    ) -> Result<Vec<(CanonicalId, EdgeType)>> {
        let ids = if outgoing {
            store.outgoing_edge_ids(scope, node)?
        } else {
            store.incoming_edge_ids(scope, node)?
        };
        let mut out = Vec::with_capacity(ids.len());
        for edge_id in ids {
            if let Some(edge) = store.get_edge(scope, &edge_id)? {
                let other = if outgoing {
                    edge.target_id
                } else {
                    edge.source_id
                };
                out.push((other, edge.edge_type));
            }
        }
        Ok(out)
    }

    let mut result = match direction {
        Direction::Outgoing => one_dir(store, scope, node, true)?,
        Direction::Incoming => one_dir(store, scope, node, false)?,
        Direction::Both => {
            let mut r = one_dir(store, scope, node, true)?;
            r.extend(one_dir(store, scope, node, false)?);
            r
        }
    };
    result.sort();
    Ok(result)
}

/// Outgoing target ids (used by traversal + SCC).
fn outgoing_targets(
    store: &dyn GraphStore,
    scope: &ScopeKey,
    node: &CanonicalId,
) -> Result<Vec<CanonicalId>> {
    let ids = store.outgoing_edge_ids(scope, node)?;
    let mut out = Vec::with_capacity(ids.len());
    for edge_id in ids {
        if let Some(edge) = store.get_edge(scope, &edge_id)? {
            out.push(edge.target_id);
        }
    }
    Ok(out)
}

/// Breadth-first search (ported `bfs` — upstream semantics preserved:
/// start node excluded, depth-limited, deduplicated, bounded, sorted output).
pub fn bfs(
    store: &dyn GraphStore,
    scope: &ScopeKey,
    start: &CanonicalId,
    max_depth: usize,
    max_nodes: Option<usize>,
) -> Result<Vec<CanonicalId>> {
    let bound = max_nodes.unwrap_or(DEFAULT_MAX_NODES);
    let mut visited: HashSet<CanonicalId> = HashSet::new();
    let mut results: Vec<CanonicalId> = Vec::new();
    let mut frontier: Vec<(CanonicalId, usize)> = vec![(start.clone(), 0)];
    visited.insert(start.clone());

    while let Some((current, depth)) = frontier.pop() {
        if depth >= max_depth {
            continue;
        }
        for target in outgoing_targets(store, scope, &current)? {
            if visited.contains(&target) {
                continue;
            }
            visited.insert(target.clone());
            if results.len() >= bound {
                return Err(GraphError::ResourceLimit {
                    limit: bound as u64,
                });
            }
            results.push(target.clone());
            frontier.push((target, depth + 1));
        }
    }

    results.sort();
    Ok(results)
}

/// Depth-first search (ported `dfs` — upstream semantics preserved:
/// start node excluded, deduplicated, bounded, sorted output).
pub fn dfs(
    store: &dyn GraphStore,
    scope: &ScopeKey,
    start: &CanonicalId,
    max_depth: usize,
    max_nodes: Option<usize>,
) -> Result<Vec<CanonicalId>> {
    let bound = max_nodes.unwrap_or(DEFAULT_MAX_NODES);
    let mut visited: HashSet<CanonicalId> = HashSet::new();
    let mut results: Vec<CanonicalId> = Vec::new();
    let mut stack: Vec<(CanonicalId, usize)> = vec![(start.clone(), 0)];
    visited.insert(start.clone());

    while let Some((current, depth)) = stack.pop() {
        if depth >= max_depth {
            continue;
        }
        let targets = outgoing_targets(store, scope, &current)?;
        for target in targets.into_iter().rev() {
            if visited.contains(&target) {
                continue;
            }
            visited.insert(target.clone());
            if results.len() >= bound {
                return Err(GraphError::ResourceLimit {
                    limit: bound as u64,
                });
            }
            results.push(target.clone());
            stack.push((target, depth + 1));
        }
    }

    results.sort();
    Ok(results)
}

/// Strongly connected components (ported Tarjan — upstream `len > 1` filter
/// preserved; iteration over sorted node ids and sorted output added for
/// determinism).
pub fn strongly_connected_components(
    store: &dyn GraphStore,
    scope: &ScopeKey,
) -> Result<Vec<Vec<CanonicalId>>> {
    let node_ids = store.node_ids(scope)?;
    let n = node_ids.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    // Sorted-index view (deterministic — recorded UKMA change).
    let mut index_of: HashMap<CanonicalId, usize> = HashMap::new();
    for (i, id) in node_ids.iter().enumerate() {
        index_of.insert(id.clone(), i);
    }
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, id) in node_ids.iter().enumerate() {
        for target in outgoing_targets(store, scope, id)? {
            if let Some(&j) = index_of.get(&target) {
                adjacency[i].push(j);
            }
        }
    }

    // Iterative Tarjan (explicit stack — avoids deep-graph stack overflow).
    // Low-link propagation: a finished node propagates `low` to its parent
    // (the next work item); cross edges to on-stack nodes use `index`.
    let mut index = vec![0usize; n];
    let mut low_link = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut counter = 0usize;
    let mut scc_stack: Vec<usize> = Vec::new();
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if index[root] != 0 {
            continue;
        }
        // First-visit the root.
        counter += 1;
        index[root] = counter;
        low_link[root] = counter;
        scc_stack.push(root);
        on_stack[root] = true;
        // work item: (node, next child position)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some((node, child_pos)) = work.pop() {
            let adj = &adjacency[node];
            if child_pos < adj.len() {
                let child = adj[child_pos];
                work.push((node, child_pos + 1));
                if index[child] == 0 {
                    counter += 1;
                    index[child] = counter;
                    low_link[child] = counter;
                    scc_stack.push(child);
                    on_stack[child] = true;
                    work.push((child, 0));
                } else if on_stack[child] {
                    low_link[node] = low_link[node].min(index[child]);
                }
            } else {
                // Node finished: propagate low-link to the parent.
                if let Some((parent, _)) = work.last() {
                    low_link[*parent] = low_link[*parent].min(low_link[node]);
                }
                if low_link[node] == index[node] {
                    let mut component = Vec::new();
                    loop {
                        let member = scc_stack.pop().expect("stack non-empty");
                        on_stack[member] = false;
                        component.push(member);
                        if member == node {
                            break;
                        }
                    }
                    if component.len() > 1 {
                        sccs.push(component);
                    }
                }
            }
        }
    }

    let mut out: Vec<Vec<CanonicalId>> = sccs
        .into_iter()
        .map(|comp| {
            let mut members: Vec<CanonicalId> =
                comp.into_iter().map(|i| node_ids[i].clone()).collect();
            members.sort();
            members
        })
        .collect();
    out.sort();
    Ok(out)
}

/// All simple paths from start to end (ported `find_all_paths` — upstream
/// semantics preserved: simple paths only via per-path visited set,
/// depth-bounded, bounded path count, sorted output).
pub fn find_all_paths(
    store: &dyn GraphStore,
    scope: &ScopeKey,
    start: &CanonicalId,
    end: &CanonicalId,
    max_depth: usize,
    max_paths: Option<usize>,
) -> Result<Vec<Vec<CanonicalId>>> {
    let bound = max_paths.unwrap_or(DEFAULT_MAX_PATHS);
    let mut out: Vec<Vec<CanonicalId>> = Vec::new();

    // Frame: (node, path so far, visited ids in this path).
    let mut stack: Vec<(CanonicalId, Vec<CanonicalId>, HashSet<CanonicalId>)> = vec![(
        start.clone(),
        vec![start.clone()],
        HashSet::from([start.clone()]),
    )];

    while let Some((node, path, visited)) = stack.pop() {
        if node == *end {
            if out.len() >= bound {
                return Err(GraphError::ResourceLimit {
                    limit: bound as u64,
                });
            }
            out.push(path);
            continue;
        }
        if path.len() > max_depth {
            continue;
        }
        let targets = outgoing_targets(store, scope, &node)?;
        for target in targets.into_iter().rev() {
            if visited.contains(&target) {
                continue;
            }
            let mut v = visited.clone();
            v.insert(target.clone());
            let mut p = path.clone();
            p.push(target.clone());
            stack.push((target, p, v));
        }
    }

    out.sort();
    Ok(out)
}
