//! Contract tests: FileGraphStore persistence (development-only adapter)
//! and adapter compatibility (same engine API over different stores).
//!
//! Guarantees under test (honest, ADR-022):
//! - a flushed state survives process restart (persistent = true);
//! - unflushed state does NOT persist;
//! - corrupt/unsupported files fail gracefully (typed error, no panic);
//! - canonical ids, revisions, etags and topology are stable across
//!   reload and across adapter substitution (PROP-02).

use std::path::PathBuf;

use ukma_graph_engine::{
    DeterministicIdGenerator, Direction, EdgeType, FileGraphStore, GraphEngine, GraphError,
    InMemoryGraphStore, NodeType, PropertyMap, ScopeKey,
};

const TENANT: &str = "tenant-p";
const PROJECT: &str = "project-p";

fn scope() -> ScopeKey {
    ScopeKey::new(TENANT, PROJECT)
}

fn tmp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ukma-graph-engine-{}-{}.json",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_file(&p); // clean slate
    p
}

fn engine_on_file(path: &PathBuf) -> (GraphEngine, ScopeKey) {
    let store = FileGraphStore::open(path).expect("store opens");
    let e = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(store),
        Box::new(DeterministicIdGenerator::default()),
    );
    (e, scope())
}

fn reengine_on_file(path: &PathBuf) -> (GraphEngine, ScopeKey) {
    // Simulates a process restart: fresh store on the same file.
    engine_on_file(path)
}

fn build_chain(
    e: &GraphEngine,
    s: &ScopeKey,
) -> (
    ukma_graph_engine::GraphNode,
    ukma_graph_engine::GraphNode,
    ukma_graph_engine::GraphNode,
) {
    let a = e
        .add_node(
            s,
            NodeType::Function,
            PropertyMap::new().with("name", "a".to_string()),
            None,
        )
        .unwrap();
    let b = e
        .add_node(
            s,
            NodeType::Function,
            PropertyMap::new().with("name", "b".to_string()),
            None,
        )
        .unwrap();
    let c = e
        .add_node(
            s,
            NodeType::Function,
            PropertyMap::new().with("name", "c".to_string()),
            None,
        )
        .unwrap();
    e.add_edge(
        s,
        &a.id,
        &b.id,
        EdgeType::Calls,
        PropertyMap::new().with("line", 1i64),
        None,
    )
    .unwrap();
    e.add_edge(
        s,
        &b.id,
        &c.id,
        EdgeType::Calls,
        PropertyMap::new().with("line", 2i64),
        None,
    )
    .unwrap();
    (a, b, c)
}

#[test]
fn file_store_capabilities_and_flush_success() {
    let path = tmp_path("caps");
    let (e, _) = engine_on_file(&path);
    let caps = e.capabilities();
    assert!(caps.persistent, "file store is persistent after flush");
    assert!(caps.atomic_batches);
    e.flush().expect("flush succeeds on the file store");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_persists_nodes_and_edges_after_flush() {
    let path = tmp_path("persist");
    let (e, s) = engine_on_file(&path);
    let (a, _b, _c) = build_chain(&e, &s);
    e.flush().unwrap();

    // "restart": fresh engine on the same file
    let (e2, s2) = reengine_on_file(&path);
    assert_eq!(e2.node_count(&s2).unwrap(), 3);
    assert_eq!(e2.edge_count(&s2).unwrap(), 2);
    let a2 = e2.get_node(&s2, &a.id).unwrap();
    assert_eq!(a2, a, "node state intact after reload");
    let neighbors = e2.neighbors(&s2, &a.id, Direction::Outgoing).unwrap();
    assert_eq!(neighbors.len(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_without_flush_does_not_persist() {
    let path = tmp_path("noflush");
    let (e, s) = engine_on_file(&path);
    build_chain(&e, &s);
    // no flush
    assert_eq!(e.node_count(&s).unwrap(), 3);

    let (e2, s2) = reengine_on_file(&path);
    assert_eq!(
        e2.node_count(&s2).unwrap(),
        0,
        "unflushed state must not persist"
    );
    assert_eq!(e2.edge_count(&s2).unwrap(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_reload_preserves_canonical_ids_revisions_and_etags() {
    let path = tmp_path("etag");
    let (e, s) = engine_on_file(&path);
    let (a, b, c) = build_chain(&e, &s);
    // bump a revision so revision > 1 is also persisted
    let a_updated = e
        .update_node(
            &s,
            &a.id,
            1,
            PropertyMap::new()
                .with("name", "a".to_string())
                .with("rev", true),
        )
        .unwrap();
    e.flush().unwrap();

    let (e2, s2) = reengine_on_file(&path);
    assert_eq!(
        e2.get_node(&s2, &a.id).unwrap(),
        a_updated,
        "updated node (revision 2 + etag) intact"
    );
    assert_eq!(e2.get_node(&s2, &b.id).unwrap(), b);
    assert_eq!(e2.get_node(&s2, &c.id).unwrap(), c);
    // etags are deterministic and therefore identical after reload
    assert_eq!(e2.get_node(&s2, &a.id).unwrap().etag, a_updated.etag);
    // canonical ids are stable (same ids, not regenerated)
    assert_eq!(e2.node_ids(&s2).unwrap(), vec![a.id, b.id, c.id]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_delete_persists() {
    let path = tmp_path("delete");
    let (e, s) = engine_on_file(&path);
    let (a, b, _c) = build_chain(&e, &s);
    e.delete_node(&s, &a.id).unwrap(); // cascades the a->b edge
    e.flush().unwrap();

    let (e2, s2) = reengine_on_file(&path);
    assert_eq!(e2.node_count(&s2).unwrap(), 2);
    assert_eq!(e2.edge_count(&s2).unwrap(), 1); // only b->c remains
    match e2.get_node(&s2, &a.id) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound after reload, got {ok:?}"),
    }
    // the cascaded edge is gone too
    let neighbors = e2.neighbors(&s2, &b.id, Direction::Incoming).unwrap();
    assert!(neighbors.is_empty(), "cascaded edge must not reappear");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_traversal_after_reload_matches() {
    let path = tmp_path("traversal");
    let (e, s) = engine_on_file(&path);
    let (a, _b, _c) = build_chain(&e, &s);
    let before = e.bfs(&s, &a.id, 5, None).unwrap();
    e.flush().unwrap();

    let (e2, s2) = reengine_on_file(&path);
    let after = e2.bfs(&s2, &a.id, 5, None).unwrap();
    assert_eq!(before, after, "bfs topology identical after reload");
    assert_eq!(after.len(), 2);

    let scc_before = e.strongly_connected_components(&s).unwrap();
    let scc_after = e2.strongly_connected_components(&s2).unwrap();
    assert_eq!(scc_before, scc_after);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_corrupted_file_fails_gracefully() {
    let path = tmp_path("corrupt");
    std::fs::write(&path, b"this is not a snapshot {").expect("seed corrupt file");
    match FileGraphStore::open(&path) {
        Err(GraphError::Internal(msg)) => assert!(msg.contains("corrupt"), "message: {msg}"),
        Ok(store) => panic!(
            "expected typed error for corrupt file, got Ok(store at {})",
            store.path().display()
        ),
        Err(other) => panic!("unexpected error variant: {other}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_unsupported_format_fails_gracefully() {
    let path = tmp_path("fmt");
    // a well-formed snapshot with a wrong format tag
    let json = r#"{"format": 999, "nodes": [], "edges": []}"#;
    std::fs::write(&path, json.as_bytes()).expect("seed file");
    match FileGraphStore::open(&path) {
        Err(GraphError::Internal(msg)) => assert!(msg.contains("format"), "message: {msg}"),
        Ok(store) => panic!(
            "expected typed error for unsupported format, got Ok(store at {})",
            store.path().display()
        ),
        Err(other) => panic!("unexpected error variant: {other}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_store_tenant_isolation_persists() {
    let path = tmp_path("isolation");
    let other = ScopeKey::new(TENANT, "other-project");
    let (e, s) = engine_on_file(&path);
    let n1 = e
        .add_node(
            &s,
            NodeType::Function,
            PropertyMap::new().with("scope", "s"),
            None,
        )
        .unwrap();
    let n2 = e
        .add_node(
            &other,
            NodeType::Function,
            PropertyMap::new().with("scope", "o"),
            None,
        )
        .unwrap();
    e.flush().unwrap();

    let (e2, s2) = reengine_on_file(&path);
    assert_eq!(e2.node_count(&s2).unwrap(), 1);
    assert_eq!(e2.node_count(&other).unwrap(), 1);
    // cross-scope read still NotFound after reload (existence never leaks)
    match e2.get_node(&other, &n1.id) {
        Err(GraphError::NotFound { .. }) => {}
        ok => panic!("expected NotFound cross-scope, got {ok:?}"),
    }
    assert_eq!(e2.get_node(&other, &n2.id).unwrap(), n2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn adapter_equivalence_same_ops_same_results() {
    // Database-compatibility contract: the same engine API over two
    // different GraphStore adapters produces identical observable state
    // (canonical ids, revisions, etags, topology) when driven with the
    // same deterministic id generator.
    let path = tmp_path("equiv");

    let mem = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(InMemoryGraphStore::new()),
        Box::new(DeterministicIdGenerator::default()),
    );
    let file = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(FileGraphStore::open(&path).unwrap()),
        Box::new(DeterministicIdGenerator::default()),
    );
    let s = scope();

    // identical operation sequence
    for e in [&mem, &file] {
        let a = e
            .add_node(
                &s,
                NodeType::CodeFile,
                PropertyMap::new().with("name", "main.rs".to_string()),
                None,
            )
            .unwrap();
        let b = e
            .add_node(
                &s,
                NodeType::Function,
                PropertyMap::new().with("name", "main".to_string()),
                None,
            )
            .unwrap();
        e.add_edge(
            &s,
            &a.id,
            &b.id,
            EdgeType::Contains,
            PropertyMap::new().with("line", 10i64),
            None,
        )
        .unwrap();
        e.update_node(
            &s,
            &b.id,
            1,
            PropertyMap::new()
                .with("name", "main".to_string())
                .with("pub", true),
        )
        .unwrap();
        // stale update -> same typed conflict on both adapters
        match e.update_node(&s, &b.id, 1, PropertyMap::new()) {
            Err(GraphError::Conflict { .. }) => {}
            ok => panic!("expected Conflict, got {ok:?}"),
        }
        // capability semantics differ by design (probed, never assumed)
        if e.capabilities().persistent {
            e.flush().expect("flush ok");
        } else {
            match e.flush() {
                Err(GraphError::CapabilityUnavailable { .. }) => {}
                ok => panic!("expected CapabilityUnavailable, got {ok:?}"),
            }
        }
    }

    // observable state identical
    assert_eq!(mem.node_count(&s).unwrap(), file.node_count(&s).unwrap());
    assert_eq!(mem.edge_count(&s).unwrap(), file.edge_count(&s).unwrap());
    assert_eq!(mem.node_ids(&s).unwrap(), file.node_ids(&s).unwrap());
    assert_eq!(mem.edge_ids(&s).unwrap(), file.edge_ids(&s).unwrap());
    for id in mem.node_ids(&s).unwrap() {
        assert_eq!(
            mem.get_node(&s, &id).unwrap(),
            file.get_node(&s, &id).unwrap(),
            "node {id} identical across adapters"
        );
    }
    for id in mem.edge_ids(&s).unwrap() {
        assert_eq!(
            mem.get_edge(&s, &id).unwrap(),
            file.get_edge(&s, &id).unwrap(),
            "edge {id} identical across adapters"
        );
    }
    // neighbors identical
    let a_id = mem.node_ids(&s).unwrap()[0].clone();
    assert_eq!(
        mem.neighbors(&s, &a_id, Direction::Outgoing).unwrap(),
        file.neighbors(&s, &a_id, Direction::Outgoing).unwrap()
    );
    let _ = std::fs::remove_file(&path);
}
