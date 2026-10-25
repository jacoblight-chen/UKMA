//! MEM-006 (§17.1) and FO-005 (§29.4): multilingual (zh/en/ja) memory.
//!
//! * Queries and synonyms are correctly distinguished: per-language queries
//!   only match their own language, and cross-lingual synonyms of the same
//!   proposition are not conflated into false contradictions; a genuine
//!   contradiction (same canonical subject + predicate, incompatible object)
//!   is flagged.
//! * A multilingual fold preserves the key cross-lingual propositions and the
//!   original language text verbatim (the extractive digest of this build
//!   keeps every fact and evidence reference, §28.3).

use serde_json::{json, Value};
use ukma_contracts::RequestContext;
use ukma_integration::{ctx, env_with_documents, episode, etag_of, sandbox};

/// Canonical concept id shared by the cross-lingual facts (§25.4: canonical
/// concept id + original utterances, the original is never overwritten).
const CONCEPT: &str = "concept:order-42";

/// `memory.propose_fact` for one language/predicate/object.
fn propose(
    env: &ukma_integration::Env,
    ctx: &RequestContext,
    language: &str,
    predicate: &str,
    object: &str,
) -> Value {
    env.ok(
        "memory.propose_fact",
        ctx,
        json!({
            "input": {
                "subject": { "id": CONCEPT, "kind": "semantic", "revision": "\"1\"" },
                "predicate": predicate,
                "object": object,
                "language": language,
                "evidence": [{
                    "evidence_id": format!("ev-{language}-{predicate}-{object}"),
                    "source_ref": "artifact:sha256:aa",
                    "confidence": 0.9,
                    "source_group": "group-1"
                }],
                "confidence": 0.8
            }
        }),
    )
}

/// `memory.find_conflicts` for the stored fact, returning the item views.
fn find_conflicts(env: &ukma_integration::Env, ctx: &RequestContext, fact: &Value) -> Vec<Value> {
    let page = env.ok(
        "memory.find_conflicts",
        ctx,
        json!({ "fact": fact, "max_results": 20 }),
    );
    page["items"].as_array().expect("items").clone()
}

/// MEM-006: zh/en/ja queries and synonyms are correctly distinguished.
#[test]
fn mem006_zh_en_ja_queries_and_synonyms_are_correctly_distinguished() {
    let (env, _) = env_with_documents(Vec::new());
    let context = ctx();
    let memory_ctx = sandbox(context.clone(), "s-ml", "agent-ml");

    // 1. Per-language queries: a query in one language matches only the
    //    record of that language (zh/en/ja tokenisations do not cross-match).
    let samples = [
        ("zh", "订单已按序发货"),
        ("en", "the order shipped in order"),
        ("ja", "注文は順次発送された"),
    ];
    for (language, text) in samples {
        let mut body = episode("s-ml", "agent-ml", text);
        body["input"]["language"] = json!(language);
        env.ok("memory.store_episode", &memory_ctx, body);
    }
    for (language, text) in samples {
        let found = env.ok(
            "memory.search",
            &memory_ctx,
            json!({
                "query": {
                    "query": text,
                    "kinds": [],
                    "language": language,
                    "after": null,
                    "before": null,
                    "min_trust": null,
                    "max_results": 20,
                    "detail": "title",
                    "max_tokens": 512
                }
            }),
        );
        let items = found["items"].as_array().expect("items");
        assert_eq!(
            items.len(),
            1,
            "a {language}-scoped query matches only the {language} record: {found}"
        );
        assert_eq!(items[0]["language"], language);
    }
    let en = env.ok(
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
    assert_eq!(
        en["items"].as_array().expect("items").len(),
        1,
        "an unscoped English query must not match the zh/ja records: {en}"
    );

    // 2. Cross-lingual synonyms of the same proposition share the canonical
    //    concept id but carry different surface predicates; none is a
    //    contradiction of another (they are distinguished, not conflated).
    let zh = propose(&env, &memory_ctx, "zh", "狀態", "已發貨");
    let en_fact = propose(&env, &memory_ctx, "en", "status", "shipped");
    let ja = propose(&env, &memory_ctx, "ja", "状態", "遅延");
    for (name, fact) in [("zh", &zh), ("en", &en_fact), ("ja", &ja)] {
        let conflicts = find_conflicts(&env, &memory_ctx, fact);
        assert!(
            conflicts.is_empty(),
            "the cross-lingual synonym {name} must not be flagged as a contradiction: {conflicts:?}"
        );
    }

    // 3. A genuine contradiction — same normalised subject and predicate,
    //    incompatible object — is flagged, and only against the matching
    //    surface form.
    let en2 = propose(&env, &memory_ctx, "en", "status", "delayed");
    let conflicts = find_conflicts(&env, &memory_ctx, &en2);
    let ids: Vec<String> = conflicts
        .iter()
        .filter_map(|view| view["reference"]["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        ids.iter()
            .any(|id| id == &en_fact["id"].as_str().expect("id").to_string()),
        "the contradictory English fact must be flagged: {conflicts:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id == &zh["id"].as_str().expect("id").to_string()),
        "the Chinese fact must not be flagged: {conflicts:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id == &ja["id"].as_str().expect("id").to_string()),
        "the Japanese fact must not be flagged: {conflicts:?}"
    );
}

/// FO-005: a multilingual fold preserves the key cross-lingual propositions
/// and the original language text.
#[test]
fn fo005_multilingual_fold_preserves_original_propositions() {
    let (env, _) = env_with_documents(Vec::new());
    let context = ctx();
    let memory_ctx = sandbox(context.clone(), "s-fold", "agent-fold");

    // Three episodes, each carrying its key proposition in its own language.
    let propositions = [
        ("zh", "订单已按序发货", "订单-42 状态=已发货"),
        (
            "en",
            "the order shipped in order",
            "order-42 status=shipped",
        ),
        ("ja", "注文は順次発送された", "注文-42 状態=発送済み"),
    ];
    let mut refs = Vec::new();
    for (language, summary, fact) in propositions {
        let mut body = episode("s-fold", "agent-fold", summary);
        body["input"]["language"] = json!(language);
        body["input"]["key_facts"] = json!([fact]);
        let receipt = env.ok("memory.store_episode", &memory_ctx, body);
        let id = receipt["resource_id"].as_str().expect("id").to_string();
        let etag = etag_of(&receipt);
        refs.push(json!({ "id": id, "kind": "episodic", "revision": etag }));
    }

    // Fold all three through the public API (deterministic, no model score).
    let fold = env.ok(
        "memory.fold",
        &memory_ctx,
        json!({
            "input": {
                "episode_refs": refs,
                "policy_id": "deterministic-v1",
                "max_tokens": 4096,
                "target_agent_id": "agent-fold"
            },
            "model_score": null
        }),
    );

    // Every original-language proposition survives the fold verbatim.
    let key_facts = fold["key_facts"].as_array().expect("key_facts");
    for (_, _, fact) in propositions {
        assert!(
            key_facts.iter().any(|kept| kept.as_str() == Some(fact)),
            "the proposition {fact} must survive the fold: {fold}"
        );
    }
    // The extractive summary carries the propositions too.
    let summary = fold["summary"].as_str().expect("summary");
    for (_, _, fact) in propositions {
        assert!(
            summary.contains(fact),
            "the summary must keep {fact}: {summary}"
        );
    }
    // All three original episodes are cited and the evidence is protected.
    assert_eq!(
        fold["source_refs"].as_array().expect("source_refs").len(),
        3,
        "every source episode must be cited: {fold}"
    );
    let protected = fold["protected_evidence_refs"]
        .as_array()
        .expect("protected_evidence_refs");
    assert!(
        protected
            .iter()
            .any(|reference| reference.as_str() == Some("artifact:sha256:aa")),
        "the evidence reference must be protected: {fold}"
    );
    // One fold over raw episodes: level 1, discounted trust in (0,1).
    assert_eq!(fold["fold_level"], 1, "{fold}");
    let trust = fold["trust_score"].as_f64().expect("trust_score");
    assert!(
        trust > 0.0 && trust < 1.0,
        "the fold-level trust discount must stay in (0,1): {trust}"
    );
}
