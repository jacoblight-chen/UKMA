//! Contract tests: traversal semantics (ported bfs/dfs/SCC/find_all_paths +
//! bounded termination, PROP-03).

use graph_engine::{
    DeterministicIdGenerator, Direction, EdgeType, GraphEngine, GraphError, NodeType, PropertyMap,
    ScopeKey,
};

fn engine() -> (GraphEngine, ScopeKey) {
    let e = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(graph_engine::InMemoryGraphStore::new()),
        Box::new(DeterministicIdGenerator::default()),
    );
    (e, ScopeKey::new("t", "p"))
}

fn add(e: &GraphEngine, s: &ScopeKey, name: &str) -> graph_engine::GraphNode {
    e.add_node(
        s,
        NodeType::Function,
        PropertyMap::new().with("name", name.to_string()),
        None,
    )
    .unwrap()
}

fn link(e: &GraphEngine, s: &ScopeKey, a: &graph_engine::GraphNode, b: &graph_engine::GraphNode) {
    e.add_edge(s, &a.id, &b.id, EdgeType::Calls, PropertyMap::new(), None)
        .unwrap();
}

#[test]
fn bfs_chain_respects_depth_and_excludes_start() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    link(&e, &s, &a, &b);
    link(&e, &s, &b, &c);

    let depth1 = e.bfs(&s, &a.id, 1, None).unwrap();
    assert_eq!(depth1, vec![b.id.clone()], "depth 1: only the first hop");

    let depth2 = e.bfs(&s, &a.id, 2, None).unwrap();
    assert_eq!(
        depth2,
        vec![b.id.clone(), c.id],
        "depth 2: both hops, sorted"
    );

    // start node excluded
    assert!(!depth2.contains(&a.id));

    // bounded: max_nodes 1 -> ResourceLimit on a 2-hop chain
    let bounded = e.bfs(&s, &a.id, 2, Some(1));
    match bounded {
        Err(GraphError::ResourceLimit { limit }) => assert_eq!(limit, 1),
        ok => panic!("expected ResourceLimit, got {ok:?}"),
    }
}

#[test]
fn bfs_diamond_deduplicates() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    let d = add(&e, &s, "d");
    link(&e, &s, &a, &b);
    link(&e, &s, &a, &c);
    link(&e, &s, &b, &d);
    link(&e, &s, &c, &d);

    let r = e.bfs(&s, &a.id, 2, None).unwrap();
    assert_eq!(r.len(), 3, "b, c, d — d deduplicated");
    assert!(r.contains(&b.id) && r.contains(&c.id) && r.contains(&d.id));
    assert!(r.iter().all(|x| x != &a.id));
}

#[test]
fn dfs_chain_and_dedup() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    link(&e, &s, &a, &b);
    link(&e, &s, &b, &c);

    let r = e.dfs(&s, &a.id, 2, None).unwrap();
    assert_eq!(r, vec![b.id, c.id]);
    assert!(!r.contains(&a.id));
}

#[test]
fn bounded_self_loop_terminates() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    // self-loop a -> a
    link(&e, &s, &a, &a);

    // BFS: the start node is pre-visited, so the self-loop target is
    // skipped — termination is guaranteed without the depth bound.
    let r = e.bfs(&s, &a.id, 100, None).unwrap();
    assert!(r.is_empty(), "self-loop yields no new nodes");

    // DFS: same guarantee.
    let r = e.dfs(&s, &a.id, 100, None).unwrap();
    assert!(r.is_empty());
}

#[test]
fn cycle_terminates_with_dedup() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    link(&e, &s, &a, &b);
    link(&e, &s, &b, &a); // cycle

    let r = e.bfs(&s, &a.id, 100, None).unwrap();
    assert_eq!(r, vec![b.id], "cycle terminates, b once");
}

#[test]
fn two_node_scc_detected_single_node_excluded() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    link(&e, &s, &a, &b);
    link(&e, &s, &b, &a); // a <-> b is an SCC of 2
                          // c has no edges: singleton SCCs are filtered out (upstream len > 1)

    let sccs = e.strongly_connected_components(&s).unwrap();
    assert_eq!(sccs.len(), 1);
    assert_eq!(sccs[0], vec![a.id, b.id]);
    assert!(!sccs[0].contains(&c.id));
}

#[test]
fn find_all_paths_diamond() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    let d = add(&e, &s, "d");
    link(&e, &s, &a, &b);
    link(&e, &s, &a, &c);
    link(&e, &s, &b, &d);
    link(&e, &s, &c, &d);

    let paths = e.find_all_paths(&s, &a.id, &d.id, 10, None).unwrap();
    assert_eq!(paths.len(), 2, "two simple paths");
    assert!(paths
        .iter()
        .any(|p| p == &vec![a.id.clone(), b.id.clone(), d.id.clone()]));
    assert!(paths
        .iter()
        .any(|p| p == &vec![a.id.clone(), c.id.clone(), d.id.clone()]));

    // bounded: max_paths 1 -> ResourceLimit
    match e.find_all_paths(&s, &a.id, &d.id, 10, Some(1)) {
        Err(GraphError::ResourceLimit { limit }) => assert_eq!(limit, 1),
        ok => panic!("expected ResourceLimit, got {ok:?}"),
    }

    // depth bound excludes the 2-hop paths (path len 3 > max_depth 1)
    let shallow = e.find_all_paths(&s, &a.id, &d.id, 1, None).unwrap();
    assert!(shallow.is_empty(), "max_depth 1: no path of length 3 fits");
}

#[test]
fn find_all_paths_no_path_returns_empty() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    link(&e, &s, &a, &b);
    // c is unreachable from a
    let paths = e.find_all_paths(&s, &a.id, &c.id, 10, None).unwrap();
    assert!(paths.is_empty());
}

#[test]
fn neighbors_directions() {
    let (e, s) = engine();
    let a = add(&e, &s, "a");
    let b = add(&e, &s, "b");
    let c = add(&e, &s, "c");
    link(&e, &s, &a, &b); // a -> b
    link(&e, &s, &c, &b); // c -> b

    let out = e.neighbors(&s, &b.id, Direction::Outgoing).unwrap();
    assert!(out.is_empty(), "b has no outgoing edges");

    let in_ = e.neighbors(&s, &b.id, Direction::Incoming).unwrap();
    assert_eq!(in_.len(), 2, "b has two incoming edges");

    let both = e.neighbors(&s, &b.id, Direction::Both).unwrap();
    assert_eq!(both.len(), 2);

    let a_out = e.neighbors(&s, &a.id, Direction::Outgoing).unwrap();
    assert_eq!(a_out, vec![(b.id, EdgeType::Calls)]);
}
