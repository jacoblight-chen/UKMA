//! Contract tests: CRUD semantics (including the S0 golden fixture parity
//! test `tests/golden/graph-basic-ops.golden.json`).

use std::path::Path;

use graph_engine::{
    CanonicalId, DeterministicIdGenerator, EdgeType, GraphEngine, GraphError, NodeType,
    PropertyMap, PropertyValue, ScopeKey,
};

const TENANT: &str = "tenant-a";
const PROJECT: &str = "project-1";

fn scope() -> ScopeKey {
    ScopeKey::new(TENANT, PROJECT)
}

fn engine() -> (GraphEngine, ScopeKey) {
    let e = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(graph_engine::InMemoryGraphStore::new()),
        Box::new(DeterministicIdGenerator::default()),
    );
    (e, scope())
}

#[test]
fn add_node_assigns_canonical_id_at_revision_1() {
    let (e, s) = engine();
    let node = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("name", "test_func".to_string()),
            None,
        )
        .unwrap();
    assert!(node.id.as_str().starts_with("graph.node."));
    assert_eq!(node.revision, 1);
    assert!(!node.etag.is_empty());
    let fetched = e.get_node(&s, &node.id).unwrap();
    assert_eq!(fetched, node);
    assert_eq!(
        fetched.get_property("name"),
        Some(&PropertyValue::String("test_func".into()))
    );
}

#[test]
fn get_missing_node_is_typed_not_found() {
    let (e, s) = engine();
    let id = CanonicalId::parse("graph.node.00000000000000000000000001").unwrap();
    match e.get_node(&s, &id) {
        Err(GraphError::NotFound { entity, id: miss }) => {
            assert_eq!(entity, "node");
            assert_eq!(id.as_str(), miss);
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn add_edge_validates_endpoints() {
    let (e, s) = engine();
    let a = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    let b = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    // valid edge
    let edge = e
        .add_edge(
            &s,
            &a.id,
            &b.id,
            EdgeType::Calls,
            PropertyMap::new().with("line", 42i64),
            None,
        )
        .unwrap();
    assert!(edge.id.as_str().starts_with("graph.edge."));
    // missing source
    let missing = CanonicalId::parse("graph.node.00000000000000000000000009").unwrap();
    match e.add_edge(
        &s,
        &missing,
        &b.id,
        EdgeType::Calls,
        PropertyMap::new(),
        None,
    ) {
        Err(GraphError::NotFound { entity, .. }) => assert_eq!(entity, "node"),
        other => panic!("expected NotFound for missing endpoint, got {other:?}"),
    }
}

#[test]
fn delete_node_cascades_edges() {
    let (e, s) = engine();
    let a = e
        .add_node(&s, NodeType::CodeFile, PropertyMap::new(), None)
        .unwrap();
    let b = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    let edge = e
        .add_edge(
            &s,
            &a.id,
            &b.id,
            EdgeType::Contains,
            PropertyMap::new(),
            None,
        )
        .unwrap();
    assert_eq!(e.edge_count(&s).unwrap(), 1);
    e.delete_node(&s, &a.id).unwrap();
    assert_eq!(e.node_count(&s).unwrap(), 1);
    // cascaded edge is gone
    assert_eq!(e.edge_count(&s).unwrap(), 0);
    match e.get_edge(&s, &edge.id) {
        Err(GraphError::NotFound { entity, .. }) => assert_eq!(entity, "edge"),
        other => panic!("expected NotFound, got {other:?}"),
    }
    // deleting an already-deleted node is NotFound (not a silent success)
    match e.delete_node(&s, &a.id) {
        Err(GraphError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn delete_edge_removes_topology() {
    let (e, s) = engine();
    let a = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    let b = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    let edge = e
        .add_edge(&s, &a.id, &b.id, EdgeType::Calls, PropertyMap::new(), None)
        .unwrap();
    e.delete_edge(&s, &edge.id).unwrap();
    assert_eq!(e.edge_count(&s).unwrap(), 0);
    let n = e
        .neighbors(&s, &a.id, graph_engine::Direction::Outgoing)
        .unwrap();
    assert!(n.is_empty());
}

#[test]
fn idempotent_add_node_replay_and_conflict() {
    let (e, s) = engine();
    let props = PropertyMap::new().with("name", "idem_fn");
    let first = e
        .add_node(&s, NodeType::Function, props.clone(), Some("key-1".into()))
        .unwrap();
    // replay with the same payload -> same id (idempotent)
    let replay = e
        .add_node(&s, NodeType::Function, props.clone(), Some("key-1".into()))
        .unwrap();
    assert_eq!(first.id, replay.id);
    // same key, different payload -> IDEMPOTENCY_CONFLICT
    let other = PropertyMap::new().with("name", "other_fn");
    match e.add_node(&s, NodeType::Function, other, Some("key-1".into())) {
        Err(GraphError::IdempotencyConflict { .. }) => {}
        ok => panic!("expected IdempotencyConflict, got {ok:?}"),
    }
    // a fresh key creates a fresh node
    let fresh = e
        .add_node(&s, NodeType::Function, props, Some("key-2".into()))
        .unwrap();
    assert_ne!(fresh.id, first.id);
}

#[test]
fn idempotency_keys_are_scope_scoped() {
    let (e, s) = engine();
    let other = ScopeKey::new(TENANT, "project-2");
    let props = PropertyMap::new().with("name", "scoped");
    let n1 = e
        .add_node(&s, NodeType::Function, props.clone(), Some("key-x".into()))
        .unwrap();
    let n2 = e
        .add_node(&other, NodeType::Function, props, Some("key-x".into()))
        .unwrap();
    assert_ne!(n1.id, n2.id);
}

#[test]
fn empty_scope_fields_are_invalid_argument() {
    let (e, _) = engine();
    let bad = ScopeKey::new("", "project-1");
    match e.add_node(&bad, NodeType::Function, PropertyMap::new(), None) {
        Err(GraphError::InvalidArgument { field, .. }) => assert_eq!(field, "tenant_id"),
        ok => panic!("expected InvalidArgument, got {ok:?}"),
    }
    let bad2 = ScopeKey::new(TENANT, "");
    match e.add_node(&bad2, NodeType::Function, PropertyMap::new(), None) {
        Err(GraphError::InvalidArgument { field, .. }) => assert_eq!(field, "project_id"),
        ok => panic!("expected InvalidArgument, got {ok:?}"),
    }
}

// ---------- S0 golden fixture parity ----------

#[test]
fn golden_fixture_basic_ops_parity() {
    // Load the S0 golden fixture (observed upstream behavior, locked SHA).
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/graph-basic-ops.golden.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden fixture missing at {path:?}: {e}"));
    let golden: serde_json::Value = serde_json::from_str(&raw).expect("fixture is valid JSON");
    assert_eq!(
        golden["fixture_id"].as_str().unwrap(),
        "graph-basic-ops",
        "wrong fixture"
    );

    let (e, s) = engine();

    // Operation 1: add_node Function "test_func"
    let n1 = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("name", "test_func".to_string()),
            None,
        )
        .unwrap();
    // Operation 2: add_node Function (empty properties)
    let n2 = e
        .add_node(&s, NodeType::Function, PropertyMap::new(), None)
        .unwrap();
    // Operation 3: add_edge n1 -> n2 Calls, line 42
    let edge = e
        .add_edge(
            &s,
            &n1.id,
            &n2.id,
            EdgeType::Calls,
            PropertyMap::new().with("line", 42i64),
            None,
        )
        .unwrap();
    // Operation 4: get_node (first node)
    let got_n1 = e.get_node(&s, &n1.id).unwrap();
    assert_eq!(got_n1.node_type, NodeType::Function);
    assert_eq!(
        got_n1.get_property("name"),
        Some(&PropertyValue::String("test_func".into()))
    );
    // Operation 5: get_edge
    let got_edge = e.get_edge(&s, &edge.id).unwrap();
    assert_eq!(got_edge.source_id, n1.id);
    assert_eq!(got_edge.target_id, n2.id);
    assert_eq!(got_edge.edge_type, EdgeType::Calls);
    assert_eq!(got_edge.get_property("line"), Some(&PropertyValue::Int(42)));
    // Operation 6: get_neighbors(n1, Outgoing)
    let neighbors = e
        .neighbors(&s, &n1.id, graph_engine::Direction::Outgoing)
        .unwrap();
    // Operation 7: add_edge with missing endpoints -> typed error
    let missing_a = CanonicalId::parse("graph.node.0000000000000000000000000A").unwrap();
    let missing_b = CanonicalId::parse("graph.node.0000000000000000000000000B").unwrap();
    match e.add_edge(
        &s,
        &missing_a,
        &missing_b,
        EdgeType::Calls,
        PropertyMap::new(),
        None,
    ) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound for missing endpoints, got {ok:?}"),
    }
    // Operation 8: get_node missing -> typed error
    let missing_n = CanonicalId::parse("graph.node.00000000000000000000000009").unwrap();
    assert!(matches!(
        e.get_node(&s, &missing_n),
        Err(GraphError::NotFound { .. })
    ));

    // ukma_s1_expectations
    assert_eq!(e.node_count(&s).unwrap(), 2, "node_count == 2");
    assert_eq!(e.edge_count(&s).unwrap(), 1, "edge_count == 1");
    assert_eq!(
        neighbors.len(),
        1,
        "neighbors_of_first_node_outgoing: exactly one target"
    );
    assert_eq!(neighbors[0].0, n2.id, "the target is the second node");
    assert_eq!(neighbors[0].1, EdgeType::Calls, "exactly one Calls edge");
    // no panic on missing ops (asserted above)
}

#[test]
fn property_map_builder_and_sorted_iteration() {
    let mut m = PropertyMap::new()
        .with("b", 2i64)
        .with("a", "x".to_string())
        .with("c", true);
    assert_eq!(m.len(), 3);
    let keys: Vec<&str> = m.iter_sorted().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["a", "b", "c"]);
    assert!(m.contains_key("a"));
    m.remove("a");
    assert!(!m.contains_key("a"));
}

#[test]
fn property_value_from_impls() {
    assert_eq!(
        PropertyValue::from("s".to_string()),
        PropertyValue::String("s".into())
    );
    assert_eq!(PropertyValue::from("s"), PropertyValue::String("s".into()));
    assert_eq!(PropertyValue::from(7i64), PropertyValue::Int(7));
    assert_eq!(PropertyValue::from(7i32), PropertyValue::Int(7));
    assert_eq!(PropertyValue::from(1.5f64), PropertyValue::Float(1.5));
    assert_eq!(PropertyValue::from(true), PropertyValue::Bool(true));
    assert_eq!(
        PropertyValue::from(vec!["a".to_string()]),
        PropertyValue::StringList(vec!["a".into()])
    );
    assert_eq!(
        PropertyValue::from(vec![1i64]),
        PropertyValue::IntList(vec![1])
    );
}
