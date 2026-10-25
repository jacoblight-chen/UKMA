//! One assertion over the four profile-C ports at once (ADR-062).
//!
//! Profile C is not "PostgreSQL plus two services": it is one deployment where the
//! canonical store, the vector store and the graph store must agree about the same
//! write. This test drives all four ports in a single run and compares the
//! observable result of every backend against the same expectation:
//!
//! - canonical: embedded, SQLite and PostgreSQL produce identical revisions,
//!   etags and pages for identical writes, and identical outbox order;
//! - vector: Qdrant answers the same nearest neighbour as the reference;
//! - graph: Neo4j stores the same nodes/edges and the same traversal answer.
//!
//! Without the three environment variables the run reports a skip: a machine with
//! no services must not fail the build, and must not claim the parity either.

use std::collections::BTreeMap;

use graph_engine::ids::{CanonicalId, IdKind, Ulid};
use graph_engine::store::{GraphStore, ScopeKey};
use graph_engine::types::{EdgeType, GraphEdge, GraphNode, NodeType, PropertyMap};
use serde_json::json;
use storage_ports::ports::{EmbeddingProfile, Metric, VectorItem, VectorPort};
use storage_ports::{
    MetadataPort, OutboxEvent, OutboxPort, PortResult, StorageError, TransactionScope,
};
use ukma_adapter_embedded::EmbeddedStore;
use ukma_adapter_neo4j::{Neo4jStore, PASSWORD_ENV, URL_ENV as NEO4J_URL_ENV};
use ukma_adapter_postgres::{PostgresStore, DSN_ENV};
use ukma_adapter_qdrant::{QdrantStore, URL_ENV as QDRANT_URL_ENV};
use ukma_adapter_sqlite::SqliteStore;
use ukma_contracts::ScopeRef;

fn scope(project: &str) -> ScopeRef {
    ScopeRef {
        tenant_id: "tenant-parity".into(),
        project_id: project.into(),
        repo_id: None,
        workspace_id: None,
        agent_id: None,
        session_id: None,
    }
}

fn event(id: &str, revision: u64) -> OutboxEvent {
    OutboxEvent {
        event_id: id.to_string(),
        event_type: "memory.updated".to_string(),
        event_version: 1,
        aggregate_ref: format!("agg:{id}"),
        aggregate_revision: revision,
        payload: json!({"event": id}),
        idempotency_key: None,
    }
}

/// One canonical backend, seen only through the two ports profile C must serve.
trait Canonical: MetadataPort + OutboxPort {}
impl Canonical for EmbeddedStore {}
impl Canonical for SqliteStore {}
impl Canonical for PostgresStore {}

/// The write set every canonical backend must reproduce exactly.
fn canonical_observations(store: &dyn Canonical, scope: &ScopeRef) -> PortResult<Vec<String>> {
    let mut seen = Vec::new();
    for index in 0..4 {
        let key = format!("canon:{index}");
        let entry = store.put_cas(scope, &key, json!({"i": index}), None)?;
        seen.push(format!("{key} r{} {}", entry.revision, entry.etag));
    }
    // A stale compare-and-swap must behave identically everywhere.
    let stale = store.put_cas(scope, "canon:0", json!({"i": 99}), Some(3));
    seen.push(format!(
        "stale={}",
        match stale {
            Ok(entry) => format!("updated r{}", entry.revision),
            Err(error) => error.code().to_string(),
        }
    ));
    let (page, cursor) = store.list(scope, "canon:", 3, None)?;
    for entry in &page {
        seen.push(format!(
            "page {} r{} {}",
            entry.key, entry.revision, entry.etag
        ));
    }
    seen.push(format!(
        "cursor={}",
        cursor.unwrap_or_else(|| "<none>".to_string())
    ));
    store.put_manifest(scope, "code", "gen-1")?;
    store.put_manifest(scope, "code", "gen-2")?;
    seen.push(format!(
        "manifest={}",
        store
            .get_manifest(scope, "code")?
            .unwrap_or_else(|| "<none>".into())
    ));
    for id in ["evt-1", "evt-2", "evt-1"] {
        store.enqueue(scope, event(id, 1))?;
    }
    let pending = store.pending(scope, "consumer-a", 10)?;
    seen.push(format!(
        "pending={:?}",
        pending
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>()
    ));
    store.ack(scope, "consumer-a", "evt-1")?;
    seen.push(format!(
        "after-ack={:?}",
        store
            .pending(scope, "consumer-a", 10)?
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>()
    ));
    Ok(seen)
}

fn graph_node(id: &CanonicalId) -> GraphNode {
    GraphNode {
        id: id.clone(),
        node_type: NodeType::Function,
        properties: PropertyMap::new(),
        revision: 1,
        etag: "e1".into(),
    }
}

fn graph_edge(id: &CanonicalId, from: &CanonicalId, to: &CanonicalId) -> GraphEdge {
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

#[test]
fn the_four_profile_c_ports_agree_in_one_run() {
    let postgres_dsn = std::env::var(DSN_ENV)
        .ok()
        .filter(|dsn| !dsn.trim().is_empty());
    let qdrant_url = std::env::var(QDRANT_URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty());
    let neo4j_url = std::env::var(NEO4J_URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty());
    if postgres_dsn.is_none() || qdrant_url.is_none() || neo4j_url.is_none() {
        eprintln!(
            "skipped: needs {DSN_ENV}, {QDRANT_URL_ENV} and {NEO4J_URL_ENV} (tools/profile-c/start.ps1)"
        );
        return;
    }

    let mut root = std::env::temp_dir();
    root.push(format!("ukma-3way-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // ---------------------------------------------------------- canonical ---
    let scope = scope("proj-three-way");
    let embedded = EmbeddedStore::open(root.join("store.json")).unwrap();
    let sqlite = SqliteStore::open(root.join("store.db")).unwrap();
    let postgres = PostgresStore::connect(postgres_dsn.as_deref().unwrap()).unwrap();
    postgres.purge_scope(&scope).unwrap();

    let from_embedded = canonical_observations(&embedded, &scope).unwrap();
    let from_sqlite = canonical_observations(&sqlite, &scope).unwrap();
    let from_postgres = canonical_observations(&postgres, &scope).unwrap();
    assert_eq!(
        from_embedded, from_sqlite,
        "embedded and SQLite disagree about the same write set"
    );
    assert_eq!(
        from_embedded, from_postgres,
        "embedded and PostgreSQL disagree about the same write set"
    );
    assert!(
        from_postgres
            .iter()
            .any(|line| line == "stale=ETAG_MISMATCH"),
        "the stale CAS must be refused by every backend"
    );

    // ------------------------------------------------------------- vector ---
    let profile = EmbeddingProfile {
        profile_id: "three-way".into(),
        model_id: "bge-m3".into(),
        model_revision: "2024-01".into(),
        dimension: 4,
        metric: Metric::Cosine,
        normalization: "l2".into(),
    };
    let qdrant = QdrantStore::connect(qdrant_url.as_deref().unwrap(), None).unwrap();
    VectorPort::declare_profile(&qdrant, &scope, &profile).unwrap();
    // The reference answer: cosine similarity of the same three vectors.
    let item = |id: &str, vector: Vec<f32>, topic: &str| {
        let mut metadata = BTreeMap::new();
        metadata.insert("topic".to_string(), topic.to_string());
        VectorItem {
            id: id.to_string(),
            revision: "r1".to_string(),
            vector,
            metadata,
        }
    };
    VectorPort::upsert(
        &qdrant,
        &scope,
        "three-way",
        &[
            item("a", vec![1.0, 0.0, 0.0, 0.0], "alpha"),
            item("b", vec![0.0, 1.0, 0.0, 0.0], "beta"),
            item("c", vec![0.9, 0.1, 0.0, 0.0], "alpha"),
        ],
    )
    .unwrap();
    let hits =
        VectorPort::search(&qdrant, &scope, "three-way", &[1.0, 0.0, 0.0, 0.0], 3, None).unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        vec!["a", "c", "b"],
        "Qdrant must answer the same neighbour order as the reference"
    );
    assert!(hits[0].score > 0.99);
    let mut filter = BTreeMap::new();
    filter.insert("topic".to_string(), "beta".to_string());
    let filtered = VectorPort::search(
        &qdrant,
        &scope,
        "three-way",
        &[1.0, 0.0, 0.0, 0.0],
        5,
        Some(&filter),
    )
    .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "b");

    // -------------------------------------------------------------- graph ---
    let graph_scope = ScopeKey::new("tenant-parity", "proj-three-way");
    let password = std::env::var(PASSWORD_ENV).unwrap_or_default();
    let neo4j =
        Neo4jStore::connect(neo4j_url.as_deref().unwrap(), "neo4j", &password, "neo4j").unwrap();
    neo4j.purge_scope(&graph_scope).unwrap();
    let a = id(IdKind::Node, 21);
    let b = id(IdKind::Node, 22);
    let calls = id(IdKind::Edge, 23);
    neo4j.put_node(&graph_scope, &graph_node(&a)).unwrap();
    neo4j.put_node(&graph_scope, &graph_node(&b)).unwrap();
    neo4j
        .put_edge(&graph_scope, &graph_edge(&calls, &a, &b))
        .unwrap();
    assert_eq!(
        neo4j.node_ids(&graph_scope).unwrap(),
        vec![a.clone(), b.clone()]
    );
    assert_eq!(
        neo4j.outgoing_edge_ids(&graph_scope, &a).unwrap(),
        vec![calls.clone()]
    );
    assert_eq!(
        neo4j.incoming_edge_ids(&graph_scope, &b).unwrap(),
        vec![calls.clone()]
    );
    neo4j.delete_node(&graph_scope, &a).unwrap();
    assert!(
        neo4j.get_edge(&graph_scope, &calls).unwrap().is_none(),
        "the cascade must reach the graph store too"
    );

    // ------------------------------------------------------------ cleanup ---
    postgres.purge_scope(&scope).unwrap();
    neo4j.purge_scope(&graph_scope).unwrap();
    let _ = std::fs::remove_dir_all(&root);

    // The four ports answered in one run; a failure above would have stopped it.
    let _ = StorageError::InvalidArgument {
        field: "unused",
        reason: "keeps the import honest".into(),
    };
    assert_eq!(TransactionScope::MultiAggregate as u8, 2);
}
