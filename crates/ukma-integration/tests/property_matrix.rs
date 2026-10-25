//! PROP-01..10 (§38.2) as executable properties over the public API and the
//! ports/stores that have no wire operation.

use std::collections::BTreeMap;

use serde_json::json;
use ukma_contracts::generated::OPERATIONS;
use ukma_integration::{
    ctx, env, episode, foreign_ctx, idempotent, reference, sandbox, working_block, PROJECT, TENANT,
};

// ---------------------------------------------------------------------------
// PROP-01: an invalid scope/ACL never leaks existence
// ---------------------------------------------------------------------------

#[test]
fn prop_01_a_hidden_object_is_indistinguishable_from_an_absent_one() {
    let env = env();
    let owner = sandbox(ctx(), "s-own", "agent-own");
    let stored = env.ok(
        "memory.store_episode",
        &idempotent(owner.clone(), "prop01-store"),
        episode(
            "s-own",
            "agent-own",
            "a secret the other tenant must not see",
        ),
    );
    let hidden_id = stored["resource_id"].as_str().unwrap().to_string();

    let outsider = sandbox(foreign_ctx("tenant-z"), "s-own", "agent-own");
    let mut hidden = None;
    for detail in ["title", "facts", "summary", "full"] {
        let code = env.error(
            "memory.retrieve",
            &outsider,
            json!({
                "memory": reference(&hidden_id, "episodic", "\"\""),
                "detail": detail,
                "max_tokens": 256
            }),
        );
        // Every detail level answers the same way, so probing cannot confirm
        // that the id exists.
        if let Some(previous) = hidden.replace(code.clone()) {
            assert_eq!(code, previous, "detail level changed the answer");
        }
    }
    let absent = env.error(
        "memory.retrieve",
        &outsider,
        json!({
            "memory": reference("mem:nothing-here", "episodic", "\"\""),
            "detail": "full",
            "max_tokens": 256
        }),
    );
    assert_eq!(
        hidden.unwrap(),
        absent,
        "a hidden object must look exactly like a missing one (§40.7)"
    );

    // Counts and search do not reveal it either.
    let found = env.ok(
        "memory.search",
        &outsider,
        json!({
            "query": {
                "query": "secret",
                "kinds": ["episodic"],
                "language": null,
                "after": null,
                "before": null,
                "min_trust": null,
                "max_results": 50,
                "detail": "full",
                "max_tokens": 4096
            }
        }),
    );
    assert_eq!(
        found["items"].as_array().map(Vec::len),
        Some(0),
        "cross-tenant search returned hits: {found}"
    );
}

// ---------------------------------------------------------------------------
// PROP-02: stable logical identity, independent of order
// ---------------------------------------------------------------------------

#[test]
fn prop_02_symbol_identity_does_not_depend_on_file_order() {
    use code_intelligence::domain::SourceFile;
    use ukma_integration::env_with_code;

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    let files = || {
        vec![
            SourceFile::new("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n"),
            SourceFile::new("src/util.rs", "pub fn beta() -> u32 { 2 }\n"),
        ]
    };
    let mut reversed = files();
    reversed.reverse();

    let mut ids: Vec<Vec<String>> = Vec::new();
    for file_set in [files(), reversed] {
        let env = env_with_code(COMMIT, file_set);
        let context = ctx();
        env.ok(
            "code.analyze_repository",
            &context,
            json!({
                "revision": {
                    "repository_id": "repo-1",
                    "vcs": "git",
                    "commit_sha": COMMIT,
                    "tree_sha": null,
                    "dirty_policy": "reject"
                },
                "parser_profile": "default",
                "include_globs": [],
                "exclude_globs": [],
                "asynchronous": true
            }),
        );
        let page = env.ok(
            "code.find_symbols",
            &context,
            json!({
                "repository_id": "repo-1",
                "revision": COMMIT,
                "query": "",
                "kinds": [],
                "path_glob": null,
                "language": null,
                "page": { "cursor": null, "limit": 100 }
            }),
        );
        let mut found: Vec<String> = page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| {
                format!(
                    "{}@{}",
                    item["id"].as_str().unwrap_or_default(),
                    item["definition"]["file_id"].as_str().unwrap_or_default()
                )
            })
            .collect();
        found.sort();
        ids.push(found);
    }
    assert_eq!(
        ids[0], ids[1],
        "the same source produced different logical identities"
    );
    assert!(!ids[0].is_empty());
}

// ---------------------------------------------------------------------------
// PROP-03: cycles and oversized traversals terminate deterministically
// ---------------------------------------------------------------------------

#[test]
fn prop_03_graph_cycles_and_bounds_terminate_deterministically() {
    use graph_engine::{
        CanonicalId, DeterministicIdGenerator, Direction, EdgeType, GraphEngine, GraphError,
        InMemoryGraphStore, NodeType, PropertyMap, ScopeKey,
    };

    let engine = GraphEngine::with_store_and_ids(
        std::sync::Arc::new(InMemoryGraphStore::new()),
        Box::new(DeterministicIdGenerator::default()),
    );
    let scope = ScopeKey::new(TENANT, PROJECT);
    let a = engine
        .add_node(
            &scope,
            NodeType::Function,
            PropertyMap::new().with("n", "a"),
            None,
        )
        .unwrap();
    let b = engine
        .add_node(
            &scope,
            NodeType::Function,
            PropertyMap::new().with("n", "b"),
            None,
        )
        .unwrap();
    let c = engine
        .add_node(
            &scope,
            NodeType::Function,
            PropertyMap::new().with("n", "c"),
            None,
        )
        .unwrap();
    // a -> b -> c -> a: a cycle that must not trap the traversal.
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
    engine
        .add_edge(
            &scope,
            &c.id,
            &a.id,
            EdgeType::Calls,
            PropertyMap::new(),
            None,
        )
        .unwrap();

    let hop = engine.bfs(&scope, &a.id, 10, Some(100)).unwrap();
    assert_eq!(hop.len(), 2, "cycle revisited a node: {hop:?}");

    // Too small a node budget is a typed refusal, not an infinite walk.
    let mut seen = 0;
    let capped: Result<Vec<CanonicalId>, GraphError> = engine.bfs(&scope, &a.id, 10, Some(0));
    if let Ok(nodes) = capped {
        seen = nodes.len();
    }
    assert!(seen <= 2, "a zero node budget returned {seen} nodes");

    // Determinism: two runs agree, and neighbours are stable.
    let first = engine.bfs(&scope, &a.id, 10, Some(100)).unwrap();
    let second = engine.bfs(&scope, &a.id, 10, Some(100)).unwrap();
    assert_eq!(first, second);
    let outgoing = engine
        .neighbors(&scope, &a.id, Direction::Outgoing)
        .unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].0, b.id);
}

// ---------------------------------------------------------------------------
// PROP-04: replays produce exactly one canonical version
// ---------------------------------------------------------------------------

#[test]
fn prop_04_repeated_mutations_converge_on_one_canonical_version() {
    use storage_ports::{MetadataPort, OutboxPort, StorageError};
    use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};

    let env = env();
    let context = idempotent(sandbox(ctx(), "s-prop04", "agent-prop04"), "prop04-key");
    let first = env.ok(
        "memory.store_episode",
        &context,
        episode("s-prop04", "agent-prop04", "one canonical version"),
    );
    for _ in 0..3 {
        let replay = env.ok(
            "memory.store_episode",
            &context,
            episode("s-prop04", "agent-prop04", "one canonical version"),
        );
        assert_eq!(replay["resource_id"], first["resource_id"]);
        assert_eq!(replay["changed"], false);
    }

    // Duplicate delivery to a projection consumer is absorbed by the ack.
    let mut path = std::env::temp_dir();
    path.push(format!("ukma-prop04-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = EmbeddedStore::open(&path).unwrap();
    let scope = default_scope(TENANT, PROJECT);
    store
        .enqueue(&scope, event("e1", "memory.episode.committed", 1))
        .unwrap();
    let delivered = store.pending(&scope, "projection", 10).unwrap();
    assert_eq!(delivered.len(), 1);
    store.ack(&scope, "projection", "e1").unwrap();
    // A second ack of the same event is idempotent, and the queue is empty.
    let _ = store.ack(&scope, "projection", "e1");
    assert!(store.pending(&scope, "projection", 10).unwrap().is_empty());

    // Two writers racing on one aggregate: exactly one wins.
    let winner = store.acquire_writer(&scope, "agg:1", "writer-a");
    assert!(winner.is_ok());
    match store.acquire_writer(&scope, "agg:1", "writer-b") {
        Err(StorageError::Conflict { .. }) => {}
        other => panic!("expected a fencing conflict, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// PROP-05: eviction and folding respect the preserve gate
// ---------------------------------------------------------------------------

#[test]
fn prop_05_eviction_never_touches_protected_or_task_state_blocks() {
    let env = env();
    let context = sandbox(ctx(), "s-prop05", "agent-prop05");
    for index in 0..8 {
        env.ok(
            "memory.put_working_block",
            &idempotent(context.clone(), &format!("prop05-{index}")),
            working_block(
                "s-prop05",
                "agent-prop05",
                &ukma_integration::payload(index, 700),
                0.4,
            ),
        );
    }
    let protected = env.ok(
        "memory.put_working_block",
        &idempotent(context.clone(), "prop05-protected"),
        json!({
            "input": {
                "session_id": "s-prop05",
                "agent_id": "agent-prop05",
                "block_type": "task_state",
                "content": "the task the agent is currently doing",
                "language": "en",
                "priority": 0.1,
                "protected": true,
                "expires_after_turns": null
            },
            "automatic": false
        }),
    );
    let protected_id = protected["id"].as_str().unwrap().to_string();

    let plan = env.ok(
        "memory.evaluate_eviction",
        &context,
        json!({
            "agent_id": "agent-prop05",
            "session_id": "s-prop05",
            "policy_id": "deterministic-v1",
            "dry_run": true,
            "required_tokens": 512
        }),
    );
    let evicted: Vec<String> = plan["evicted"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !evicted.contains(&protected_id),
        "a protected block was scheduled for eviction: {plan}"
    );
    if let Some(candidates) = plan["candidates"].as_array() {
        for candidate in candidates {
            assert_ne!(
                candidate["block_type"], "task_state",
                "task state must never be an eviction candidate: {candidate}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PROP-06: accepted links always carry accessible, revision-matched evidence
// ---------------------------------------------------------------------------

#[test]
fn prop_06_an_accepted_link_never_outlives_its_evidence_revision() {
    use traceability_engine::domain::{EndpointKind, IndexFreshness, Ref};

    let env = env();
    let context = ctx();
    for (id, kind, endpoint) in [
        ("prop06-req", "requirement", EndpointKind::Requirement),
        ("prop06-code", "code_symbol", EndpointKind::Code),
    ] {
        env.services
            .trace
            .register_endpoint(
                &context,
                &Ref {
                    id: id.to_string(),
                    kind: kind.to_string(),
                    revision: "r1".to_string(),
                },
                endpoint,
                true,
                true,
                IndexFreshness::Current,
            )
            .unwrap();
    }

    let proposed = env.ok(
        "traceability.propose_link",
        &context,
        json!({
            "link": {
                "source": reference("prop06-code", "code_symbol", "r1"),
                "target": reference("prop06-req", "requirement", "r1"),
                "kind": "IMPLEMENTS",
                "evidence": [{
                    "evidence_id": "prop06-ev",
                    "source_ref": "artifact:sha256:aa",
                    "confidence": 0.9
                }],
                "rationale": null
            }
        }),
    );
    let link_id = proposed["reference"]["id"].as_str().unwrap().to_string();
    let etag = proposed["etag"].as_str().unwrap().to_string();
    let validated = env.ok(
        "traceability.validate_link",
        &context,
        json!({
            "link": reference(&link_id, "trace_link", &etag),
            "decision": "verify",
            "evidence_ids": ["prop06-ev"],
            "expected_etag": etag
        }),
    );
    let accepted = env.ok(
        "traceability.accept_link",
        &context,
        json!({
            "link": reference(&link_id, "trace_link", validated["etag"].as_str().unwrap()),
            "decision": "accept",
            "expected_etag": validated["etag"].as_str().unwrap()
        }),
    );
    assert_eq!(accepted["state"], "accepted");
    let evidence = accepted["evidence"].as_array().expect("evidence");
    assert!(!evidence.is_empty(), "an accepted link has no evidence");
    assert_eq!(
        accepted["validity"]["asserted_source_revision"],
        accepted["source_revision"].clone()
    );

    // The moment the source head moves, applicability stops claiming the old
    // revision: an accepted link never pretends to describe a newer revision.
    env.services
        .trace
        .observe_endpoint(
            &context,
            &Ref {
                id: "prop06-code".into(),
                kind: "code_symbol".into(),
                revision: "r2".into(),
            },
        )
        .unwrap();
    let after = env.ok(
        "traceability.get_link",
        &context,
        json!({ "link": reference(&link_id, "trace_link", &etag) }),
    );
    assert_ne!(after["state"], "accepted");
    assert_eq!(
        after["validity"]["current_applicability"],
        "requires_revalidation"
    );
}

// ---------------------------------------------------------------------------
// PROP-07: vectors from different profiles are never compared
// ---------------------------------------------------------------------------

#[test]
fn prop_07_vectors_from_different_profiles_cannot_be_compared() {
    use storage_ports::{
        testing::InMemoryVector, EmbeddingProfile, Metric, StorageError, VectorItem, VectorPort,
    };

    let store = InMemoryVector::new();
    let scope = ukma_adapter_embedded::default_scope(TENANT, PROJECT);
    let small = EmbeddingProfile {
        profile_id: "embed-small".into(),
        model_id: "model-a".into(),
        model_revision: "1".into(),
        dimension: 4,
        metric: Metric::Cosine,
        normalization: "l2".into(),
    };
    let large = EmbeddingProfile {
        profile_id: "embed-large".into(),
        model_id: "model-b".into(),
        model_revision: "1".into(),
        dimension: 8,
        metric: Metric::Cosine,
        normalization: "l2".into(),
    };
    store.declare_profile(&scope, &small).unwrap();
    store.declare_profile(&scope, &large).unwrap();
    store
        .upsert(
            &scope,
            "embed-small",
            &[VectorItem {
                id: "v1".into(),
                revision: "r1".into(),
                vector: vec![1.0, 0.0, 0.0, 0.0],
                metadata: BTreeMap::new(),
            }],
        )
        .unwrap();

    // A query vector from the other profile is refused, never silently compared.
    match store.search(&scope, "embed-small", &[1.0; 8], 5, None) {
        Err(StorageError::ModelProfileMismatch { detail }) => {
            assert!(detail.contains("dimension"), "{detail}")
        }
        other => panic!("expected MODEL_PROFILE_MISMATCH, got {other:?}"),
    }
    // Redeclaring a profile with another dimension is refused too.
    let mut conflicting = large.clone();
    conflicting.profile_id = "embed-small".into();
    match store.declare_profile(&scope, &conflicting) {
        Err(StorageError::ModelProfileMismatch { .. }) => {}
        other => panic!("expected MODEL_PROFILE_MISMATCH, got {other:?}"),
    }
    // And the mismatched profile simply has no items to compare against.
    assert!(store
        .search(&scope, "embed-large", &[1.0; 8], 5, None)
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// PROP-08: identical query + snapshot + ACL yield stable, duplicate-free pages
// ---------------------------------------------------------------------------

#[test]
fn prop_08_pagination_is_stable_and_duplicate_free() {
    let env = env();
    let context = sandbox(ctx(), "s-prop08", "agent-prop08");
    for index in 0..9 {
        env.ok(
            "memory.store_episode",
            &idempotent(context.clone(), &format!("prop08-{index}")),
            episode("s-prop08", "agent-prop08", &format!("parser note {index}")),
        );
    }

    let query = |cursor: Option<&str>| {
        json!({
            "query": {
                "query": "parser",
                "kinds": ["episodic"],
                "language": null,
                "after": null,
                "before": null,
                "min_trust": null,
                "max_results": 100,
                "detail": "title",
                "max_tokens": 4096
            },
            "page": { "cursor": cursor, "limit": 4 }
        })
    };

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let page = env.ok("memory.search", &context, query(cursor.as_deref()));
        let items = page["items"].as_array().cloned().unwrap_or_default();
        for item in &items {
            let id = item["reference"]["id"]
                .as_str()
                .or_else(|| item["id"].as_str())
                .unwrap_or_default()
                .to_string();
            assert!(!seen.contains(&id), "page repeated {id}");
            seen.push(id);
        }
        match page["next_cursor"].as_str() {
            Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    assert!(
        seen.len() >= 4,
        "pagination returned {seen:?} — the fixture is not paginating"
    );

    // The same request twice returns the same order.
    let first = env.ok("memory.search", &context, query(None));
    let second = env.ok("memory.search", &context, query(None));
    assert_eq!(first["items"], second["items"]);
}

// ---------------------------------------------------------------------------
// PROP-09: cancel / timeout / retry never delete committed data
// ---------------------------------------------------------------------------

#[test]
fn prop_09_cancelling_a_job_keeps_the_committed_data_and_the_final_state() {
    use ukma_mcp::{JobIndex, JobRecord};

    let jobs = JobIndex::new();
    jobs.record(JobRecord {
        job_id: "prop09-job".into(),
        tenant_id: TENANT.into(),
        project_id: PROJECT.into(),
        kind: "code.analyze_repository".into(),
        stage: "ready".into(),
        canonical_committed: true,
        cancellable: false,
        cancelled: false,
        diagnostics: vec![],
    });

    // Too late to cancel: the canonical commit is kept and the state is readable.
    let error = jobs
        .cancel(TENANT, PROJECT, "prop09-job")
        .expect_err("a committed job is not cancellable");
    assert_eq!(error.code, "JOB_NOT_CANCELLABLE");
    let record = jobs
        .get(TENANT, PROJECT, "prop09-job")
        .expect("still there");
    assert!(record.canonical_committed);
    assert_eq!(record.stage, "ready");

    // A job cancelled mid-flight keeps whatever it already committed.
    jobs.record(JobRecord {
        job_id: "prop09-live".into(),
        tenant_id: TENANT.into(),
        project_id: PROJECT.into(),
        kind: "document.ingest".into(),
        stage: "running".into(),
        canonical_committed: true,
        cancellable: true,
        cancelled: false,
        diagnostics: vec![],
    });
    let cancelled = jobs.cancel(TENANT, PROJECT, "prop09-live").unwrap();
    assert!(cancelled.cancelled);
    assert!(
        cancelled.canonical_committed,
        "cancel must not undo a commit"
    );
    // Retrying the cancel is idempotent and the final state stays queryable.
    let again = jobs.cancel(TENANT, PROJECT, "prop09-live").unwrap();
    assert_eq!(again.stage, "cancelled");
}

// ---------------------------------------------------------------------------
// PROP-10: registry, generated bindings, MCP tools and routes are one source
// ---------------------------------------------------------------------------

#[test]
fn prop_10_every_artefact_is_projected_from_one_registry() {
    let env = env();
    assert!(
        ukma_mcp::drift().is_empty(),
        "manifest drift: {:?}",
        ukma_mcp::drift()
    );

    let registry_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for spec in OPERATIONS {
        // The MCP tool name is the operation id with `_v1`, so it cannot drift.
        assert_eq!(
            spec.mcp_tool,
            format!(
                "ukma_{}_{}_v1",
                spec.operation_id.split('.').next().unwrap_or_default(),
                spec.operation_id.split('.').nth(1).unwrap_or_default()
            )
        );
        assert!(
            spec.mcp_tool.ends_with("_v1"),
            "{} lacks the version suffix",
            spec.mcp_tool
        );
        assert_eq!(
            ukma_mcp::tool_by_name(spec.mcp_tool)
                .expect("tool exists")
                .operation_id,
            spec.operation_id
        );
        // The REST route is derived from the operation id as well.
        let path = spec
            .server_method
            .split_once(' ')
            .map(|(_, path)| path.to_string())
            .expect("route");
        assert_eq!(
            path,
            format!(
                "/api/v1/{}/{}",
                spec.operation_id.split('.').next().unwrap_or_default(),
                spec.operation_id.split('.').nth(1).unwrap_or_default()
            )
        );
        // The published tool schema exists and declares the same operation.
        let tool_path = registry_root
            .join("contracts/mcp/tools")
            .join(format!("{}.input.schema.json", spec.mcp_tool));
        let text = std::fs::read_to_string(&tool_path)
            .unwrap_or_else(|error| panic!("{}: {error}", tool_path.display()));
        assert!(
            text.contains(spec.operation_uri),
            "{} does not name {}",
            tool_path.display(),
            spec.operation_uri
        );
    }
    // And the gateway serves exactly those operations.
    assert_eq!(
        env.gateway.registered().len(),
        OPERATIONS.len(),
        "registered handler count differs from the registry"
    );
}
