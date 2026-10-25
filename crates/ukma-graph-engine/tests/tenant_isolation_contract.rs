//! Contract tests: tenant/project isolation (scope keying, no cross-scope
//! leakage — a foreign-scope read returns NotFound, existence never leaks).

use ukma_graph_engine::{
    CanonicalId, DeterministicIdGenerator, Direction, EdgeType, GraphEngine, GraphError, NodeType,
    PropertyMap, ScopeKey,
};

fn engine() -> GraphEngine {
    GraphEngine::with_store_and_ids(
        std::sync::Arc::new(ukma_graph_engine::InMemoryGraphStore::new()),
        Box::new(DeterministicIdGenerator::default()),
    )
}

const TENANT: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";

fn scope_a() -> ScopeKey {
    ScopeKey::new(TENANT, "project-1")
}

fn scope_b() -> ScopeKey {
    ScopeKey::new(TENANT_B, "project-1")
}

#[test]
fn same_id_in_two_scopes_is_distinct() {
    let e = engine();
    let a = scope_a();
    let b = scope_b();
    let props = PropertyMap::new().with("name", "shared-name");

    let n1 = e
        .add_node(&a, NodeType::Function, props.clone(), Some("key-1".into()))
        .unwrap();
    let n2 = e
        .add_node(&b, NodeType::Function, props, Some("key-1".into()))
        .unwrap();

    // the idempotency key is per-scope: both nodes were created
    assert_ne!(n1.id, n2.id);
    assert_eq!(e.node_count(&a).unwrap(), 1);
    assert_eq!(e.node_count(&b).unwrap(), 1);

    // each scope sees only its own node
    let ids_a = e.node_ids(&a).unwrap();
    let ids_b = e.node_ids(&b).unwrap();
    assert_eq!(ids_a, vec![n1.id]);
    assert_eq!(ids_b, vec![n2.id]);
}

#[test]
fn cross_scope_read_is_not_found() {
    let e = engine();
    let a = scope_a();
    let b = scope_b();
    let n = e
        .add_node(&a, NodeType::Function, PropertyMap::new(), None)
        .unwrap();

    // reading from the foreign scope: NotFound (not forbidden-leak)
    match e.get_node(&b, &n.id) {
        Err(GraphError::NotFound { entity, .. }) => assert_eq!(entity, "node"),
        ok => panic!("expected NotFound, got {ok:?}"),
    }
    // the node still exists in its own scope
    assert_eq!(e.node_count(&a).unwrap(), 1);
}

#[test]
fn cross_scope_update_and_delete_are_denied() {
    let e = engine();
    let a = scope_a();
    let b = scope_b();
    let n = e
        .add_node(
            &a,
            NodeType::Function,
            PropertyMap::new().with("v", 1i64),
            None,
        )
        .unwrap();

    // foreign-scope update: NotFound (no mutation across the boundary)
    match e.update_node(&b, &n.id, 1, PropertyMap::new().with("v", 99i64)) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound, got {ok:?}"),
    }
    // foreign-scope delete: NotFound
    match e.delete_node(&b, &n.id) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound, got {ok:?}"),
    }
    // original untouched
    let still = e.get_node(&a, &n.id).unwrap();
    assert_eq!(still.revision, 1);
    assert_eq!(
        still.get_property("v"),
        Some(&ukma_graph_engine::PropertyValue::Int(1))
    );
}

#[test]
fn cross_scope_edge_creation_is_denied() {
    let e = engine();
    let a = scope_a();
    let b = scope_b();
    let n = e
        .add_node(&a, NodeType::Function, PropertyMap::new(), None)
        .unwrap();

    // the node exists in scope a, not in scope b; creating an edge in b
    // referencing it must fail (endpoints are validated per-scope)
    match e.add_edge(&b, &n.id, &n.id, EdgeType::Calls, PropertyMap::new(), None) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound, got {ok:?}"),
    }
    assert_eq!(e.edge_count(&b).unwrap(), 0);
}

#[test]
fn traversal_does_not_leak_across_scopes() {
    let e = engine();
    let a = scope_a();
    let b = scope_b();

    // scope a: n1 -> n2
    let n1 = e
        .add_node(&a, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    let n2 = e
        .add_node(&a, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    e.add_edge(
        &a,
        &n1.id,
        &n2.id,
        EdgeType::Calls,
        PropertyMap::new(),
        None,
    )
    .unwrap();

    // scope b: an unrelated node
    let m = e
        .add_node(&b, NodeType::Function, PropertyMap::new(), None)
        .unwrap();

    // neighbors in scope b for a scope-a node: NotFound-shaped empty (no leak)
    // (neighbors of a node that doesn't exist in scope b -> empty, not an
    // error, but also never includes scope-a data)
    let n = e.neighbors(&b, &n1.id, Direction::Both).unwrap();
    assert!(n.is_empty(), "no cross-scope neighbor leakage");

    // BFS from the scope-a node in scope b: empty
    let r = e.bfs(&b, &n1.id, 5, None).unwrap();
    assert!(r.is_empty());

    // scope-a traversal sees only scope-a data
    let r = e.bfs(&a, &n1.id, 5, None).unwrap();
    assert_eq!(r, vec![n2.id]);
    assert!(!r.contains(&m.id));
}

#[test]
fn scc_is_scope_isolated() {
    let e = engine();
    let a = scope_a();
    let b = scope_b();
    // cycle in scope a
    let n1 = e
        .add_node(&a, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    let n2 = e
        .add_node(&a, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    e.add_edge(
        &a,
        &n1.id,
        &n2.id,
        EdgeType::Calls,
        PropertyMap::new(),
        None,
    )
    .unwrap();
    e.add_edge(
        &a,
        &n2.id,
        &n1.id,
        EdgeType::Calls,
        PropertyMap::new(),
        None,
    )
    .unwrap();
    // a node in scope b
    e.add_node(&b, NodeType::Function, PropertyMap::new(), None)
        .unwrap();

    let sccs_a = e.strongly_connected_components(&a).unwrap();
    assert_eq!(sccs_a, vec![vec![n1.id, n2.id]]);
    let sccs_b = e.strongly_connected_components(&b).unwrap();
    assert!(sccs_b.is_empty());
}

#[test]
fn scope_validation_rejects_empty_fields() {
    let e = engine();
    let bad = ScopeKey::new("", "p");
    let n = e
        .add_node(&bad, NodeType::Function, PropertyMap::new(), None)
        .unwrap_err();
    assert_eq!(n.code(), "INVALID_ARGUMENT");
    assert_eq!(n.error_detail().code, "INVALID_ARGUMENT");
}

#[test]
fn canonical_id_roundtrip_through_scopes() {
    let e = engine();
    let a = scope_a();
    let n = e
        .add_node(&a, NodeType::Class, PropertyMap::new(), None)
        .unwrap();
    let id = CanonicalId::parse(n.id.as_str()).unwrap();
    assert_eq!(id, n.id);
    assert_eq!(e.get_node(&a, &id).unwrap(), n);
}
