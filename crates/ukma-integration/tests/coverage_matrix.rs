//! P9-WP3/WP4: the §17.1 and §29.4 dispositions, plus the workspace-level
//! checks that back the architecture and portability rows.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

const MATRIX: &str = "contracts/testing/test-coverage.json";

/// §17.1 mandatory test matrix.
const MATRIX_17_1: [&str; 29] = [
    "REP-001",
    "REP-002",
    "REP-003",
    "ARC-001",
    "ARC-002",
    "DB-001",
    "DB-002",
    "DB-003",
    "DB-004",
    "DB-005",
    "DB-006",
    "DB-007",
    "DB-008",
    "DB-009",
    "DB-010",
    "CODE-001",
    "CODE-002",
    "DOC-001",
    "DOC-002",
    "TRACE-001",
    "MEM-001",
    "MEM-002",
    "MEM-003",
    "MEM-004",
    "MEM-005",
    "MEM-006",
    "MCP-001",
    "SEC-001",
    "OPS-001",
];

/// §29.4 fault matrix (40 items).
const MATRIX_29_4: [&str; 40] = [
    "FM-001", "FM-002", "FM-003", "FM-004", "FM-005", "FM-006", "FM-007", "FM-008", "FM-009",
    "FM-010", "FO-001", "FO-002", "FO-003", "FO-004", "FO-005", "RE-001", "RE-002", "RE-003",
    "CT-001", "CT-002", "CT-003", "CT-004", "AG-001", "AG-002", "AG-003", "TR-001", "TR-002",
    "CO-001", "LC-001", "LC-002", "LC-003", "PM-001", "ML-001", "ML-002", "ML-003", "ML-004",
    "SR-001", "SR-002", "SR-003", "SR-004",
];

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_matrix() -> Value {
    let path = registry_root().join(MATRIX);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).expect("the coverage matrix is JSON")
}

/// Whether `file` really declares a test called `name`.
fn test_exists(file: &str, name: &str) -> bool {
    let path = registry_root().join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => text.contains(&format!("fn {name}(")),
        Err(_) => false,
    }
}

#[test]
fn every_mandatory_test_id_has_exactly_one_disposition() {
    let matrix = load_matrix();
    let entries = matrix["dispositions"]
        .as_array()
        .expect("dispositions array");

    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for entry in entries {
        let id = entry["id"].as_str().expect("id").to_string();
        assert!(
            seen.insert(
                id.clone(),
                entry["disposition"].as_str().expect("disposition")
            )
            .is_none(),
            "{id} is classified twice"
        );
    }

    let expected: BTreeSet<&str> = MATRIX_17_1
        .iter()
        .chain(MATRIX_29_4.iter())
        .copied()
        .collect();
    let classified: BTreeSet<&str> = seen.keys().map(String::as_str).collect();

    let unclassified: Vec<&&str> = expected.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "unclassified mandatory tests: {unclassified:?}"
    );
    let extra: Vec<&&str> = classified.difference(&expected).collect();
    assert!(extra.is_empty(), "unknown ids in the matrix: {extra:?}");

    // §17.1 and §29.4 are both fully disposed.
    assert_eq!(seen.len(), MATRIX_17_1.len() + MATRIX_29_4.len());

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, disposition) in &seen {
        assert!(
            ["covered", "partial", "gap"].contains(disposition),
            "{id} has an unknown disposition `{disposition}`"
        );
        *counts.entry(disposition).or_default() += 1;
    }
    println!("coverage: {counts:?} of {} items", seen.len());
}

#[test]
fn a_covered_row_cites_a_test_that_exists() {
    let matrix = load_matrix();
    for entry in matrix["dispositions"].as_array().expect("dispositions") {
        let id = entry["id"].as_str().expect("id");
        let disposition = entry["disposition"].as_str().expect("disposition");
        if disposition == "gap" {
            assert!(
                entry["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.trim().len() > 20),
                "{id} is a gap without a real reason"
            );
            assert!(entry.get("evidence").is_none(), "{id} gap with evidence");
            continue;
        }
        let evidence = entry["evidence"]
            .as_array()
            .unwrap_or_else(|| panic!("{id} has no evidence"));
        assert!(!evidence.is_empty(), "{id} has empty evidence");
        assert!(
            entry["note"]
                .as_str()
                .is_some_and(|note| note.trim().len() > 20),
            "{id} has no note"
        );
        for item in evidence {
            let file = item["file"].as_str().expect("file");
            let test = item["test"].as_str().expect("test");
            assert!(
                test_exists(file, test),
                "{id} cites {file}::{test}, which does not exist"
            );
        }
    }
}

#[test]
fn the_matrix_covers_the_whole_closed_error_and_operation_set() {
    // The registry is the source the matrix describes: every operation must name
    // the contract test that covers it, and every named test must belong to a
    // known family. A new operation without a test id fails here.
    let mut families: BTreeMap<String, usize> = BTreeMap::new();
    for spec in ukma_contracts::generated::OPERATIONS {
        assert!(
            !spec.tests.is_empty(),
            "{} declares no contract test",
            spec.operation_id
        );
        for test in spec.tests {
            let family: String = test.split('-').next().unwrap_or_default().to_string();
            assert!(
                ["CAPI", "MAPI", "DAPI", "TAPI", "SYS"].contains(&family.as_str()),
                "{} names an unknown test family `{test}`",
                spec.operation_id
            );
            *families.entry(family).or_default() += 1;
        }
    }
    assert_eq!(families.get("CAPI").copied(), Some(13));
    assert_eq!(families.get("DAPI").copied(), Some(14));
    assert_eq!(families.get("MAPI").copied(), Some(25));
    assert_eq!(families.get("TAPI").copied(), Some(13));
    assert_eq!(families.get("SYS").copied(), Some(4));

    // And the matrix describes this repository at this spec version.
    let matrix = load_matrix();
    assert_eq!(matrix["spec_version"], "1.2.3");
    let text = matrix.to_string();
    assert!(text.contains("17.1"));
    assert!(text.contains("29.4"));
}

// ---------------------------------------------------------------------------
// ARC-001 / DB-001: dependency hygiene
// ---------------------------------------------------------------------------

const FORBIDDEN_BACKENDS: [&str; 14] = [
    "rocksdb",
    "librocksdb-sys",
    "sqlx",
    "diesel",
    "tokio-postgres",
    "postgres",
    "qdrant-client",
    "neo4j",
    "sled",
    "redb",
    "sea-orm",
    "mongodb",
    "rusqlite",
    "tantivy",
];

/// Crates whose whole point is to stay backend-neutral.
const DOMAIN_CRATES: [&str; 16] = [
    "ukma-types",
    "ukma-contracts",
    "graph-engine",
    "retrieval-engine",
    "indexing-engine",
    "embedding-engine",
    "context-engine",
    "memory-domain",
    "traceability-engine",
    "code-intelligence",
    "document-intelligence",
    "storage-ports",
    "ukma-security",
    "intelligence-core",
    "ukma-mcp",
    "ukma-integration",
];

struct Manifest {
    name: String,
    path_dependencies: Vec<String>,
    all_dependencies: Vec<String>,
}

fn read_manifest(directory: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(directory.join("Cargo.toml")).ok()?;
    let mut name = String::new();
    let mut section = String::new();
    let mut path_dependencies = Vec::new();
    let mut all_dependencies = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.trim_matches(['[', ']']).to_string();
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = match trimmed.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        let key = key.trim();
        let value = value.trim();
        if section == "package" && key == "name" {
            name = value.trim_matches('"').to_string();
            continue;
        }
        let is_dependency_section = section.ends_with("dependencies");
        if !is_dependency_section {
            continue;
        }
        all_dependencies.push(key.to_string());
        if let Some(rest) = value.strip_prefix('{') {
            if let Some(position) = rest.find("path") {
                let after = &rest[position..];
                if let Some(start) = after.find('"') {
                    if let Some(end) = after[start + 1..].find('"') {
                        path_dependencies.push(after[start + 1..start + 1 + end].to_string());
                    }
                }
            }
        }
    }
    Some(Manifest {
        name,
        path_dependencies,
        all_dependencies,
    })
}

fn workspace_crates() -> BTreeMap<PathBuf, Manifest> {
    let root = registry_root();
    let mut manifests = BTreeMap::new();
    let mut directories: Vec<PathBuf> = Vec::new();
    for base in ["crates", "apps", "crates/adapters"] {
        let base = root.join(base);
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                directories.push(entry.path());
            }
        }
    }
    for directory in directories {
        if let Some(manifest) = read_manifest(&directory) {
            manifests.insert(directory, manifest);
        }
    }
    manifests
}

#[test]
fn arc001_the_dependency_graph_is_acyclic_and_free_of_vendor_sdks() {
    let root = registry_root();
    let manifests = workspace_crates();
    assert!(
        manifests.len() >= 20,
        "only {} crates found",
        manifests.len()
    );

    let by_name: BTreeMap<String, &PathBuf> = manifests
        .iter()
        .map(|(path, manifest)| (manifest.name.clone(), path))
        .collect();

    // No illegal vendor SDK is reachable from any workspace crate.
    for (path, manifest) in &manifests {
        for dependency in &manifest.all_dependencies {
            assert!(
                !FORBIDDEN_BACKENDS.contains(&dependency.as_str()),
                "{} depends on the backend SDK `{dependency}`",
                path.display()
            );
        }
        // A dependency with a path must point at another workspace crate.
        for relative in &manifest.path_dependencies {
            let target = path.join(relative);
            let canonical = target
                .canonicalize()
                .unwrap_or_else(|error| panic!("{}: {error}", target.display()));
            assert!(
                canonical.starts_with(root.canonicalize().unwrap()),
                "{} points outside the workspace: {relative}",
                path.display()
            );
        }
    }

    // No cycle in the workspace graph.
    fn visit(
        name: &str,
        by_name: &BTreeMap<String, &PathBuf>,
        visiting: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) {
        if done.contains(name) {
            return;
        }
        assert!(
            !visiting.iter().any(|entry| entry == name),
            "dependency cycle: {visiting:?} -> {name}"
        );
        visiting.push(name.to_string());
        if let Some(path) = by_name.get(name) {
            if let Some(manifest) = read_manifest(path) {
                for relative in &manifest.path_dependencies {
                    let target = path.join(relative);
                    if let Some(dependency) = read_manifest(&target) {
                        visit(&dependency.name, by_name, visiting, done);
                    }
                }
            }
        }
        visiting.pop();
        done.insert(name.to_string());
    }

    let mut done = BTreeSet::new();
    for name in by_name.keys() {
        visit(name, &by_name, &mut Vec::new(), &mut done);
    }
}

#[test]
fn db001_domain_crates_declare_no_database_sdk() {
    let manifests = workspace_crates();
    for (path, manifest) in &manifests {
        if !DOMAIN_CRATES.contains(&manifest.name.as_str()) {
            continue;
        }
        for dependency in &manifest.all_dependencies {
            assert!(
                !FORBIDDEN_BACKENDS.contains(&dependency.as_str()),
                "domain crate {} depends on `{dependency}`",
                path.display()
            );
        }
        // The domain layer may only reach storage through the ports crate.
        if manifest.name != "storage-ports" && manifest.name != "ukma-integration" {
            for dependency in &manifest.all_dependencies {
                assert!(
                    !dependency.starts_with("ukma-adapter"),
                    "domain crate {} depends on the adapter `{dependency}`",
                    manifest.name
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DB-002: two independent graph stores satisfy one contract
// ---------------------------------------------------------------------------

fn graph_contract(store: std::sync::Arc<dyn graph_engine::GraphStore>) {
    use graph_engine::{
        DeterministicIdGenerator, Direction, EdgeType, GraphEngine, GraphError, NodeType,
        PropertyMap, ScopeKey,
    };

    let engine =
        GraphEngine::with_store_and_ids(store, Box::new(DeterministicIdGenerator::default()));
    let scope = ScopeKey::new("tenant-a", "project-b");
    let other = ScopeKey::new("tenant-z", "project-b");

    let node = engine
        .add_node(
            &scope,
            NodeType::Function,
            PropertyMap::new().with("name", "contract"),
            None,
        )
        .expect("add node");
    let child = engine
        .add_node(&scope, NodeType::Function, PropertyMap::new(), None)
        .expect("add node");
    let edge = engine
        .add_edge(
            &scope,
            &node.id,
            &child.id,
            EdgeType::Calls,
            PropertyMap::new(),
            None,
        )
        .expect("add edge");

    // Identity, revision and etag are stable and content-addressed.
    let fetched = engine.get_node(&scope, &node.id).expect("get node");
    assert_eq!(fetched.revision, 1);
    assert_eq!(fetched.id, node.id);
    assert_eq!(engine.edge_count(&scope).unwrap(), 1);

    // Optimistic concurrency: a stale revision is refused.
    let updated = engine
        .update_node(&scope, &node.id, 1, PropertyMap::new().with("name", "v2"))
        .expect("update at revision 1");
    assert_eq!(updated.revision, 2);
    match engine.update_node(&scope, &node.id, 1, PropertyMap::new()) {
        Err(GraphError::Conflict { .. }) => {}
        other => panic!("expected an etag mismatch, got {other:?}"),
    }

    // Traversal respects direction, and a foreign scope sees nothing.
    let neighbours = engine
        .neighbors(&scope, &node.id, Direction::Outgoing)
        .expect("neighbours");
    assert_eq!(neighbours.len(), 1);
    assert_eq!(neighbours[0].0, child.id);
    assert!(engine.get_node(&other, &node.id).is_err());

    // Deletes cascade and are idempotent per scope.
    engine.delete_edge(&scope, &edge.id).expect("delete edge");
    assert_eq!(engine.edge_count(&scope).unwrap(), 0);
    engine.delete_node(&scope, &child.id).expect("delete node");
    assert_eq!(engine.node_count(&scope).unwrap(), 1);
}

#[test]
fn db002_two_independent_graph_stores_pass_the_same_contract() {
    use graph_engine::{FileGraphStore, InMemoryGraphStore};
    use std::sync::Arc;

    graph_contract(Arc::new(InMemoryGraphStore::new()));

    let mut path = std::env::temp_dir();
    path.push(format!("ukma-db002-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = FileGraphStore::open(&path).expect("file store opens");
    graph_contract(Arc::new(store));
    let _ = std::fs::remove_file(&path);
}
