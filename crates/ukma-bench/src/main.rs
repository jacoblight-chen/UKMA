//! Reproducible workload harness (P10-WP3, FM-010).
//!
//! It measures what the deployment actually does through the public API: store
//! episodes and search them, on the in-memory profile. What it records is stated
//! explicitly in the report — including what it does **not** measure (RSS/VRAM, a
//! second backend, an embedding model) — so a reader can tell a baseline from a
//! claim.
//!
//! Usage:
//!   ukma-bench [--episodes 2000] [--queries 500] [--out path.json]

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ResponseStatus, ScopeRef};
use ukma_mcp::gateway::StaticScopes;
use ukma_mcp::{build, Gateway, Services};

const TENANT: &str = "tenant-bench";
const PROJECT: &str = "project-bench";

struct Config {
    episodes: usize,
    queries: usize,
    out: Option<String>,
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config {
        episodes: 2_000,
        queries: 500,
        out: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--episodes" => {
                config.episodes = args
                    .next()
                    .ok_or("--episodes needs a value")?
                    .parse()
                    .map_err(|error| format!("--episodes: {error}"))?;
            }
            "--queries" => {
                config.queries = args
                    .next()
                    .ok_or("--queries needs a value")?
                    .parse()
                    .map_err(|error| format!("--queries: {error}"))?;
            }
            "--out" => config.out = Some(args.next().ok_or("--out needs a value")?),
            "--help" | "-h" => {
                println!("ukma-bench [--episodes N] [--queries N] [--out PATH]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if config.episodes == 0 || config.queries == 0 {
        return Err("--episodes and --queries must be greater than zero".to_string());
    }
    Ok(config)
}

fn gateway() -> Gateway {
    let services = Services::in_memory();
    let scopes = ukma_mcp::manifest::tools()
        .iter()
        .flat_map(|tool| tool.required_scopes.iter().copied())
        .chain(["trace:admin", "code:index_admin", "document:index_admin"])
        .fold(Vec::<String>::new(), |mut acc, scope| {
            if !acc.iter().any(|existing| existing == scope) {
                acc.push(scope.to_string());
            }
            acc
        });
    let borrowed: Vec<&str> = scopes.iter().map(String::as_str).collect();
    build(&services)
        .expect("handlers register")
        .with_scope_resolver(Arc::new(StaticScopes::new().grant("bench", &borrowed)))
}

fn context(request_id: &str) -> RequestContext {
    RequestContext {
        request_id: request_id.to_string(),
        trace_id: "trace-bench".into(),
        scope: ScopeRef {
            tenant_id: TENANT.into(),
            project_id: PROJECT.into(),
            repo_id: None,
            workspace_id: None,
            agent_id: Some("agent-bench".into()),
            session_id: Some("session-bench".into()),
        },
        principal_id: "bench".into(),
        scopes: Vec::new(),
        deadline_at: None,
        consistency: Consistency::ReadYourWrites,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: Some(format!("idem-{request_id}")),
        expected_etag: Some("\"bench\"".into()),
        delegation_token: None,
    }
}

fn episode(session: &str, agent: &str, index: usize) -> Value {
    json!({
        "input": {
            "session_id": session,
            "agent_id": agent,
            "agent_depth": 0,
            "episode_type": "conversation_turn",
            "summary": format!("episode {index} discusses the graph engine and the parser baseline"),
            "full_text_ref": null,
            "key_facts": [format!("fact {index}")],
            "outcome": "success",
            "source_refs": ["artifact:sha256:bench"],
            "importance_score": 0.5,
            "language": "en",
            "evidence": [{
                "evidence_id": format!("ev-{index}"),
                "source_ref": "artifact:sha256:bench",
                "confidence": 0.9,
                "source_group": "group-bench"
            }],
        }
    })
}

/// Nearest-rank percentile over the sorted sample.
fn percentile(sorted: &[f64], percent: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((percent / 100.0) * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

fn stats(mut samples: Vec<f64>) -> Value {
    samples.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = samples.iter().sum();
    let mean = if samples.is_empty() {
        0.0
    } else {
        total / samples.len() as f64
    };
    json!({
        "count": samples.len(),
        "p50_ms": percentile(&samples, 50.0),
        "p95_ms": percentile(&samples, 95.0),
        "p99_ms": percentile(&samples, 99.0),
        "max_ms": samples.last().copied().unwrap_or(0.0),
        "mean_ms": mean,
    })
}

fn main() {
    let config = match parse_args() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("ukma-bench: {error}");
            std::process::exit(2);
        }
    };

    let gateway = gateway();
    let mut store_latencies = Vec::with_capacity(config.episodes);
    let mut search_latencies = Vec::with_capacity(config.queries);
    let mut failures = 0usize;
    let mut failure_codes: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut records = 0usize;

    let store_started = Instant::now();
    for index in 0..config.episodes {
        let request = RequestEnvelope {
            api_version: "1.2.3".into(),
            operation: "memory.store_episode".into(),
            context: context(&format!("store-{index}")),
            input: episode("session-bench", "agent-bench", index),
        };
        let started = Instant::now();
        let response = gateway.dispatch(request);
        store_latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
        match response.status {
            ResponseStatus::Ok => records += 1,
            _ => {
                failures += 1;
                let code = response
                    .error
                    .as_ref()
                    .map(|error| error.code.clone())
                    .unwrap_or_else(|| "UNKNOWN".to_string());
                *failure_codes.entry(code).or_default() += 1;
            }
        }
    }
    let store_elapsed = store_started.elapsed().as_secs_f64();

    let search_started = Instant::now();
    for index in 0..config.queries {
        let request = RequestEnvelope {
            api_version: "1.2.3".into(),
            operation: "memory.search".into(),
            context: context(&format!("search-{index}")),
            input: json!({
                "query": {
                    "query": "graph engine",
                    "kinds": ["episodic"],
                    "language": null,
                    "after": null,
                    "before": null,
                    "min_trust": null,
                    "max_results": 20,
                    "detail": "title",
                    "max_tokens": 2048
                }
            }),
        };
        let started = Instant::now();
        let response = gateway.dispatch(request);
        search_latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
        if response.status == ResponseStatus::Error {
            failures += 1;
            let code = response
                .error
                .as_ref()
                .map(|error| error.code.clone())
                .unwrap_or_else(|| "UNKNOWN".to_string());
            *failure_codes.entry(code).or_default() += 1;
        }
    }
    let search_elapsed = search_started.elapsed().as_secs_f64();

    let total_operations = config.episodes + config.queries;
    let report = json!({
        "spec_version": "1.2.3",
        "audit": "P10-WP3 reproducible workload baseline (FM-010)",
        "command": format!(
            "cargo run --release -p ukma-bench -- --episodes {} --queries {}",
            config.episodes, config.queries
        ),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "hardware": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpu_count": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "note": "single process, single thread workload; RAM/VRAM are not measured by this harness"
        },
        "workload": {
            "episodes": config.episodes,
            "queries": config.queries,
            "language": "en",
            "backend": "embedded profile A (in memory), no embedding model",
            "embedding_version": null,
            "analyzer": "none (BM25 lexical search)"
        },
        "results": {
            "store": stats(store_latencies),
            "search": stats(search_latencies),
            "store_throughput_ops_per_sec": if store_elapsed > 0.0 { config.episodes as f64 / store_elapsed } else { 0.0 },
            "search_throughput_ops_per_sec": if search_elapsed > 0.0 { config.queries as f64 / search_elapsed } else { 0.0 },
            "failure_rate": failures as f64 / total_operations as f64,
            "failure_codes": failure_codes,
            "canonical_records": records,
        },
        "index": {
            "records": records,
            "bytes": null,
            "note": "the in-memory profile does not expose an on-disk index size"
        },
        "not_measured": [
            "process RSS / VRAM",
            "a second backend (profiles B and C are declared unavailable)",
            "1M episode scale (this harness is parameterised, see docs/performance/baseline.md)"
        ]
    });

    let text = serde_json::to_string_pretty(&report).expect("report serialises");
    if let Some(path) = &config.out {
        if let Err(error) = std::fs::write(path, format!("{text}\n")) {
            eprintln!("ukma-bench: cannot write {path}: {error}");
            std::process::exit(1);
        }
    }
    println!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let sorted = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&sorted, 50.0), 3.0);
        assert_eq!(percentile(&sorted, 95.0), 5.0);
        assert_eq!(percentile(&sorted, 99.0), 5.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn stats_report_every_percentile_it_promises() {
        let value = stats(vec![5.0, 1.0, 3.0]);
        assert_eq!(value["count"], 3);
        assert_eq!(value["p50_ms"], 3.0);
        assert_eq!(value["max_ms"], 5.0);
        assert!(value["mean_ms"].as_f64().unwrap() > 2.9);
    }

    #[test]
    fn a_short_run_produces_a_complete_report() {
        let gateway = gateway();
        let request = RequestEnvelope {
            api_version: "1.2.3".into(),
            operation: "memory.store_episode".into(),
            context: context("smoke"),
            input: episode("session-bench", "agent-bench", 0),
        };
        let response = gateway.dispatch(request);
        assert_eq!(response.status, ResponseStatus::Ok, "{:?}", response.error);
    }
}
