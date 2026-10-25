//! Neo4j graph adapter contract (profile C).
//!
//! Needs a live server: set `UKMA_NEO4J_URL` (and `UKMA_NEO4J_PASSWORD`), then
//! `cargo test -p ukma-adapter-neo4j`. Without it the tests report a skip.

use graph_engine::ids::{CanonicalId, IdKind, Ulid};
use graph_engine::store::{GraphStore, ScopeKey};
use graph_engine::types::{EdgeType, GraphEdge, GraphNode, NodeType, PropertyMap};
use ukma_adapter_neo4j::{capabilities_with, Neo4jStore, URL_ENV};

fn node(id: &CanonicalId) -> GraphNode {
    GraphNode {
        id: id.clone(),
        node_type: NodeType::Function,
        properties: PropertyMap::new(),
        revision: 1,
        etag: "e1".into(),
    }
}

fn edge(id: &CanonicalId, from: &CanonicalId, to: &CanonicalId) -> GraphEdge {
    GraphEdge {
        id: id.clone(),
        source_id: from.clone(),
        target_id: to.clone(),
        edge_type: EdgeType::Calls,
        properties: PropertyMap::new(),
        revision: 1,
        etag: "e1".into(),
    }
}

fn id(kind: IdKind, seed: u8) -> CanonicalId {
    let mut bytes = [0u8; 16];
    bytes[15] = seed;
    CanonicalId::from_parts(kind, Ulid(bytes))
}

fn opened(tag: &str) -> Option<(Neo4jStore, ScopeKey)> {
    let url = std::env::var(URL_ENV).ok()?;
    if url.trim().is_empty() {
        return None;
    }
    let user = std::env::var("UKMA_NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
    let password = std::env::var("UKMA_NEO4J_PASSWORD").unwrap_or_default();
    let store = Neo4jStore::connect(&url, &user, &password, "neo4j").expect("connect to Neo4j");
    let scope = ScopeKey::new("tenant-n4j", format!("proj-{tag}"));
    store.purge_scope(&scope).expect("clean scope");
    Some((store, scope))
}

macro_rules! needs_server {
    ($tag:literal) => {
        match opened($tag) {
            Some(pair) => pair,
            None => {
                eprintln!("skipped: {} is not set", URL_ENV);
                return;
            }
        }
    };
}

#[test]
fn nodes_edges_and_cascading_delete_hold() {
    let (store, scope) = needs_server!("graph");
    let a = id(IdKind::Node, 1);
    let b = id(IdKind::Node, 2);
    let call = id(IdKind::Edge, 3);
    store.put_node(&scope, &node(&a)).unwrap();
    store.put_node(&scope, &node(&b)).unwrap();
    store.put_edge(&scope, &edge(&call, &a, &b)).unwrap();

    assert_eq!(store.node_ids(&scope).unwrap().len(), 2);
    assert_eq!(store.get_node(&scope, &a).unwrap().unwrap().id, a);
    assert_eq!(
        store.outgoing_edge_ids(&scope, &a).unwrap(),
        vec![call.clone()]
    );
    assert_eq!(
        store.incoming_edge_ids(&scope, &b).unwrap(),
        vec![call.clone()]
    );

    // Deleting a node cascades to its incident edges.
    store.delete_node(&scope, &a).unwrap();
    assert!(store.get_edge(&scope, &call).unwrap().is_none());
    assert_eq!(store.node_ids(&scope).unwrap(), vec![b.clone()]);
    store.purge_scope(&scope).unwrap();
}

#[test]
fn a_foreign_scope_sees_nothing() {
    let (store, scope) = needs_server!("scope");
    let other = ScopeKey::new("tenant-n4j-other", "proj-other");
    let a = id(IdKind::Node, 9);
    store.put_node(&scope, &node(&a)).unwrap();
    assert!(store.get_node(&other, &a).unwrap().is_none());
    assert!(store.node_ids(&other).unwrap().is_empty());
    store.purge_scope(&scope).unwrap();
}

#[test]
fn the_adapter_is_available_only_with_a_url() {
    let unconfigured = capabilities_with(None);
    assert!(!unconfigured.available);
    assert!(unconfigured.unavailable_reason.contains("UKMA_NEO4J_URL"));
    let configured = capabilities_with(Some("http://127.0.0.1:7474"));
    assert!(configured.available);
    assert!(configured.native_graph_traversal);
    assert_eq!(configured.supported_ports, vec!["GraphPort".to_string()]);
}

#[test]
fn the_store_reports_persistent_capabilities() {
    let (store, scope) = needs_server!("caps");
    let caps = store.capabilities();
    assert!(caps.persistent);
    assert!(caps.atomic_batches);
    store.flush().unwrap();
    store.purge_scope(&scope).unwrap();
}
