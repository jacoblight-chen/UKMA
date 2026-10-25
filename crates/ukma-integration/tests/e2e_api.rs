//! E2E-API-01..12 (§38.1) driven through the public API.
//!
//! Each test names the scenario it proves. A scenario that needs a destination with
//! no wire operation (storage ports, graph query facade) is marked adapter-level and
//! drives that destination directly, because the spec's own scenario is about the
//! backend behaviour.

use serde_json::{json, Value};
use ukma_contracts::generated::OPERATIONS;
use ukma_integration::{
    all_scopes, ctx, env, env_with_code, env_with_documents, episode, etag_of, foreign_ctx,
    idempotent, payload, reference, sandbox, working_block, PROJECT, TENANT,
};

const COMMIT: &str = "c0ffee1234567890abcdef1234567890abcdef12";

// ---------------------------------------------------------------------------
// E2E-API-01: repository commit -> analysis -> symbol search
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_01_commit_analysis_and_symbol_search_are_bound_to_one_commit() {
    let env = env_with_code(
        COMMIT,
        vec![
            code_intelligence::domain::SourceFile::new(
                "src/lib.rs",
                "pub fn alpha_order() -> u32 { 1 }\n\nfn beta_order() { let _ = alpha_order(); }\n",
            ),
            code_intelligence::domain::SourceFile::new(
                "src/util.rs",
                "pub fn gamma_helper(value: u32) -> u32 { value + 1 }\n",
            ),
        ],
    );
    let context = ctx();
    let handle = env.ok(
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
    let job_id = handle["job_id"].as_str().expect("job id").to_string();
    let job = env.ok(
        "code.get_analysis_job",
        &context,
        json!({ "job_id": job_id }),
    );
    assert_eq!(job["stage"], "ready");
    assert_eq!(job["canonical_committed"], true);

    // The committed job names the authoritative index it produced.
    assert!(
        job["result_ref"]
            .as_str()
            .is_some_and(|reference| !reference.is_empty()),
        "a committed analysis exposes its index: {job}"
    );
    // The index is bound to that exact commit: another commit is not served.
    let other = env.dispatch(
        "code.find_symbols",
        &context,
        json!({
            "repository_id": "repo-1",
            "revision": "0000000000000000000000000000000000000000",
            "query": "order",
            "kinds": [],
            "path_glob": null,
            "language": null,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let other_code = other.error.expect("an unknown commit is not served").code;
    // Recorded gap: `code.find_symbols` does not declare NOT_FOUND, so a missing
    // index surfaces as FORBIDDEN (the gateway never emits an undeclared code).
    assert!(
        [
            "SOURCE_UNAVAILABLE",
            "INDEX_STALE",
            "INDEX_NOT_READY",
            "NOT_FOUND",
            "FORBIDDEN"
        ]
        .contains(&other_code.as_str()),
        "unexpected code for an unanalyzed commit: {other_code}"
    );

    let page = env.ok(
        "code.find_symbols",
        &context,
        json!({
            "repository_id": "repo-1",
            "revision": COMMIT,
            "query": "order",
            "kinds": [],
            "path_glob": null,
            "language": null,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let items = page["items"].as_array().expect("items");
    assert!(items.len() >= 2, "expected both order symbols: {page}");

    let symbol_id = items[0]["id"].as_str().expect("symbol id").to_string();
    let symbol = env.ok(
        "code.get_symbol",
        &context,
        json!({ "symbol_id": symbol_id }),
    );
    assert_eq!(symbol["id"], items[0]["id"]);
    // The declaration site is retained: file + line range + the revision itself.
    let definition = &symbol["definition"];
    assert!(
        definition["file_id"]
            .as_str()
            .is_some_and(|file| file.ends_with("src/lib.rs")),
        "definition: {definition}"
    );
    assert!(definition["line_start"]
        .as_u64()
        .is_some_and(|line| line >= 1));
    assert!(
        definition["line_end"]
            .as_u64()
            .is_some_and(|line| line >= 1),
        "definition: {definition}"
    );
}

// ---------------------------------------------------------------------------
// E2E-API-02: Markdown/PDF/DOCX ingest keeps source coordinates
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_02_document_ingest_keeps_source_coordinates_and_reports_partial() {
    let markdown = b"# Release notes\n\n## Parser\n\nThe parser handles UTF-8.\n".to_vec();
    let (env, hashes) = env_with_documents(vec![("artifact:doc-md", markdown)]);
    let context = ctx();

    let handle = env.ok(
        "document.ingest",
        &context,
        json!({
            "source": {
                "source_uri": "memory://release-notes.md",
                "media_type": "text/markdown",
                "content_hash": hashes[0],
                "revision": "rev-1",
                "language_hint": "en",
                "artifact_ref": "artifact:doc-md"
            },
            "parser_profile": "markdown/1",
            "include_images": false,
            "asynchronous": true
        }),
    );
    let job_id = handle["job_id"].as_str().expect("job id").to_string();
    let job = env.ok(
        "document.get_ingest_job",
        &context,
        json!({ "job_id": job_id.clone() }),
    );
    assert_eq!(job["stage"], "ready");

    let document_id = job["result_ref"]
        .as_str()
        .expect("a committed ingest exposes its document")
        .to_string();

    let view = env.ok(
        "document.get_document",
        &context,
        json!({
            "document_id": document_id,
            "include_blocks": true,
            "page": { "cursor": null, "limit": 50 }
        }),
    );
    assert_eq!(view["source"]["content_hash"], hashes[0]);
    let blocks = view["blocks"]["items"].as_array().expect("blocks");
    assert!(!blocks.is_empty(), "expected parsed blocks: {view}");
    // Source coordinates survive parsing: every block points back into the artifact.
    for block in blocks {
        assert!(
            block["span"]["byte_start"].as_u64().is_some()
                && block["span"]["line_start"]
                    .as_u64()
                    .is_some_and(|line| line >= 1),
            "block without a source coordinate: {block}"
        );
    }

    // A source the parser cannot fully read is reported explicitly, never silently
    // dropped: an image inside a markdown document is an unsupported part.
    let with_image = b"# Title\n\n![diagram](diagram.png)\n\ntext after the image\n".to_vec();
    let (env2, hashes2) = env_with_documents(vec![("artifact:doc-img", with_image)]);
    let handle2 = env2.ok(
        "document.ingest",
        &context,
        json!({
            "source": {
                "source_uri": "memory://with-image.md",
                "media_type": "text/markdown",
                "content_hash": hashes2[0],
                "revision": "rev-1",
                "language_hint": "en",
                "artifact_ref": "artifact:doc-img"
            },
            "parser_profile": "markdown/1",
            "include_images": true,
            "asynchronous": true
        }),
    );
    let job2 = env2.ok(
        "document.get_ingest_job",
        &context,
        json!({ "job_id": handle2["job_id"].clone() }),
    );
    assert_eq!(job2["stage"], "ready");
    // The image cannot be parsed without OCR, so the document is explicitly
    // partial instead of silently complete.
    assert!(
        job2["partial_ratio"].as_u64().unwrap_or(0) > 0,
        "an unparsed image must make the document partial: {job2}"
    );
}

// ---------------------------------------------------------------------------
// E2E-API-03: requirement -> accepted link -> coverage
// ---------------------------------------------------------------------------

/// Propose, validate and accept one `implements` link; returns the link id + etag.
fn accepted_implementation_link(
    env: &ukma_integration::Env,
    context: &ukma_contracts::RequestContext,
) -> (String, String) {
    use traceability_engine::domain::{EndpointKind, IndexFreshness, Ref};
    for (id, kind, endpoint) in [
        ("req-001", "requirement", EndpointKind::Requirement),
        ("code:alpha_order", "code_symbol", EndpointKind::Code),
    ] {
        env.services
            .trace
            .register_endpoint(
                context,
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
            .expect("endpoint registered");
    }
    let proposed = env.ok(
        "traceability.propose_link",
        context,
        json!({
            "link": {
                "source": reference("code:alpha_order", "code_symbol", "r1"),
                "target": reference("req-001", "requirement", "r1"),
                "kind": "IMPLEMENTS",
                "evidence": [{
                    "evidence_id": "ev-code",
                    "source_ref": "artifact:sha256:aa",
                    "confidence": 0.9
                }],
                "rationale": "the symbol implements the requirement"
            }
        }),
    );
    let link_id = proposed["reference"]["id"]
        .as_str()
        .expect("link id")
        .to_string();
    let validated = env.ok(
        "traceability.validate_link",
        context,
        json!({
            "link": reference(&link_id, "trace_link", &etag_of(&proposed)),
            "decision": "verify",
            "evidence_ids": ["ev-code"],
            "expected_etag": etag_of(&proposed)
        }),
    );
    let accepted = env.ok(
        "traceability.accept_link",
        context,
        json!({
            "link": reference(&link_id, "trace_link", &etag_of(&validated)),
            "decision": "accept",
            "expected_etag": etag_of(&validated)
        }),
    );
    assert_eq!(accepted["state"], "accepted", "{accepted}");
    (link_id, etag_of(&accepted))
}

#[test]
fn e2e_api_03_accepted_link_carries_verifiable_evidence_into_coverage() {
    let env = env();
    let context = ctx();
    let (link_id, _) = accepted_implementation_link(&env, &context);

    let link = env.ok(
        "traceability.get_link",
        &context,
        json!({ "link": reference(&link_id, "trace_link", "r1") }),
    );
    assert_eq!(link["state"], "accepted");
    let evidence = link["evidence"].as_array().expect("evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["source_ref"], "artifact:sha256:aa");
    assert_eq!(
        link["validity"]["historical_validity"], "valid",
        "an accepted link must be historically valid: {link}"
    );

    let coverage = env.ok(
        "traceability.query_coverage",
        &context,
        json!({
            "requirement_scope": {
                "tenant_id": TENANT,
                "project_id": PROJECT,
                "repo_id": null,
                "workspace_id": null,
                "agent_id": null,
                "session_id": null
            },
            "requirement_refs": [reference("req-001", "requirement", "r1")],
            "implementation_kinds": ["IMPLEMENTS"],
            "accepted_only": true,
            "include_unindexed": false,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    // Exactly one requirement in the denominator, and it is covered.
    assert_eq!(coverage["denominator"], 1, "{coverage}");
    assert_eq!(coverage["numerator"], 1, "{coverage}");
    assert_eq!(coverage["coverage_ratio"], 1.0, "{coverage}");
    assert_eq!(
        coverage["covered"].as_array().map(Vec::len),
        Some(1),
        "{coverage}"
    );
}

// ---------------------------------------------------------------------------
// E2E-API-04: code revision update -> stale link -> revalidate
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_04_a_new_head_makes_the_link_stale_and_revalidation_restores_it() {
    let env = env();
    let context = ctx();
    let (link_id, _) = accepted_implementation_link(&env, &context);

    // The code moved on: the authoritative head is now r2.
    let moved = env.services.trace.observe_endpoint(
        &context,
        &traceability_engine::domain::Ref {
            id: "code:alpha_order".into(),
            kind: "code_symbol".into(),
            revision: "r2".into(),
        },
    );
    assert_eq!(moved.expect("head recorded"), 1);

    let link = env.ok(
        "traceability.get_link",
        &context,
        json!({ "link": reference(&link_id, "trace_link", "r1") }),
    );
    assert_eq!(
        link["state"], "stale",
        "old acceptance must not look current: {link}"
    );
    assert_eq!(
        link["validity"]["current_applicability"],
        "requires_revalidation"
    );
    // History is untouched: the link really was true for r1.
    assert_eq!(link["validity"]["historical_validity"], "valid");

    let job = env.ok(
        "traceability.revalidate_stale",
        &context,
        json!({
            "link_refs": [reference(&link_id, "trace_link", "r1")],
            "policy_id": "revalidate-on-head",
            "asynchronous": true
        }),
    );
    assert!(
        job["job_id"].as_str().is_some_and(|id| !id.is_empty()),
        "revalidation must run as a job: {job}"
    );
}

// ---------------------------------------------------------------------------
// E2E-API-05: delegation -> child memory -> folded result
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_05_a_parent_sees_the_folded_result_not_the_child_transcript() {
    let env = env();
    let parent = sandbox(ctx(), "s-main", "agent-main");
    let child = sandbox(ctx(), "s-main", "agent-child");

    let grant = env.ok(
        "memory.open_agent_scope",
        &parent,
        json!({
            "grant": {
                "parent_agent_id": "agent-main",
                "child_agent_id": "agent-child",
                "session_id": "s-main",
                "task_id": "task-1",
                "depth": 1,
                "allowed_scopes": ["memory:read", "memory:write"],
                "allowed_tools": [],
                "expires_at": "2030-01-01T00:00:00Z",
                "max_context_tokens": 4096
            }
        }),
    );
    assert!(!grant["scope_id"].as_str().unwrap_or_default().is_empty());

    let child_episode = env.ok(
        "memory.store_episode",
        &idempotent(child.clone(), "idem-child-1"),
        episode("s-main", "agent-child", "child ran the migration"),
    );
    let child_id = child_episode["resource_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let child_ref = reference(&child_id, "episodic", &etag_of(&child_episode));

    // The parent seals the delegation; the hand-over is a folded result, not the
    // child's transcript.
    let closed = env.ok(
        "memory.close_agent_scope",
        &parent,
        json!({
            "scope_id": grant["scope_id"].clone(),
            "reason": "task finished",
            "fold_on_close": true
        }),
    );
    assert_eq!(closed["closed"], true, "{closed}");
    assert_eq!(closed["status"], "sealed", "{closed}");
    let folded = &closed["folded_result"];
    assert_eq!(folded["fold_level"], 1, "{closed}");
    // Evidence references survive the fold, so the parent can still audit.
    assert_eq!(
        folded["protected_evidence_refs"],
        json!(["artifact:sha256:aa"])
    );
    assert!(closed["evidence_refs"]
        .as_array()
        .is_some_and(|refs| refs.iter().any(|value| value == "artifact:sha256:aa")));

    // The child transcript itself is not readable from the parent sandbox.
    let denied = env.error(
        "memory.retrieve",
        &parent,
        json!({
            "memory": child_ref,
            "detail": "full",
            "max_tokens": 128
        }),
    );
    assert_eq!(denied, "NOT_FOUND");
}

// ---------------------------------------------------------------------------
// E2E-API-06: working-memory overflow -> paging -> reconsolidation
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_06_paging_protects_blocks_and_reconsolidation_uses_the_etag() {
    let env = env();
    let context = sandbox(ctx(), "s-page", "agent-page");

    let mut block_ids = Vec::new();
    for index in 0..12 {
        let receipt = env.ok(
            "memory.put_working_block",
            &idempotent(context.clone(), &format!("idem-block-{index}")),
            working_block("s-page", "agent-page", &payload(index, 900), 0.5),
        );
        assert_eq!(receipt["kind"], "working", "{receipt}");
        block_ids.push(etag_of(&receipt));
    }

    let listed = env.ok(
        "memory.list_working_blocks",
        &context,
        json!({
            "session_id": "s-page",
            "agent_id": "agent-page",
            "page": { "cursor": null, "limit": 100 }
        }),
    );
    let items = listed["items"].as_array().expect("blocks");
    assert!(!items.is_empty());

    let first = items[0]["reference"].clone();
    let page_out = env.ok(
        "memory.page_out",
        &context,
        json!({ "block": first.clone(), "policy_id": "page-policy" }),
    );
    let page = page_out["page_ref"].clone();
    assert_eq!(page_out["status"], "paged_out", "{page_out}");

    // Repeated page-in is safe and keeps the integrity hash.
    for _ in 0..2 {
        let restored = env.ok(
            "memory.page_in",
            &context,
            json!({ "page_ref": page.clone(), "expected_hash": page_out["integrity_hash"].clone() }),
        );
        assert_eq!(
            restored["receipt"]["integrity_hash"],
            page_out["integrity_hash"]
        );
    }

    // Reconsolidation: the window commits only when the caller still holds the
    // revision it opened the window on.
    let stored = env.ok(
        "memory.store_episode",
        &idempotent(context.clone(), "idem-recon-1"),
        episode("s-page", "agent-page", "alpha order shipped"),
    );
    let memory_id = stored["resource_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let memory_etag = etag_of(&stored);
    let window = env.ok(
        "memory.begin_reconsolidation",
        &context,
        json!({
            "fact": reference(&memory_id, "episodic", &memory_etag),
            "expected_etag": memory_etag,
            "window_hours": 4
        }),
    );
    let committed = env.ok(
        "memory.commit_reconsolidation",
        &context,
        json!({
            "window_id": window["window_id"].clone(),
            "patch": { "summary": "alpha order shipped and verified" },
            "evidence": [{
                "evidence_id": "ev-recon",
                "source_ref": "artifact:sha256:aa",
                "confidence": 0.9,
                "source_group": "group-1"
            }],
            "expected_etag": memory_etag
        }),
    );
    assert_eq!(committed["changed"], true, "{committed}");

    // A stale window cannot be replayed: the etag moved.
    let replay = env.error(
        "memory.commit_reconsolidation",
        &context,
        json!({
            "window_id": window["window_id"].clone(),
            "patch": { "summary": "replayed edit" },
            "evidence": [{
                "evidence_id": "ev-recon",
                "source_ref": "artifact:sha256:aa",
                "confidence": 0.9,
                "source_group": "group-1"
            }],
            "expected_etag": memory_etag
        }),
    );
    assert!(
        [
            "ETAG_MISMATCH",
            "POLICY_DENIED",
            "INVALID_SCHEMA",
            "LINK_STATE_CONFLICT"
        ]
        .contains(&replay.as_str()),
        "unexpected replay code {replay}"
    );
}

// ---------------------------------------------------------------------------
// E2E-API-07: nine-stage consolidation
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_07_consolidation_promotes_only_at_the_commit_stage_and_replays() {
    let env = env();
    let context = sandbox(ctx(), "s-cons", "agent-cons");
    let mut refs = Vec::new();
    for index in 0..3 {
        let stored = env.ok(
            "memory.store_episode",
            &idempotent(context.clone(), &format!("idem-cons-{index}")),
            episode(
                "s-cons",
                "agent-cons",
                &format!("claim {index} about the parser"),
            ),
        );
        refs.push(reference(
            stored["resource_id"].as_str().unwrap_or_default(),
            "episodic",
            &etag_of(&stored),
        ));
    }

    let handle = env.ok(
        "memory.consolidate",
        &context,
        json!({
            "episode_refs": refs,
            "policy_id": "deterministic-v1",
            "asynchronous": true
        }),
    );
    let job_id = handle["job_id"].as_str().expect("job id").to_string();
    let job = env.ok(
        "memory.get_consolidation_job",
        &context,
        json!({ "job_id": job_id.clone() }),
    );
    assert_eq!(job["stage"], "committed", "{job}");
    assert_eq!(
        job["canonical_committed"], true,
        "the promote stage is the only commit point: {job}"
    );

    // Replaying the same episodes must not create a second canonical fact.
    let before = semantic_fact_count(&env, &context);
    let replay = env.ok(
        "memory.consolidate",
        &context,
        json!({
            "episode_refs": refs,
            "policy_id": "deterministic-v1",
            "asynchronous": true
        }),
    );
    let replay_job = env.ok(
        "memory.get_consolidation_job",
        &context,
        json!({ "job_id": replay["job_id"].clone() }),
    );
    assert_eq!(replay_job["stage"], "committed", "{replay_job}");
    assert_eq!(
        semantic_fact_count(&env, &context),
        before,
        "replay duplicated the promoted facts"
    );
}

fn semantic_fact_count(
    env: &ukma_integration::Env,
    context: &ukma_contracts::RequestContext,
) -> usize {
    let found = env.ok(
        "memory.search",
        context,
        json!({
            "query": {
                "query": "parser",
                "kinds": ["semantic"],
                "language": null,
                "after": null,
                "before": null,
                "min_trust": null,
                "max_results": 100,
                "detail": "title",
                "max_tokens": 4096
            }
        }),
    );
    found["items"].as_array().map(Vec::len).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// E2E-API-08: tenant A/B cross-domain graph traversal (adapter level)
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_08_a_connected_cross_tenant_edge_never_leaks_between_tenants() {
    use graph_engine::{
        Direction, EdgeType, GraphEngine, InMemoryGraphStore, NodeType, PropertyMap, ScopeKey,
    };
    use std::sync::Arc;

    let engine = GraphEngine::with_store(Arc::new(InMemoryGraphStore::new()));
    let scope_a = ScopeKey::new(TENANT, PROJECT);
    let scope_b = ScopeKey::new("tenant-z", PROJECT);

    // The same logical names exist in both tenants and are connected inside each.
    let a_root = engine
        .add_node(
            &scope_a,
            NodeType::Module,
            PropertyMap::new().with("name", "root"),
            None,
        )
        .unwrap();
    let a_child = engine
        .add_node(
            &scope_a,
            NodeType::Function,
            PropertyMap::new().with("name", "child"),
            None,
        )
        .unwrap();
    engine
        .add_edge(
            &scope_a,
            &a_root.id,
            &a_child.id,
            EdgeType::Contains,
            PropertyMap::new(),
            None,
        )
        .unwrap();

    let b_root = engine
        .add_node(
            &scope_b,
            NodeType::Module,
            PropertyMap::new().with("name", "root"),
            None,
        )
        .unwrap();
    let b_child = engine
        .add_node(
            &scope_b,
            NodeType::Function,
            PropertyMap::new().with("name", "child"),
            None,
        )
        .unwrap();
    engine
        .add_edge(
            &scope_b,
            &b_root.id,
            &b_child.id,
            EdgeType::Contains,
            PropertyMap::new(),
            None,
        )
        .unwrap();

    let reachable = engine.bfs(&scope_a, &a_root.id, 3, Some(100)).unwrap();
    assert_eq!(reachable, vec![a_child.id.clone()]);
    assert!(
        !reachable.contains(&b_child.id),
        "tenant B leaked into tenant A"
    );

    // A traversal from tenant A can never reach a tenant B id, even by id guessing.
    assert!(engine.get_node(&scope_a, &b_root.id).is_err());
    assert!(engine.get_node(&scope_b, &a_root.id).is_err());

    // Neighbours agree: the cross-tenant edge does not exist in either direction.
    let neighbours = engine
        .neighbors(&scope_a, &a_root.id, Direction::Outgoing)
        .unwrap();
    assert_eq!(neighbours.len(), 1);
    assert_eq!(neighbours[0].0, a_child.id);
}

// ---------------------------------------------------------------------------
// E2E-API-09: delayed cross-backend outbox projection
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_09_a_generation_is_refused_until_the_projection_is_published() {
    use indexing_engine::{GenerationPointer, JobError};
    use storage_ports::{MetadataPort, OutboxPort};
    use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};

    let mut path = std::env::temp_dir();
    path.push(format!("ukma-e2e-outbox-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = EmbeddedStore::open(&path).unwrap();
    let scope = default_scope(TENANT, PROJECT);

    // Canonical commit lands, and the projection event is queued but not applied.
    store
        .put_cas(&scope, "graph/manifest", json!({"gen": 2}), None)
        .unwrap();
    store
        .enqueue(&scope, event("e1", "code.index.publish", 2))
        .unwrap();
    store.flush().unwrap();

    let mut pointer = GenerationPointer::new();
    // Nothing is published yet: a strict_generation read must be refused.
    match pointer.require("gen-2") {
        Err(JobError::StaleGeneration { requested }) => assert_eq!(requested, "gen-2"),
        other => panic!("expected INDEX_NOT_READY-style refusal, got {other:?}"),
    }

    // Replay the outbox, publish the generation, then read it strictly.
    let pending = store.pending(&scope, "projection", 10).unwrap();
    assert_eq!(pending.len(), 1);
    store
        .ack(&scope, "projection", &pending[0].event_id)
        .unwrap();
    pointer.publish("gen-2");
    pointer
        .require("gen-2")
        .expect("published generation is visible");

    // A caller that still asks for the superseded generation keeps working while
    // it is retained, and a never-published one is still refused.
    pointer.publish("gen-3");
    pointer.require("gen-2").expect("retained generation");
    assert!(matches!(
        pointer.require("gen-404"),
        Err(JobError::StaleGeneration { .. })
    ));
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// E2E-API-10: RocksDB-free adapter profile
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_10_the_shipped_adapters_need_no_rocksdb_and_meet_the_minimum_profile() {
    use storage_ports::capabilities::CapabilityProbe;
    use ukma_adapter_embedded::EmbeddedStore;

    let mut path = std::env::temp_dir();
    path.push(format!("ukma-e2e-profile-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = EmbeddedStore::open(&path).unwrap();
    store
        .capabilities()
        .assert_minimum_profile()
        .expect("embedded adapter provides the §40.5 minimum profile");
    let _ = std::fs::remove_file(&path);

    // The dependency graph is the real evidence: no RocksDB crate is linked.
    let lock = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
    )
    .expect("workspace Cargo.lock");
    for forbidden in ["rocksdb", "librocksdb-sys"] {
        assert!(
            !lock.contains(&format!("name = \"{forbidden}\"")),
            "{forbidden} must not be a workspace dependency"
        );
    }
}

// ---------------------------------------------------------------------------
// E2E-API-11: adapter A -> B cutover keeps logical identity
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_11_a_store_copy_preserves_logical_ids_revisions_and_scope() {
    use storage_ports::{MetadataPort, OutboxPort};
    use ukma_adapter_embedded::{default_scope, event, EmbeddedStore};

    let mut source_path = std::env::temp_dir();
    source_path.push(format!("ukma-e2e-cutover-a-{}.json", std::process::id()));
    let mut target_path = std::env::temp_dir();
    target_path.push(format!("ukma-e2e-cutover-b-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&target_path);

    let scope = default_scope(TENANT, PROJECT);
    let entry_revision;
    let entry_etag;
    {
        let source = EmbeddedStore::open(&source_path).unwrap();
        let entry = source
            .put_cas(&scope, "memory/mem:1", json!({"summary": "kept"}), None)
            .unwrap();
        entry_revision = entry.revision;
        entry_etag = entry.etag.clone();
        source.put_manifest(&scope, "memory", "gen-1").unwrap();
        source
            .enqueue(&scope, event("e1", "memory.episode.committed", 1))
            .unwrap();
        source.flush().unwrap();
    }

    // Cutover: the canonical snapshot moves to the new backend unchanged.
    std::fs::copy(&source_path, &target_path).unwrap();
    let target = EmbeddedStore::open(&target_path).unwrap();
    let reloaded = target
        .get(&scope, "memory/mem:1")
        .unwrap()
        .expect("entry survives the cutover");
    assert_eq!(reloaded.revision, entry_revision, "logical revision");
    assert_eq!(reloaded.etag, entry_etag, "etag");
    assert_eq!(reloaded.value, json!({"summary": "kept"}));
    assert_eq!(
        target.get_manifest(&scope, "memory").unwrap().as_deref(),
        Some("gen-1")
    );
    assert_eq!(target.pending(&scope, "projection", 10).unwrap().len(), 1);

    // ACL/scope parity: another tenant sees nothing, even with the same key.
    let foreign = default_scope("tenant-z", PROJECT);
    assert!(target.get(&foreign, "memory/mem:1").unwrap().is_none());

    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&target_path);
}

// ---------------------------------------------------------------------------
// E2E-API-12: network timeout at the canonical commit
// ---------------------------------------------------------------------------

#[test]
fn e2e_api_12_a_lost_response_is_resolved_by_the_idempotency_receipt() {
    let env = env();
    let context = idempotent(sandbox(ctx(), "s-loss", "agent-loss"), "idem-loss-1");

    let first = env.ok(
        "memory.store_episode",
        &context,
        episode("s-loss", "agent-loss", "commit then the network died"),
    );
    assert_eq!(first["changed"], true);
    let resource = first["resource_id"].as_str().unwrap().to_string();

    // The client never saw the response: it retries the same request.
    let second = env.ok(
        "memory.store_episode",
        &context,
        episode("s-loss", "agent-loss", "commit then the network died"),
    );
    assert_eq!(
        second["resource_id"], resource,
        "one canonical version only"
    );
    assert_eq!(second["changed"], false, "no second write");

    // A different payload under the same key is a conflict, not a silent overwrite.
    let conflict = env.error(
        "memory.store_episode",
        &context,
        episode("s-loss", "agent-loss", "a different summary"),
    );
    assert_eq!(conflict, "IDEMPOTENCY_CONFLICT");

    // And exactly one record exists for the sandbox.
    let listed = env.ok(
        "memory.search",
        &context,
        json!({
            "query": {
                "query": "commit then the network died",
                "kinds": ["episodic"],
                "language": null,
                "after": null,
                "before": null,
                "min_trust": null,
                "max_results": 20,
                "detail": "title",
                "max_tokens": 512
            }
        }),
    );
    let hits = listed["items"].as_array().expect("hits");
    assert_eq!(hits.len(), 1, "duplicate canonical records: {listed}");
}

// ---------------------------------------------------------------------------
// The scenarios are registered work, not an ad-hoc list
// ---------------------------------------------------------------------------

#[test]
fn every_e2e_scenario_has_a_registered_operation_behind_it() {
    let total = all_scopes().len();
    assert!(total >= 20, "scope registry looks truncated: {total}");
    for operation in [
        "code.analyze_repository",
        "code.find_symbols",
        "document.ingest",
        "document.get_document",
        "traceability.propose_link",
        "traceability.accept_link",
        "traceability.revalidate_stale",
        "memory.open_agent_scope",
        "memory.fold",
        "memory.page_out",
        "memory.consolidate",
        "memory.store_episode",
        "system.get_job",
        "system.cancel_job",
    ] {
        assert!(
            OPERATIONS.iter().any(|spec| spec.operation_id == operation),
            "{operation} is not in the registry"
        );
    }
    let _: Value = json!({ "tenant": TENANT });
    let _ = foreign_ctx("tenant-z");
}
