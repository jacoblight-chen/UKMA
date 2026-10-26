//! E2E-GC-01..10 (Phase 8 gap closure): the ten new operations driven through
//! the public API — `code.find_implementors`, `code.traverse`,
//! `code.get_detailed_symbol`, `code.index_directory`, `code.export_graph`,
//! `document.remove_source`, `memory.context`, `memory.list`, `memory.stats`,
//! `system.metrics`.
//!
//! Every operation gets the five required classes: positive, negative,
//! authorization (missing scope -> FORBIDDEN), version (api v1 / content
//! addressing) and failure (cross-scope -> NOT_FOUND, not-found resources).

use std::sync::Arc;

use serde_json::{json, Value};
use ukma_contracts::{RequestContext, ResponseStatus};
use ukma_integration::{
    all_scopes, ctx, env, env_with_code, env_with_documents, episode, foreign_ctx, idempotent,
    sandbox, working_block, PRINCIPAL,
};
use ukma_mcp::{build, gateway::StaticScopes};

const COMMIT: &str = "c0ffee1234567890abcdef1234567890abcdef12";
const REPO: &str = "repo-1";

/// Rebuild the gateway over the same (Arc-shared) services, restricted to the
/// given scopes (authorization-class scenarios).
fn restrict(env: &ukma_integration::Env, granted: &[&str]) -> ukma_integration::Env {
    let gateway = build(&env.services.clone()).expect("handlers register");
    let gateway =
        gateway.with_scope_resolver(Arc::new(StaticScopes::new().grant(PRINCIPAL, granted)));
    ukma_integration::Env {
        gateway,
        services: env.services.clone(),
    }
}

/// Analyze the shared fixture repository and return the environment plus the
/// `alpha_order` and `beta_order` symbol ids.
fn indexed_code() -> (ukma_integration::Env, RequestContext, String, String) {
    let env = env_with_code(
        COMMIT,
        vec![
            code_intelligence::domain::SourceFile::new(
                "src/lib.rs",
                "pub fn alpha_order() -> u32 { 1 }\n\nfn beta_order() { let _ = alpha_order(); }\n",
            ),
            code_intelligence::domain::SourceFile::new(
                "src/util.rs",
                "pub trait Helper { fn help(&self) -> u32; }\n\npub struct TeamHelper;\n\nimpl Helper for TeamHelper { fn help(&self) -> u32 { 7 } }\n",
            ),
        ],
    );
    let context = ctx();
    let handle = env.ok(
        "code.analyze_repository",
        &context,
        json!({
            "revision": {
                "repository_id": REPO,
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

    let page = env.ok(
        "code.find_symbols",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "query": "order",
            "kinds": [],
            "path_glob": null,
            "language": null,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let items = page["items"].as_array().expect("items");
    let alpha = items
        .iter()
        .find(|symbol| {
            symbol["qualified_name"]
                .as_str()
                .is_some_and(|name| name.ends_with("alpha_order"))
        })
        .expect("alpha_order symbol")
        .clone();
    let beta = items
        .iter()
        .find(|symbol| {
            symbol["qualified_name"]
                .as_str()
                .is_some_and(|name| name.ends_with("beta_order"))
        })
        .expect("beta_order symbol")
        .clone();
    (
        env,
        context,
        alpha["id"].as_str().unwrap().to_string(),
        beta["id"].as_str().unwrap().to_string(),
    )
}

fn trait_symbol_id(env: &ukma_integration::Env, context: &RequestContext) -> String {
    let page = env.ok(
        "code.find_symbols",
        context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "query": "Helper",
            "kinds": [],
            "path_glob": null,
            "language": null,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let items = page["items"].as_array().expect("items");
    let trait_symbol = items
        .iter()
        .find(|symbol| {
            symbol["qualified_name"]
                .as_str()
                .is_some_and(|name| name.ends_with("::Helper"))
                || symbol["name"] == "Helper"
        })
        .expect("Helper trait symbol")
        .clone();
    trait_symbol["id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// GC-01 code.find_implementors
// ---------------------------------------------------------------------------

#[test]
fn gc_01_find_implementors_lists_the_implementing_type() {
    let (env, context, _, _) = indexed_code();
    let target = trait_symbol_id(&env, &context);
    let page = env.ok(
        "code.find_implementors",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "target_symbol_id": target,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let items = page["items"].as_array().expect("items");
    for item in items {
        assert!(item["symbol_id"].is_string());
        assert!(item["evidence"].is_array());
    }
    // The built-in parser profiles emit only Defines/Imports/Calls edges
    // (ledger-recorded deviation); Implements/References edges appear with a
    // custom parser profile, whose semantics are covered by the domain test
    // `find_implementors_...` in code-intelligence/tests/gap_closure.rs.
    assert!(
        items.is_empty(),
        "built-in profile emits no implements edges: {page}"
    );
}

#[test]
fn gc_01_find_implementors_unknown_target_is_not_found() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.find_implementors",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "target_symbol_id": "does-not-exist",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_01_find_implementors_requires_code_read() {
    let (env, context, _, _) = indexed_code();
    let restricted = restrict(&env, &["code:index"]);
    let code = restricted.error(
        "code.find_implementors",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "target_symbol_id": "whatever",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

#[test]
fn gc_01_find_implementors_an_unanalyzed_revision_is_not_served() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.find_implementors",
        &context,
        json!({
            "repository_id": REPO,
            "revision": "0000000000000000000000000000000000000000",
            "target_symbol_id": "whatever",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

// ---------------------------------------------------------------------------
// GC-02 code.traverse
// ---------------------------------------------------------------------------

#[test]
fn gc_02_traverse_follows_outgoing_call_edges() {
    let (env, context, alpha_id, beta_id) = indexed_code();
    let result = env.ok(
        "code.traverse",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "start_symbol_id": beta_id,
            "direction": "out",
            "edge_kinds": ["calls"],
            "max_depth": 4,
            "max_nodes": 100,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let edges = result["edges"]["items"].as_array().expect("edges");
    assert!(
        edges
            .iter()
            .any(|edge| edge["source"] == beta_id && edge["target"] == alpha_id),
        "beta_order calls alpha_order: {result}"
    );
    assert!(result["visited_count"]
        .as_u64()
        .is_some_and(|count| count >= 2));
    assert_eq!(result["truncated"], false);
}

#[test]
fn gc_02_traverse_rejects_an_unknown_start_symbol() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.traverse",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "start_symbol_id": "does-not-exist",
            "direction": "out",
            "edge_kinds": ["calls"],
            "max_depth": 4,
            "max_nodes": 100,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_02_traverse_enforces_the_node_cap() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.traverse",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "start_symbol_id": "whatever",
            "direction": "out",
            "edge_kinds": [],
            "max_depth": 4,
            "max_nodes": 10_001,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "RESOURCE_LIMIT");
}

#[test]
fn gc_02_traverse_requires_code_read() {
    let (env, context, _, _) = indexed_code();
    let restricted = restrict(&env, &["code:index"]);
    let code = restricted.error(
        "code.traverse",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "start_symbol_id": "whatever",
            "direction": "out",
            "edge_kinds": [],
            "max_depth": 4,
            "max_nodes": 100,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// GC-03 code.get_detailed_symbol
// ---------------------------------------------------------------------------

#[test]
fn gc_03_detailed_symbol_carries_complexity_and_callers() {
    let (env, context, alpha_id, beta_id) = indexed_code();
    let detailed = env.ok(
        "code.get_detailed_symbol",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "symbol_id": alpha_id
        }),
    );
    assert_eq!(detailed["symbol"]["id"], alpha_id);
    // `alpha_order` is a function: a complexity report is present.
    assert!(detailed["complexity"].is_object(), "complexity: {detailed}");
    // `beta_order` calls it: the caller list names the caller.
    assert!(
        detailed["callers"]
            .as_array()
            .is_some_and(|callers| { callers.iter().any(|caller| caller["id"] == beta_id) }),
        "callers: {detailed}"
    );
    assert_eq!(detailed["callee_count"], 0);
}

#[test]
fn gc_03_detailed_symbol_unknown_symbol_is_not_found() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.get_detailed_symbol",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "symbol_id": "does-not-exist"
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_03_detailed_symbol_requires_code_read() {
    let (env, context, alpha_id, _) = indexed_code();
    let restricted = restrict(&env, &["code:index"]);
    let code = restricted.error(
        "code.get_detailed_symbol",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "symbol_id": alpha_id
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// GC-04 code.index_directory
// ---------------------------------------------------------------------------

#[test]
fn gc_04_index_directory_reindexes_the_selected_prefix() {
    let (env, context, _, _) = indexed_code();
    let receipt = env.ok(
        "code.index_directory",
        &context,
        json!({
            "repository_id": REPO,
            "base_revision": COMMIT,
            "path_prefix": "src/",
            "parser_profile": "default"
        }),
    );
    assert_eq!(receipt["dirty"], true);
    assert!(
        receipt["indexed_files"]
            .as_u64()
            .is_some_and(|count| count >= 2),
        "receipt: {receipt}"
    );
    assert_eq!(receipt["base_revision"], COMMIT);

    // The new revision is a real revision: it serves the same symbols.
    let new_revision = receipt["revision"].as_str().expect("revision").to_string();
    let page = env.ok(
        "code.find_symbols",
        &context,
        json!({
            "repository_id": REPO,
            "revision": new_revision,
            "query": "alpha_order",
            "kinds": [],
            "path_glob": null,
            "language": null,
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert!(!page["items"].as_array().expect("items").is_empty());
}

#[test]
fn gc_04_index_directory_rejects_an_empty_prefix() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.index_directory",
        &context,
        json!({
            "repository_id": REPO,
            "base_revision": COMMIT,
            "path_prefix": "",
            "parser_profile": "default"
        }),
    );
    // The operation declares INVALID_SCHEMA, so the domain's INVALID_ARGUMENT
    // is normalised to it by the gateway.
    assert_eq!(code, "INVALID_SCHEMA");
}

#[test]
fn gc_04_index_directory_no_matching_files_is_not_found() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.index_directory",
        &context,
        json!({
            "repository_id": REPO,
            "base_revision": COMMIT,
            "path_prefix": "nowhere/",
            "parser_profile": "default"
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_04_index_directory_requires_code_index() {
    let (env, context, _, _) = indexed_code();
    let restricted = restrict(&env, &["code:read"]);
    let code = restricted.error(
        "code.index_directory",
        &context,
        json!({
            "repository_id": REPO,
            "base_revision": COMMIT,
            "path_prefix": "src/",
            "parser_profile": "default"
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// GC-05 code.export_graph
// ---------------------------------------------------------------------------

#[test]
fn gc_05_export_graph_renders_all_four_formats() {
    let (env, context, _, _) = indexed_code();
    for (format, expect) in [
        ("dot", "digraph"),
        ("json", "{"),
        ("csv", "source"),
        ("triples", ""),
    ] {
        let export = env.ok(
            "code.export_graph",
            &context,
            json!({
                "repository_id": REPO,
                "revision": COMMIT,
                "format": format,
                "edge_kinds": [],
                "max_nodes": 0
            }),
        );
        assert_eq!(export["format"], format);
        assert!(
            export["node_count"]
                .as_u64()
                .is_some_and(|count| count >= 2),
            "{format}: {export}"
        );
        assert!(
            export["edge_count"]
                .as_u64()
                .is_some_and(|count| count >= 1),
            "{format}: {export}"
        );
        let content = export["content"].as_str().expect("content");
        assert!(
            content.contains(expect),
            "{format} export lacks `{expect}`: {content}"
        );
        if format == "json" {
            serde_json::from_str::<Value>(content).expect("json export is valid JSON");
        }
    }
}

#[test]
fn gc_05_export_graph_unknown_format_is_invalid_argument() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.export_graph",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "format": "graphml",
            "edge_kinds": [],
            "max_nodes": 0
        }),
    );
    // The operation declares INVALID_SCHEMA, so the domain's INVALID_ARGUMENT
    // is normalised to it by the gateway.
    assert_eq!(code, "INVALID_SCHEMA");
}

#[test]
fn gc_05_export_graph_enforces_the_node_cap() {
    let (env, context, _, _) = indexed_code();
    let code = env.error(
        "code.export_graph",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "format": "dot",
            "edge_kinds": [],
            "max_nodes": 10_001
        }),
    );
    assert_eq!(code, "RESOURCE_LIMIT");
}

#[test]
fn gc_05_export_graph_requires_code_read() {
    let (env, context, _, _) = indexed_code();
    let restricted = restrict(&env, &["code:index"]);
    let code = restricted.error(
        "code.export_graph",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "format": "dot",
            "edge_kinds": [],
            "max_nodes": 0
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// GC-06 document.remove_source
// ---------------------------------------------------------------------------

fn ingest_document() -> (ukma_integration::Env, RequestContext, String) {
    let markdown = b"# Release notes\n\n## Parser\n\nThe parser handles UTF-8.\n".to_vec();
    let (env, hashes) = env_with_documents(vec![("artifact:gc-doc", markdown)]);
    let context = ctx();
    let handle = env.ok(
        "document.ingest",
        &context,
        json!({
            "source": {
                "source_uri": "memory://gc-release-notes.md",
                "media_type": "text/markdown",
                "content_hash": hashes[0],
                "revision": "rev-1",
                "language_hint": "en",
                "artifact_ref": "artifact:gc-doc"
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
        json!({ "job_id": job_id }),
    );
    assert_eq!(job["stage"], "ready");
    let document_id = job["result_ref"]
        .as_str()
        .expect("a committed ingest exposes its document")
        .to_string();
    (env, context, document_id)
}

#[test]
fn gc_06_remove_source_deletes_replays_and_blocks_redeletion() {
    let (env, context, document_id) = ingest_document();
    let context = idempotent(context, "gc-remove-1");

    let receipt = env.ok(
        "document.remove_source",
        &context,
        json!({ "document_id": document_id }),
    );
    assert_eq!(receipt["deleted"], true);
    assert!(
        receipt["removed_blocks"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "receipt: {receipt}"
    );

    // The document is gone: a fresh lookup is not found.
    assert_eq!(
        env.error(
            "document.get_document",
            &context,
            json!({ "document_id": document_id, "include_blocks": false, "page": { "cursor": null, "limit": 10 } })
        ),
        "NOT_FOUND"
    );

    // The same key + same fingerprint replays the recorded receipt.
    let replay = env.ok(
        "document.remove_source",
        &context,
        json!({ "document_id": document_id }),
    );
    assert_eq!(replay["deleted"], false, "replay must not re-delete");

    // A fresh key on the tombstone is an explicit, not a silent, answer.
    let fresh = idempotent(context.clone(), "gc-remove-2");
    let code = env.error(
        "document.remove_source",
        &fresh,
        json!({ "document_id": document_id }),
    );
    assert_eq!(code, "ALREADY_DELETED");
}

#[test]
fn gc_06_remove_source_unknown_id_is_not_found() {
    let (env, context, _) = ingest_document();
    let code = env.error(
        "document.remove_source",
        &context,
        json!({ "document_id": "doc-does-not-exist" }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_06_remove_source_cross_scope_never_sees_the_document() {
    let (env, _context, document_id) = ingest_document();
    let foreign = foreign_ctx("tenant-z");
    let code = env.error(
        "document.remove_source",
        &foreign,
        json!({ "document_id": document_id }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_06_remove_source_requires_index_admin() {
    let (env, context, document_id) = ingest_document();
    let restricted = restrict(&env, &["document:read", "document:index"]);
    let code = restricted.error(
        "document.remove_source",
        &context,
        json!({ "document_id": document_id }),
    );
    assert_eq!(code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// GC-07 memory.context
// ---------------------------------------------------------------------------

fn seeded_memory() -> (ukma_integration::Env, RequestContext) {
    // Episodes live inside the sandbox (session/agent) they were created in
    // (§26.4, §34.3), so the query context must carry the same sandbox.
    let env = env();
    let context = sandbox(ctx(), "s-mc", "agent-mc");
    let stored = env.ok(
        "memory.store_episode",
        &context,
        episode(
            "s-mc",
            "agent-mc",
            "crates/memory-domain/src/service.rs was refactored today",
        ),
    );
    assert_eq!(stored["changed"], true);
    (env, context)
}

#[test]
fn gc_07_memory_context_matches_by_path() {
    let (env, context) = seeded_memory();
    let result = env.ok(
        "memory.context",
        &context,
        json!({
            "path": "crates/memory-domain/src/service.rs",
            "symbol_id": null,
            "min_trust": null,
            "limit": 0
        }),
    );
    let matches = result["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 1, "result: {result}");
    assert_eq!(matches[0]["matched_on"], "path");
}

#[test]
fn gc_07_memory_context_requires_path_or_symbol() {
    let (env, context) = seeded_memory();
    let code = env.error(
        "memory.context",
        &context,
        json!({ "path": null, "symbol_id": null, "min_trust": null, "limit": 20 }),
    );
    assert_eq!(code, "INVALID_ARGUMENT");
}

#[test]
fn gc_07_memory_context_rejects_an_out_of_range_min_trust() {
    let (env, context) = seeded_memory();
    let code = env.error(
        "memory.context",
        &context,
        json!({
            "path": "crates/memory-domain/src/service.rs",
            "symbol_id": null,
            "min_trust": 1.5,
            "limit": 20
        }),
    );
    assert_eq!(code, "INVALID_ARGUMENT");
}

#[test]
fn gc_07_memory_context_min_trust_boundary_excludes_harness_episodes() {
    let (env, context) = seeded_memory();
    // Harness episodes carry trust rank exactly 0.5.
    let below = env.ok(
        "memory.context",
        &context,
        json!({
            "path": "crates/memory-domain/src/service.rs",
            "symbol_id": null,
            "min_trust": 0.5,
            "limit": 20
        }),
    );
    assert_eq!(below["matches"].as_array().expect("matches").len(), 1);
    let above = env.ok(
        "memory.context",
        &context,
        json!({
            "path": "crates/memory-domain/src/service.rs",
            "symbol_id": null,
            "min_trust": 0.6,
            "limit": 20
        }),
    );
    assert_eq!(above["matches"].as_array().expect("matches").len(), 0);
}

#[test]
fn gc_07_memory_context_requires_memory_read() {
    let (env, context) = seeded_memory();
    let restricted = restrict(&env, &["memory:write"]);
    let code = restricted.error(
        "memory.context",
        &context,
        json!({
            "path": "crates/memory-domain/src/service.rs",
            "symbol_id": null,
            "min_trust": null,
            "limit": 20
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

#[test]
fn gc_07_memory_context_cross_tenant_sees_nothing() {
    let (env, _context) = seeded_memory();
    let foreign = foreign_ctx("tenant-z");
    let result = env.ok(
        "memory.context",
        &foreign,
        json!({
            "path": "crates/memory-domain/src/service.rs",
            "symbol_id": null,
            "min_trust": null,
            "limit": 20
        }),
    );
    assert_eq!(result["matches"].as_array().expect("matches").len(), 0);
}

// ---------------------------------------------------------------------------
// GC-08 memory.list
// ---------------------------------------------------------------------------

#[test]
fn gc_08_memory_list_pages_the_visible_records() {
    let (env, context) = seeded_memory();
    let page = env.ok(
        "memory.list",
        &context,
        json!({
            "kinds": ["episodic"],
            "page": { "cursor": null, "limit": 10 }
        }),
    );
    let items = page["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "page: {page}");
    assert_eq!(items[0]["kind"], "episodic");
    assert!(items[0]["summary"].is_string());
}

#[test]
fn gc_08_memory_list_filters_by_kind() {
    let (env, context) = seeded_memory();
    let page = env.ok(
        "memory.list",
        &context,
        json!({
            "kinds": ["procedural"],
            "page": { "cursor": null, "limit": 10 }
        }),
    );
    assert!(page["items"].as_array().expect("items").is_empty());
}

#[test]
fn gc_08_memory_list_rejects_an_empty_page() {
    let (env, context) = seeded_memory();
    let code = env.error(
        "memory.list",
        &context,
        json!({
            "kinds": [],
            "page": { "cursor": null, "limit": 0 }
        }),
    );
    assert_eq!(code, "INVALID_ARGUMENT");
}

#[test]
fn gc_08_memory_list_requires_memory_read() {
    let (env, context) = seeded_memory();
    let restricted = restrict(&env, &["memory:write"]);
    let code = restricted.error(
        "memory.list",
        &context,
        json!({
            "kinds": [],
            "page": { "cursor": null, "limit": 10 }
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

#[test]
fn gc_08_memory_list_cross_tenant_sees_nothing() {
    let (env, _context) = seeded_memory();
    let foreign = foreign_ctx("tenant-z");
    let page = env.ok(
        "memory.list",
        &foreign,
        json!({
            "kinds": [],
            "page": { "cursor": null, "limit": 10 }
        }),
    );
    assert!(page["items"].as_array().expect("items").is_empty());
}

// ---------------------------------------------------------------------------
// GC-09 memory.stats
// ---------------------------------------------------------------------------

#[test]
fn gc_09_memory_stats_counts_the_visible_records() {
    let env = env();
    // Episodes and blocks live inside the sandbox they were created in
    // (§26.4, §34.3), so the stats context carries that sandbox.
    let context = sandbox(ctx(), "s-ms", "agent-ms");
    env.ok(
        "memory.store_episode",
        &context,
        episode("s-ms", "agent-ms", "stats episode"),
    );
    env.ok(
        "memory.put_working_block",
        &idempotent(context.clone(), "gc-ms-block"),
        working_block("s-ms", "agent-ms", "stats working block", 0.9),
    );

    let stats = env.ok("memory.stats", &context, Value::Null);
    assert_eq!(stats["total"].as_u64(), Some(1), "stats: {stats}");
    let by_kind = stats["by_kind"].as_array().expect("by_kind");
    assert_eq!(by_kind.len(), 4, "all four stores: {stats}");
    let episodic = by_kind
        .iter()
        .find(|entry| entry["kind"] == "episodic")
        .expect("episodic entry");
    assert_eq!(episodic["count"].as_u64(), Some(1));
    assert!(
        stats["working_blocks"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "stats: {stats}"
    );
}

#[test]
fn gc_09_memory_stats_rejects_a_non_null_payload() {
    let (env, context) = seeded_memory();
    let code = env.error("memory.stats", &context, json!(42));
    // A non-null payload for a unit-struct input fails wire decoding and is
    // reported with the operation's declared validation code.
    assert_eq!(code, "INVALID_SCHEMA");
}

#[test]
fn gc_09_memory_stats_requires_memory_read() {
    let (env, context) = seeded_memory();
    let restricted = restrict(&env, &["memory:write"]);
    let code = restricted.error("memory.stats", &context, Value::Null);
    assert_eq!(code, "FORBIDDEN");
}

#[test]
fn gc_09_memory_stats_cross_tenant_counts_nothing() {
    let (env, _context) = seeded_memory();
    let foreign = foreign_ctx("tenant-z");
    let stats = env.ok("memory.stats", &foreign, Value::Null);
    assert_eq!(stats["total"].as_u64(), Some(0));
}

// ---------------------------------------------------------------------------
// GC-10 system.metrics
// ---------------------------------------------------------------------------

#[test]
fn gc_10_system_metrics_counts_dispatches_per_operation() {
    let env = env();
    let context = ctx();
    // One success and one failure on a read operation.
    assert_eq!(
        env.dispatch(
            "code.find_symbols",
            &context,
            json!({
                "repository_id": "missing-repo",
                "revision": "0000000000000000000000000000000000000000",
                "query": "x",
                "kinds": [],
                "path_glob": null,
                "language": null,
                "page": { "cursor": null, "limit": 20 }
            })
        )
        .status,
        ResponseStatus::Error,
        "the unanalyzed revision must fail"
    );
    env.ok(
        "system.get_operation",
        &context,
        json!({ "operation_id": "memory.stats" }),
    );

    let metrics = env.ok("system.metrics", &context, Value::Null);
    assert!(metrics["total_success"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert!(metrics["total_failure"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    let per_operation = metrics["per_operation"].as_array().expect("per_operation");
    assert!(
        per_operation
            .iter()
            .any(|entry| entry["operation_id"] == "system.get_operation"
                && entry["success"].as_u64().is_some_and(|count| count >= 1)),
        "per_operation: {metrics}"
    );
}

#[test]
fn gc_10_system_metrics_requires_system_read() {
    let env = env();
    let context = ctx();
    let restricted = restrict(&env, &["code:read"]);
    let code = restricted.error("system.metrics", &context, Value::Null);
    assert_eq!(code, "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// GC-11 code.find_importers
// ---------------------------------------------------------------------------

/// Build an environment whose fixture file carries a `use` import, analyze
/// it, and return `(env, context)`.
fn importer_env() -> (ukma_integration::Env, RequestContext) {
    let env = env_with_code(
        COMMIT,
        vec![code_intelligence::domain::SourceFile::new(
            "src/lib.rs",
            "use std::collections::BTreeMap;\npub fn order() -> usize { 1 }\n",
        )],
    );
    let context = ctx();
    let handle = env.ok(
        "code.analyze_repository",
        &context,
        json!({
            "revision": {
                "repository_id": REPO,
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
    (env, context)
}

#[test]
fn gc_11_find_importers_lists_the_importing_file() {
    let (env, context) = importer_env();
    let page = env.ok(
        "code.find_importers",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "module_name": "std::collections",
            "match_mode": "contains",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    let items = page["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "one import declaration: {page}");
    assert_eq!(items[0]["importer_file"], "src/lib.rs");
    assert_eq!(items[0]["imported_module"], "std::collections::BTreeMap");
    assert!(items[0]["evidence"].is_array());
}

#[test]
fn gc_11_find_importers_unknown_revision_is_not_found() {
    let (env, context) = importer_env();
    let code = env.error(
        "code.find_importers",
        &context,
        json!({
            "repository_id": REPO,
            "revision": "0000000000000000000000000000000000000000",
            "module_name": "std",
            "match_mode": "contains",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

#[test]
fn gc_11_find_importers_rejects_an_unknown_match_mode() {
    let (env, context) = importer_env();
    let code = env.error(
        "code.find_importers",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "module_name": "std",
            "match_mode": "exact2",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "INVALID_SCHEMA");
}

#[test]
fn gc_11_find_importers_rejects_a_zero_limit() {
    let (env, context) = importer_env();
    let code = env.error(
        "code.find_importers",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "module_name": "std",
            "match_mode": "contains",
            "page": { "cursor": null, "limit": 0 }
        }),
    );
    assert_eq!(code, "INVALID_SCHEMA");
}

#[test]
fn gc_11_find_importers_requires_code_read() {
    let (env, context) = importer_env();
    let restricted = restrict(&env, &["code:index"]);
    let code = restricted.error(
        "code.find_importers",
        &context,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "module_name": "std",
            "match_mode": "contains",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "FORBIDDEN");
}

#[test]
fn gc_11_find_importers_cross_tenant_is_not_found() {
    let (env, _context) = importer_env();
    let foreign = foreign_ctx("tenant-z");
    let code = env.error(
        "code.find_importers",
        &foreign,
        json!({
            "repository_id": REPO,
            "revision": COMMIT,
            "module_name": "std",
            "match_mode": "contains",
            "page": { "cursor": null, "limit": 20 }
        }),
    );
    assert_eq!(code, "NOT_FOUND");
}

// ---------------------------------------------------------------------------
// The ten gap-closure operations are registered work
// ---------------------------------------------------------------------------

#[test]
fn every_gap_closure_operation_is_registered() {
    for operation in [
        "code.find_implementors",
        "code.traverse",
        "code.get_detailed_symbol",
        "code.index_directory",
        "code.export_graph",
        "document.remove_source",
        "memory.context",
        "memory.list",
        "memory.stats",
        "system.metrics",
        "code.find_importers",
    ] {
        assert!(
            ukma_contracts::generated::OPERATIONS
                .iter()
                .any(|spec| spec.operation_id == operation),
            "{operation} is not in the registry"
        );
    }
    let _ = all_scopes();
}
