//! DB-005 (§17.1) and SR-001 (§29.4): memory retention — the TTL/FSRS sweep,
//! RL eviction and `forget` — must never touch Code or Document authority.
//!
//! Everything is driven through the public API, except the TTL/FSRS sweep,
//! which is a scheduled maintenance job with no wire operation and is
//! therefore driven at the adapter level (the same convention as the
//! storage-port scenarios).

use std::sync::Arc;

use serde_json::json;
use ukma_integration::{ctx, env_with_clock_and_sources, etag_of, sandbox};

use code_intelligence::domain::SourceFile;
use memory_domain::{FixedClock, SweepAction};

const COMMIT: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
const T0: i64 = 1_700_000_000;

fn seed(
    env: &ukma_integration::Env,
    context: &ukma_contracts::RequestContext,
    hashes: &[String],
) -> String {
    let handle = env.ok(
        "code.analyze_repository",
        context,
        json!({
            "revision": {
                "repository_id": "repo-retain",
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
    let code_job = env.ok(
        "code.get_analysis_job",
        context,
        json!({ "job_id": handle["job_id"] }),
    );
    assert_eq!(code_job["stage"], "ready", "{code_job}");

    let handle = env.ok(
        "document.ingest",
        context,
        json!({
            "source": {
                "source_uri": "memory://retention.md",
                "media_type": "text/markdown",
                "content_hash": hashes[0],
                "revision": "rev-1",
                "language_hint": "en",
                "artifact_ref": "artifact:retention.md"
            },
            "parser_profile": "markdown/1",
            "include_images": false,
            "asynchronous": true
        }),
    );
    let job = env.ok(
        "document.get_ingest_job",
        context,
        json!({ "job_id": handle["job_id"] }),
    );
    assert_eq!(job["stage"], "ready", "{job}");
    job["result_ref"].as_str().expect("document id").to_string()
}

fn code_symbols(
    env: &ukma_integration::Env,
    context: &ukma_contracts::RequestContext,
) -> serde_json::Value {
    env.ok(
        "code.find_symbols",
        context,
        json!({
            "repository_id": "repo-retain",
            "revision": COMMIT,
            "query": "order",
            "kinds": [],
            "path_glob": null,
            "language": null,
            "page": { "cursor": null, "limit": 20 }
        }),
    )
}

fn document_view(
    env: &ukma_integration::Env,
    context: &ukma_contracts::RequestContext,
    document_id: &str,
) -> serde_json::Value {
    env.ok(
        "document.get_document",
        context,
        json!({
            "document_id": document_id,
            "include_blocks": true,
            "page": { "cursor": null, "limit": 50 }
        }),
    )
}

fn store_trilingual(
    env: &ukma_integration::Env,
    memory_ctx: &ukma_contracts::RequestContext,
) -> (String, String) {
    let mut zh: Option<(String, String)> = None;
    for (language, summary) in [
        ("zh", "订单已按序发货"),
        ("en", "the order shipped in order"),
        ("ja", "注文は順次発送された"),
    ] {
        let mut body = ukma_integration::episode("s-retain", "agent-retain", summary);
        body["input"]["language"] = json!(language);
        let receipt = env.ok("memory.store_episode", memory_ctx, body);
        if language == "zh" {
            let id = receipt["resource_id"].as_str().expect("id").to_string();
            let etag = etag_of(&receipt);
            zh = Some((id, etag));
        }
    }
    zh.expect("the zh episode is stored")
}

/// DB-005: the TTL/FSRS sweep archives and then purges memory records, and
/// the Code and Document authority is byte-for-byte identical before and
/// after every sweep state.
#[test]
fn db005_memory_ttl_and_fsrs_never_purge_code_or_document_authority() {
    let clock = Arc::new(FixedClock::new(T0));
    let (env, hashes) = env_with_clock_and_sources(
        clock.clone(),
        COMMIT,
        vec![
            SourceFile::new("src/alpha.rs", "pub fn alpha_order() -> u32 { 1 }\n"),
            SourceFile::new("src/beta.rs", "pub fn beta_order() -> u32 { 2 }\n"),
        ],
        vec![(
            "artifact:retention.md",
            b"# Retention\n\nThis document is authoritative source data.\n".to_vec(),
        )],
    );
    let context = ctx();
    let memory_ctx = sandbox(context.clone(), "s-retain", "agent-retain");
    let document_id = seed(&env, &context, &hashes);
    store_trilingual(&env, &memory_ctx);

    // Snapshots of the two other domains' authority, before any retention ran.
    let symbols_before = code_symbols(&env, &context);
    let document_before = document_view(&env, &context, &document_id);
    assert_eq!(document_before["source"]["content_hash"], hashes[0]);

    // Fresh: nothing is purged while the active window holds.
    let fresh = env.services.memory.sweep(&[], true).unwrap();
    assert!(!fresh.is_empty(), "the sweep must see the stored records");
    assert!(
        fresh
            .iter()
            .all(|(_, decision)| decision.action != SweepAction::Purge),
        "a fresh sweep must not purge: {fresh:?}"
    );

    // Past the 72h active window: archived, the purge horizon has not passed.
    clock.advance(73 * 3_600);
    let archived = env.services.memory.sweep(&[], true).unwrap();
    assert!(
        archived
            .iter()
            .any(|(_, decision)| decision.action == SweepAction::Archive),
        "past the active window the records archive: {archived:?}"
    );
    assert!(
        archived
            .iter()
            .all(|(_, decision)| decision.action != SweepAction::Purge),
        "the 30-day purge horizon has not been reached: {archived:?}"
    );

    // Past the 30-day horizon with every guard satisfied: purged.
    clock.advance(31 * 86_400);
    let purged = env.services.memory.sweep(&[], true).unwrap();
    assert!(
        purged
            .iter()
            .any(|(_, decision)| decision.action == SweepAction::Purge),
        "past the horizon, with regulation allowing, the records purge: {purged:?}"
    );

    // The memory records are really gone...
    let gone = env.ok(
        "memory.search",
        &memory_ctx,
        json!({
            "query": {
                "query": "the order shipped in order",
                "kinds": [],
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
    assert!(
        gone["items"].as_array().expect("items").is_empty(),
        "the purged memory must be gone: {gone}"
    );

    // ...while Code and Document authority are byte-for-byte identical.
    assert_eq!(
        code_symbols(&env, &context),
        symbols_before,
        "memory retention touched code authority"
    );
    let document_after = document_view(&env, &context, &document_id);
    assert_eq!(
        document_after, document_before,
        "memory retention touched document authority"
    );
    assert_eq!(document_after["source"]["content_hash"], hashes[0]);
}

/// SR-001: `forget` and RL eviction remove memory records, and the Code and
/// Document source artifacts are untouched by both mechanisms.
#[test]
fn sr001_forget_and_rl_eviction_leave_code_and_document_sources_untouched() {
    let clock = Arc::new(FixedClock::new(T0));
    let (env, hashes) = env_with_clock_and_sources(
        clock,
        COMMIT,
        vec![SourceFile::new(
            "src/alpha.rs",
            "pub fn alpha_order() -> u32 { 1 }\n",
        )],
        vec![(
            "artifact:retention.md",
            b"# Retention\n\nSource artifacts outlive memory retention.\n".to_vec(),
        )],
    );
    let context = ctx();
    let memory_ctx = sandbox(context.clone(), "s-retain", "agent-retain");
    let document_id = seed(&env, &context, &hashes);

    let stored = store_trilingual(&env, &memory_ctx);
    let (memory_id, memory_etag) = stored;
    let block = env.ok(
        "memory.put_working_block",
        &memory_ctx,
        ukma_integration::working_block("s-retain", "agent-retain", "scratch material", 0.3),
    );
    assert_eq!(block["kind"], "working", "{block}");

    // Snapshots of the other domains' authority.
    let symbols_before = code_symbols(&env, &context);
    let document_before = document_view(&env, &context, &document_id);

    // RL eviction through the public API frees the working block.
    let plan = env.ok(
        "memory.evaluate_eviction",
        &memory_ctx,
        json!({
            "agent_id": "agent-retain",
            "session_id": "s-retain",
            "policy_id": "deterministic-v1",
            "dry_run": false,
            "required_tokens": 1
        }),
    );
    assert!(
        !plan["selected"].as_array().expect("selected").is_empty(),
        "the eviction must actually free a block: {plan}"
    );

    // Forget through the public API deletes the episodic record.
    env.ok(
        "memory.forget",
        &memory_ctx,
        json!({
            "memory": { "id": memory_id, "kind": "episodic", "revision": memory_etag },
            "reason": "retention cleanup",
            "expected_etag": memory_etag
        }),
    );

    // The forgotten episode and the evicted block are both gone...
    let forgotten = env.ok(
        "memory.search",
        &memory_ctx,
        json!({
            "query": {
                "query": "订单已按序发货",
                "kinds": [],
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
    assert!(
        forgotten["items"].as_array().expect("items").is_empty(),
        "the forgotten episode must be gone: {forgotten}"
    );
    let listed = env.ok(
        "memory.list_working_blocks",
        &memory_ctx,
        json!({
            "session_id": "s-retain",
            "agent_id": "agent-retain",
            "page": { "cursor": null, "limit": 50 }
        }),
    );
    assert!(
        listed["items"].as_array().expect("items").is_empty(),
        "the evicted working block must be gone: {listed}"
    );

    // ...while the Code and Document source artifacts are untouched.
    assert_eq!(
        code_symbols(&env, &context),
        symbols_before,
        "memory RL/forget touched code authority"
    );
    let document_after = document_view(&env, &context, &document_id);
    assert_eq!(
        document_after, document_before,
        "memory RL/forget touched document authority"
    );
}
