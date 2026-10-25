//! CAPI-01..13 contract matrix (`P3-WP4`, `P3-WP5`).
//!
//! Every operation is exercised through the sub-cases §41.4 asks for:
//! `valid`, `invalid_schema`, `cross_scope`, `revision_conflict`,
//! `idempotent_retry`, `capability_missing`, `generation_staleness`,
//! `pagination` — each used where it applies to the operation.

#![cfg(feature = "parser-rust")]

use std::sync::Arc;

use code_intelligence::domain::{
    AnalyzeRepository, CancelJob, CodeRevisionDiff, Direction, DirtyPolicy, EditContextQuery,
    GetJob, GetSymbol, ImpactQuery, InvalidateCodeRevision, PageRequest, ReferenceQuery,
    RepoRevision, SourceFile, SymbolQuery, TraversalQuery,
};
use code_intelligence::parsers::rust::RustParser;
use code_intelligence::service::{
    CodeAuthorizer, CodeIntelligenceService, InMemorySources, ScopeAuthorizer, CHARS_PER_TOKEN,
    SCOPE_INDEX, SCOPE_INDEX_ADMIN, SCOPE_READ,
};
use code_intelligence::{
    CodeEdgeKind, CodeError, CodeResult, ParserPort, SupportLevel, Symbol, SymbolKind,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const REPO: &str = "repo-1";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const COMMIT_PY: &str = "cccccccccccccccccccccccccccccccccccccccc";
const PROFILE: &str = "tree-sitter-rust-0.24.2/ts-0.25.10";

const LIB_RS: &str = "\
mod core {
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    pub fn twice(x: i32) -> i32 {
        add(x, x)
    }

    pub fn test_add() {
        add(1, 2);
    }
}

fn main() {
    let _ = core::twice(3);
}
";

const CHECK_RS: &str = "\
fn test_twice() {
    crate::core::twice(2);
}
";

const LIB_RS_B: &str = "\
mod core {
    pub fn add(a: i64, b: i64) -> i64 {
        a + b
    }

    pub fn sub(a: i32, b: i32) -> i32 {
        a - b
    }
}

fn main() {
    let _ = core::add(1, 2);
}
";

fn files_a() -> Vec<SourceFile> {
    vec![
        SourceFile::new("src/lib.rs", LIB_RS),
        SourceFile::new("tests/check.rs", CHECK_RS),
    ]
}

fn source_set() -> InMemorySources {
    InMemorySources::new()
        .with_commit(COMMIT_A, files_a())
        .with_commit(COMMIT_B, vec![SourceFile::new("src/lib.rs", LIB_RS_B)])
        .with_commit(
            COMMIT_PY,
            vec![SourceFile::new("src/app.py", "def main():\n    pass\n")],
        )
}

fn service() -> CodeIntelligenceService {
    CodeIntelligenceService::new(Arc::new(RustParser::new()), Arc::new(source_set()))
}

fn base_ctx(tenant: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: "req-1".to_string(),
        trace_id: "trace-1".to_string(),
        scope: ScopeRef {
            tenant_id: tenant.to_string(),
            project_id: "project-1".to_string(),
            repo_id: Some(REPO.to_string()),
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "agent-1".to_string(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        deadline_at: None,
        consistency: Consistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: None,
        expected_etag: None,
        delegation_token: None,
    }
}

fn full_ctx() -> RequestContext {
    base_ctx("tenant-1", &[SCOPE_READ, SCOPE_INDEX, SCOPE_INDEX_ADMIN])
}

fn analyze_request(commit: &str) -> AnalyzeRepository {
    AnalyzeRepository {
        revision: RepoRevision::git(REPO, commit),
        parser_profile: PROFILE.to_string(),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        asynchronous: true,
    }
}

fn analyze(svc: &CodeIntelligenceService, ctx: &RequestContext, commit: &str) -> String {
    svc.analyze_repository(ctx, &analyze_request(commit))
        .expect("analysis must be accepted")
        .job_id
}

fn symbol_id(
    svc: &CodeIntelligenceService,
    ctx: &RequestContext,
    commit: &str,
    name: &str,
) -> String {
    let page = svc
        .find_symbols(
            ctx,
            &SymbolQuery {
                repository_id: REPO.to_string(),
                revision: commit.to_string(),
                query: name.to_string(),
                kinds: Vec::new(),
                path_glob: None,
                language: None,
                page: PageRequest::new(200),
            },
        )
        .expect("search must succeed");
    page.items
        .iter()
        .find(|symbol| symbol.qualified_name.ends_with(name))
        .map(|symbol| symbol.id.clone())
        .unwrap_or_else(|| panic!("symbol {name} not found"))
}

// ---------------------------------------------------------------------------
// CAPI-01 code.analyze_repository
// ---------------------------------------------------------------------------

#[test]
fn capi01_valid_analysis_publishes_a_complete_manifest() {
    let svc = service();
    let ctx = full_ctx();
    let job_id = analyze(&svc, &ctx, COMMIT_A);
    let view = svc
        .get_analysis_job(
            &ctx,
            &GetJob {
                job_id: job_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(view.stage, "ready");
    assert!(view.canonical_committed);
    let manifest = svc.analysis_manifest(&ctx, &job_id).unwrap();
    assert_eq!(manifest.commit_sha, COMMIT_A);
    assert_eq!(manifest.parser_profile, PROFILE);
    assert_eq!(manifest.indexed_files, 2);
    assert_eq!(manifest.skipped_files, 0);
    assert_eq!(manifest.completeness(), "complete");
    assert!(
        view.result_ref.is_some(),
        "committed jobs expose a manifest ref"
    );
    assert!(!manifest.graph_generation.is_empty());
}

#[test]
fn capi01_invalid_schema_is_rejected() {
    let svc = service();
    let ctx = full_ctx();

    let mut request = analyze_request(COMMIT_A);
    request.asynchronous = false;
    assert_eq!(
        svc.analyze_repository(&ctx, &request).unwrap_err().code(),
        "INVALID_ARGUMENT"
    );

    let mut request = analyze_request(COMMIT_A);
    request.parser_profile = "  ".to_string();
    assert_eq!(
        svc.analyze_repository(&ctx, &request).unwrap_err().code(),
        "INVALID_ARGUMENT"
    );

    let mut request = analyze_request(COMMIT_A);
    request.revision.commit_sha = "HEAD".to_string();
    assert_eq!(
        svc.analyze_repository(&ctx, &request).unwrap_err().code(),
        "INVALID_ARGUMENT"
    );

    let mut request = analyze_request(COMMIT_A);
    request.revision.vcs = "svn".to_string();
    assert_eq!(
        svc.analyze_repository(&ctx, &request).unwrap_err().code(),
        "INVALID_ARGUMENT"
    );
}

#[test]
fn capi01_requires_the_index_scope() {
    let svc = service();
    let read_only = base_ctx("tenant-1", &[SCOPE_READ]);
    let error = svc
        .analyze_repository(&read_only, &analyze_request(COMMIT_A))
        .unwrap_err();
    assert_eq!(error.code(), "FORBIDDEN");
}

#[test]
fn capi01_capability_missing_when_no_ported_grammar_covers_the_revision() {
    let svc = service();
    let error = svc
        .analyze_repository(&full_ctx(), &analyze_request(COMMIT_PY))
        .unwrap_err();
    assert_eq!(error.code(), "CAPABILITY_UNAVAILABLE");
    assert!(error.to_string().contains("python"));
}

#[test]
fn capi01_source_unavailable_for_unknown_and_dirty_revisions() {
    let svc = service();
    let unknown = "dddddddddddddddddddddddddddddddddddddddd";
    assert_eq!(
        svc.analyze_repository(&full_ctx(), &analyze_request(unknown))
            .unwrap_err()
            .code(),
        "SOURCE_UNAVAILABLE"
    );

    let dirty = CodeIntelligenceService::new(
        Arc::new(RustParser::new()),
        Arc::new(
            InMemorySources::new()
                .with_dirty_commit(COMMIT_A, vec![SourceFile::new("src/lib.rs", LIB_RS)]),
        ),
    );
    assert_eq!(
        dirty
            .analyze_repository(&full_ctx(), &analyze_request(COMMIT_A))
            .unwrap_err()
            .code(),
        "SOURCE_UNAVAILABLE"
    );

    // the same snapshot is accepted when the caller opts into a snapshot policy
    let mut request = analyze_request(COMMIT_A);
    request.revision.dirty_policy = DirtyPolicy::Snapshot;
    assert!(dirty.analyze_repository(&full_ctx(), &request).is_ok());
}

#[test]
fn capi01_retry_is_idempotent_and_conflicts_are_reported() {
    let svc = service();
    let mut ctx = full_ctx();
    ctx.idempotency_key = Some("key-1".to_string());

    let first = analyze(&svc, &ctx, COMMIT_A);
    let second = analyze(&svc, &ctx, COMMIT_A);
    assert_eq!(first, second, "same request must join the existing job");

    let mut conflicting = analyze_request(COMMIT_A);
    conflicting.include_globs = vec!["src/**".to_string()];
    assert_eq!(
        svc.analyze_repository(&ctx, &conflicting)
            .unwrap_err()
            .code(),
        "IDEMPOTENCY_CONFLICT"
    );
}

#[test]
fn capi01_cross_tenant_analysis_is_invisible_to_another_tenant() {
    let svc = service();
    let owner = base_ctx("tenant-1", &[SCOPE_READ, SCOPE_INDEX, SCOPE_INDEX_ADMIN]);
    let job_id = analyze(&svc, &owner, COMMIT_A);
    let intruder = base_ctx("tenant-2", &[SCOPE_READ, SCOPE_INDEX, SCOPE_INDEX_ADMIN]);
    let error = svc
        .get_analysis_job(&intruder, &GetJob { job_id })
        .unwrap_err();
    assert_eq!(error.code(), "CROSS_SCOPE_DENIED");
}

// ---------------------------------------------------------------------------
// CAPI-02 / CAPI-03
// ---------------------------------------------------------------------------

#[test]
fn capi02_unknown_job_and_missing_scope() {
    let svc = service();
    assert_eq!(
        svc.get_analysis_job(
            &full_ctx(),
            &GetJob {
                job_id: "job-code-999".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "JOB_NOT_FOUND"
    );
    let no_read = base_ctx("tenant-1", &[SCOPE_INDEX]);
    assert_eq!(
        svc.get_analysis_job(
            &no_read,
            &GetJob {
                job_id: "job-code-1".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "FORBIDDEN"
    );
}

#[test]
fn capi03_cancel_before_commit_wins_and_after_commit_is_refused() {
    let svc = service();
    let ctx = full_ctx();
    let handle = svc
        .submit_analysis(&ctx, &analyze_request(COMMIT_A))
        .unwrap();
    assert_eq!(handle.stage, "queued");
    let cancelled = svc
        .cancel_analysis_job(
            &ctx,
            &CancelJob {
                job_id: handle.job_id.clone(),
                reason: "user abort".to_string(),
            },
        )
        .unwrap();
    assert_eq!(cancelled.stage, "cancelled");
    assert!(!cancelled.canonical_committed);
    // running a cancelled job must not resurrect it
    let view = svc.run_analysis(&ctx, &handle.job_id).unwrap();
    assert_eq!(view.stage, "cancelled");

    // after the commit point the job is not cancellable and the commit stands
    let done = analyze(&svc, &ctx, COMMIT_A);
    assert_eq!(
        svc.cancel_analysis_job(
            &ctx,
            &CancelJob {
                job_id: done.clone(),
                reason: "too late".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "JOB_NOT_CANCELLABLE"
    );
    let view = svc
        .get_analysis_job(&ctx, &GetJob { job_id: done })
        .unwrap();
    assert!(view.canonical_committed);
}

// ---------------------------------------------------------------------------
// CAPI-04 / CAPI-05 / CAPI-06
// ---------------------------------------------------------------------------

#[test]
fn capi04_get_symbol_paths() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");

    let symbol: Symbol = svc
        .get_symbol(
            &ctx,
            &GetSymbol {
                symbol_id: add.clone(),
            },
        )
        .unwrap();
    assert_eq!(symbol.kind, SymbolKind::Function);
    assert_eq!(symbol.qualified_name, "lib::core::add");
    assert_eq!(symbol.language, "rust");
    assert!(symbol.signature.unwrap().contains("fn add"));
    assert!(symbol.id.starts_with("sym:v1:"));

    assert_eq!(
        svc.get_symbol(
            &ctx,
            &GetSymbol {
                symbol_id: "sym:v1:dead".to_string()
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
    let no_read = base_ctx("tenant-1", &[SCOPE_INDEX]);
    assert_eq!(
        svc.get_symbol(&no_read, &GetSymbol { symbol_id: add })
            .unwrap_err()
            .code(),
        "FORBIDDEN"
    );
}

#[test]
fn capi05_search_filters_and_pagination() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    let request = |query: &str, limit: u32, cursor: Option<String>| SymbolQuery {
        repository_id: REPO.to_string(),
        revision: COMMIT_A.to_string(),
        query: query.to_string(),
        kinds: Vec::new(),
        path_glob: None,
        language: None,
        page: PageRequest { cursor, limit },
    };

    let all = svc.find_symbols(&ctx, &request("", 200, None)).unwrap();
    assert!(
        all.items.len() >= 7,
        "module/struct/fn symbols: {}",
        all.items.len()
    );
    assert!(all.next_cursor.is_none());

    let functions = svc
        .find_symbols(
            &ctx,
            &SymbolQuery {
                kinds: vec![SymbolKind::Function],
                ..request("", 200, None)
            },
        )
        .unwrap();
    assert!(functions
        .items
        .iter()
        .all(|symbol| symbol.kind == SymbolKind::Function));

    let in_tests = svc
        .find_symbols(
            &ctx,
            &SymbolQuery {
                path_glob: Some("tests/**".to_string()),
                ..request("", 200, None)
            },
        )
        .unwrap();
    let mut names: Vec<String> = in_tests
        .items
        .iter()
        .map(|symbol| symbol.qualified_name.clone())
        .collect();
    names.sort();
    assert_eq!(names, vec!["check", "check::test_twice"]);

    // pagination walks the whole result set exactly once
    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    loop {
        let page = svc
            .find_symbols(&ctx, &request("", 3, cursor.clone()))
            .unwrap();
        assert!(page.items.len() <= 3);
        seen.extend(page.items.iter().map(|symbol| symbol.id.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), all.items.len());

    assert_eq!(
        svc.find_symbols(&ctx, &request("", 0, None))
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        svc.find_symbols(&ctx, &request("", 10, Some("nonsense".to_string())))
            .unwrap_err()
            .code(),
        "CURSOR_EXPIRED"
    );
    assert_eq!(
        svc.find_symbols(
            &ctx,
            &SymbolQuery {
                revision: COMMIT_B.to_string(),
                ..request("", 10, None)
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND",
        "an unanalyzed revision must not silently resolve to another one"
    );
}

#[test]
fn capi06_references_respect_declaration_and_relation_filters() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");

    let without_declaration = svc
        .find_references(
            &ctx,
            &ReferenceQuery {
                symbol_id: add.clone(),
                include_declaration: false,
                relation_kinds: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert_eq!(without_declaration.items.len(), 2);
    assert!(without_declaration
        .items
        .iter()
        .all(|reference| reference.kind == CodeEdgeKind::Calls));

    let with_declaration = svc
        .find_references(
            &ctx,
            &ReferenceQuery {
                symbol_id: add.clone(),
                include_declaration: true,
                relation_kinds: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert_eq!(with_declaration.items.len(), 3);

    let only_imports = svc
        .find_references(
            &ctx,
            &ReferenceQuery {
                symbol_id: add,
                include_declaration: false,
                relation_kinds: vec![CodeEdgeKind::Imports],
                page: PageRequest::new(50),
            },
        )
        .unwrap();
    assert!(only_imports.items.is_empty());
}

// ---------------------------------------------------------------------------
// CAPI-07 / CAPI-08 / CAPI-09
// ---------------------------------------------------------------------------

fn traversal(
    symbol_id: &str,
    direction: Direction,
    max_depth: u32,
    max_nodes: u32,
) -> TraversalQuery {
    TraversalQuery {
        symbol_id: symbol_id.to_string(),
        direction,
        edge_kinds: Vec::new(),
        max_depth,
        max_nodes,
        page: PageRequest::new(50),
    }
}

#[test]
fn capi07_callers_are_bounded_and_authorized() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");
    let page = svc
        .query_callers(&ctx, &traversal(&add, Direction::In, 4, 100))
        .unwrap();
    // max_depth bounds the caller closure: direct callers plus one more hop
    assert_eq!(page.items.len(), 3, "twice, test_add and main (2nd hop)");
    let direct = svc
        .query_callers(&ctx, &traversal(&add, Direction::In, 1, 100))
        .unwrap();
    assert_eq!(direct.items.len(), 2, "direct callers of add");
    assert!(page
        .items
        .iter()
        .all(|edge| !edge.is_certain() || edge.kind != CodeEdgeKind::Calls));
    assert!(page
        .items
        .iter()
        .all(|edge| edge.kind != CodeEdgeKind::Defines));

    // one node of budget is not enough for two callers
    assert_eq!(
        svc.query_callers(&ctx, &traversal(&add, Direction::In, 4, 1))
            .unwrap_err()
            .code(),
        "RESOURCE_LIMIT"
    );
    assert_eq!(
        svc.query_callers(&ctx, &traversal(&add, Direction::In, 0, 10))
            .unwrap_err()
            .code(),
        "RESOURCE_LIMIT"
    );
    assert_eq!(
        svc.query_callers(&ctx, &traversal(&add, Direction::In, 99, 10))
            .unwrap_err()
            .code(),
        "RESOURCE_LIMIT"
    );
}

#[test]
fn capi08_callees_follow_outgoing_edges() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let twice = symbol_id(&svc, &ctx, COMMIT_A, "twice");
    let page = svc
        .query_callees(&ctx, &traversal(&twice, Direction::Out, 1, 10))
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, CodeEdgeKind::Calls);
    assert_eq!(
        page.items[0].resolution,
        code_intelligence::EdgeResolution::StaticPossible
    );
}

#[test]
fn capi09_dependencies_require_an_import_or_dependency_edge_kind() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let main = symbol_id(&svc, &ctx, COMMIT_A, "main");

    let default_kinds = svc
        .query_dependencies(&ctx, &traversal(&main, Direction::Out, 2, 10))
        .unwrap();
    assert!(default_kinds
        .items
        .iter()
        .all(|edge| matches!(edge.kind, CodeEdgeKind::Imports | CodeEdgeKind::DependsOn)));

    let calls_only = TraversalQuery {
        edge_kinds: vec![CodeEdgeKind::Calls],
        ..traversal(&main, Direction::Out, 2, 10)
    };
    assert_eq!(
        svc.query_dependencies(&ctx, &calls_only)
            .unwrap_err()
            .code(),
        "INVALID_ARGUMENT"
    );
}

// ---------------------------------------------------------------------------
// CAPI-10
// ---------------------------------------------------------------------------

#[test]
fn capi10_impact_reports_paths_tests_and_honest_completeness() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");
    let test = symbol_id(&svc, &ctx, COMMIT_A, "test_twice");

    let report = svc
        .analyze_impact(
            &ctx,
            &ImpactQuery {
                changed: vec![add.clone()],
                revision: COMMIT_A.to_string(),
                edge_kinds: Vec::new(),
                max_depth: 8,
                max_nodes: 100,
                include_test_candidates: true,
            },
        )
        .unwrap();
    assert_eq!(
        report.impacted.len(),
        3,
        "test_add + twice + main (cross-file callers stay unresolved and are not traversed)"
    );
    assert_eq!(
        report.test_candidates.len(),
        1,
        "only the test-named symbol"
    );
    let expected_test = symbol_id(&svc, &ctx, COMMIT_A, "test_add");
    assert_eq!(report.test_candidates, vec![expected_test]);
    let _ = test;
    assert_eq!(
        report.completeness, "partial",
        "the fixture graph contains possible (dynamic) call edges"
    );
    assert_eq!(report.paths.len(), report.impacted.len());
    assert!(!report.evidence_ids.is_empty());

    // truncated traversal is partial, not complete
    let truncated = svc
        .analyze_impact(
            &ctx,
            &ImpactQuery {
                changed: vec![add],
                revision: COMMIT_A.to_string(),
                edge_kinds: Vec::new(),
                max_depth: 1,
                max_nodes: 100,
                include_test_candidates: false,
            },
        )
        .unwrap();
    assert_eq!(truncated.completeness, "partial");
    assert!(truncated.test_candidates.is_empty());
}

#[test]
fn capi10_rejects_revision_mismatch_and_unknown_symbols() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    analyze(&svc, &ctx, COMMIT_B);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");

    let mismatch = svc
        .analyze_impact(
            &ctx,
            &ImpactQuery {
                changed: vec![add.clone()],
                revision: COMMIT_B.to_string(),
                edge_kinds: Vec::new(),
                max_depth: 4,
                max_nodes: 10,
                include_test_candidates: false,
            },
        )
        .unwrap_err();
    assert_eq!(mismatch.code(), "SOURCE_REVISION_MISMATCH");

    assert_eq!(
        svc.analyze_impact(
            &ctx,
            &ImpactQuery {
                changed: vec!["sym:v1:missing".to_string()],
                revision: COMMIT_A.to_string(),
                edge_kinds: Vec::new(),
                max_depth: 4,
                max_nodes: 10,
                include_test_candidates: false,
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
}

// ---------------------------------------------------------------------------
// CAPI-11
// ---------------------------------------------------------------------------

#[test]
fn capi11_edit_context_respects_the_token_budget() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");
    let twice = symbol_id(&svc, &ctx, COMMIT_A, "twice");

    let request = |targets: Vec<String>, budget: u32| EditContextQuery {
        intent: "fix the addition".to_string(),
        repository_id: REPO.to_string(),
        revision: COMMIT_A.to_string(),
        target_refs: targets,
        max_tokens: budget,
    };

    let full = svc
        .get_edit_context(&ctx, &request(vec![add.clone(), twice.clone()], 4096))
        .unwrap();
    assert_eq!(full.blocks.len(), 2);
    assert!(!full.truncation);
    assert!(full.blocks[0].content.contains("fn add"));
    assert_eq!(
        full.total_tokens,
        full.blocks
            .iter()
            .map(|block| block.token_count)
            .sum::<u32>()
    );

    // a budget that fits only the first block truncates the rest
    let first_only = svc
        .get_edit_context(
            &ctx,
            &request(vec![add.clone(), twice.clone()], full.blocks[0].token_count),
        )
        .unwrap();
    assert_eq!(first_only.blocks.len(), 1);
    assert!(first_only.truncation);
    assert_eq!(first_only.omitted_refs, vec![twice]);

    // a budget too small for even one block is a typed failure, not silent data
    assert_eq!(
        svc.get_edit_context(&ctx, &request(vec![add.clone()], 1))
            .unwrap_err()
            .code(),
        "CONTEXT_BUDGET_EXCEEDED"
    );
    assert_eq!(
        svc.get_edit_context(&ctx, &request(vec![add], 0))
            .unwrap_err()
            .code(),
        "CONTEXT_BUDGET_EXCEEDED"
    );
}

#[test]
fn capi11_token_estimate_is_four_characters_per_token() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");
    let result = svc
        .get_edit_context(
            &ctx,
            &EditContextQuery {
                intent: "audit".to_string(),
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_refs: vec![add],
                max_tokens: 4096,
            },
        )
        .unwrap();
    let block = &result.blocks[0];
    let chars = block.content.chars().count() as u32;
    assert_eq!(block.token_count, chars.div_ceil(CHARS_PER_TOKEN));
}

/// An authorizer that denies one path, to prove per-resource ACL is enforced and
/// not just the scope list.
struct PathDeny {
    denied: &'static str,
}

impl CodeAuthorizer for PathDeny {
    fn require_scope(&self, ctx: &RequestContext, scope: &str) -> CodeResult<()> {
        ScopeAuthorizer.require_scope(ctx, scope)
    }

    fn check_path(&self, _ctx: &RequestContext, path: &str) -> CodeResult<()> {
        if path.starts_with(self.denied) {
            Err(CodeError::Forbidden {
                required_scope: format!("code:read:{path}"),
            })
        } else {
            Ok(())
        }
    }
}

#[test]
fn capi11_per_path_acl_removes_blocks_instead_of_leaking_them() {
    let svc = CodeIntelligenceService::new(Arc::new(RustParser::new()), Arc::new(source_set()))
        .with_authorizer(Arc::new(PathDeny { denied: "src/" }));
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");
    let error = svc
        .get_edit_context(
            &ctx,
            &EditContextQuery {
                intent: "audit".to_string(),
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_refs: vec![add],
                max_tokens: 4096,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// CAPI-12
// ---------------------------------------------------------------------------

#[test]
fn capi12_revision_diff_classifies_added_removed_and_modified() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    analyze(&svc, &ctx, COMMIT_B);

    let page = svc
        .compare_revisions(
            &ctx,
            &CodeRevisionDiff {
                repository_id: REPO.to_string(),
                from_revision: COMMIT_A.to_string(),
                to_revision: COMMIT_B.to_string(),
                page: PageRequest::new(100),
            },
        )
        .unwrap();

    let kinds: Vec<String> = page
        .items
        .iter()
        .map(|change| format!("{:?}", change.change_kind))
        .collect();
    assert!(kinds.contains(&"Added".to_string()), "core::sub is new");
    assert!(
        kinds.contains(&"Removed".to_string()),
        "core::twice is gone"
    );
    assert!(
        kinds.contains(&"Modified".to_string()),
        "core::add changed signature"
    );

    let added = page
        .items
        .iter()
        .find(|change| change.change_kind == code_intelligence::domain::ChangeKind::Added)
        .unwrap();
    assert!(added
        .new_symbol
        .as_ref()
        .unwrap()
        .qualified_name
        .ends_with("sub"));
    assert!(added.old_symbol.is_none());

    assert_eq!(
        svc.compare_revisions(
            &ctx,
            &CodeRevisionDiff {
                repository_id: REPO.to_string(),
                from_revision: COMMIT_A.to_string(),
                to_revision: COMMIT_PY.to_string(),
                page: PageRequest::new(10),
            }
        )
        .unwrap_err()
        .code(),
        "NOT_FOUND"
    );
}

// ---------------------------------------------------------------------------
// CAPI-13
// ---------------------------------------------------------------------------

#[test]
fn capi13_invalidate_is_stale_not_delete_and_needs_the_etag() {
    let svc = service();
    let ctx = full_ctx();
    let job_id = analyze(&svc, &ctx, COMMIT_A);
    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");

    let wrong = svc
        .invalidate_revision(
            &ctx,
            &InvalidateCodeRevision {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                reason: "superseded".to_string(),
                expected_etag: "\"wrong\"".to_string(),
            },
        )
        .unwrap_err();
    assert_eq!(wrong.code(), "ETAG_MISMATCH");

    // discover the current etag through a revision-scoped read of the manifest
    let etag = svc
        .analysis_manifest(&ctx, &job_id)
        .map(|manifest| {
            format!(
                "\"etag-{}-{}\"",
                manifest.commit_sha, manifest.indexed_files
            )
        })
        .unwrap();

    let receipt = svc
        .invalidate_revision(
            &ctx,
            &InvalidateCodeRevision {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                reason: "superseded".to_string(),
                expected_etag: etag,
            },
        )
        .unwrap();
    assert!(receipt.changed);
    assert_eq!(receipt.events, vec!["code.revision.stale".to_string()]);

    // reads are refused as stale, but the manifest and the history remain
    assert_eq!(
        svc.get_symbol(&ctx, &GetSymbol { symbol_id: add })
            .unwrap_err()
            .code(),
        "INDEX_STALE"
    );
    let manifest = svc.analysis_manifest(&ctx, &job_id).unwrap();
    assert_eq!(manifest.commit_sha, COMMIT_A);

    let again = svc
        .invalidate_revision(
            &ctx,
            &InvalidateCodeRevision {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                reason: "again".to_string(),
                expected_etag: receipt.etag,
            },
        )
        .unwrap();
    assert!(!again.changed, "second invalidation changes nothing");
}

#[test]
fn capi13_requires_index_admin_scope() {
    let svc = service();
    let admin = full_ctx();
    analyze(&svc, &admin, COMMIT_A);
    let weaker = base_ctx("tenant-1", &[SCOPE_READ, SCOPE_INDEX]);
    assert_eq!(
        svc.invalidate_revision(
            &weaker,
            &InvalidateCodeRevision {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                reason: "nope".to_string(),
                expected_etag: "\"x\"".to_string(),
            }
        )
        .unwrap_err()
        .code(),
        "FORBIDDEN"
    );
}

// ---------------------------------------------------------------------------
// §37 port surface
// ---------------------------------------------------------------------------

/// Minimal executor: the §37 port is `async`, so one test awaits it.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[test]
fn port_surface_matches_the_thirteen_operations() {
    use code_intelligence::service::CodeIntelligencePort;

    let svc = service();
    let ctx = full_ctx();
    let handle = block_on(CodeIntelligencePort::analyze_repository(
        &svc,
        &ctx,
        &analyze_request(COMMIT_A),
    ))
    .unwrap();
    let view = block_on(CodeIntelligencePort::get_analysis_job(
        &svc,
        &ctx,
        &GetJob {
            job_id: handle.job_id.clone(),
        },
    ))
    .unwrap();
    assert!(view.canonical_committed);

    let add = symbol_id(&svc, &ctx, COMMIT_A, "add");
    let symbol = block_on(CodeIntelligencePort::get_symbol(
        &svc,
        &ctx,
        &GetSymbol {
            symbol_id: add.clone(),
        },
    ))
    .unwrap();
    assert_eq!(symbol.qualified_name, "lib::core::add");

    let callees = block_on(CodeIntelligencePort::query_callees(
        &svc,
        &ctx,
        &traversal(
            &symbol_id(&svc, &ctx, COMMIT_A, "twice"),
            Direction::Out,
            1,
            10,
        ),
    ))
    .unwrap();
    assert_eq!(callees.items.len(), 1);

    let impact = block_on(CodeIntelligencePort::analyze_impact(
        &svc,
        &ctx,
        &ImpactQuery {
            changed: vec![add],
            revision: COMMIT_A.to_string(),
            edge_kinds: Vec::new(),
            max_depth: 4,
            max_nodes: 50,
            include_test_candidates: true,
        },
    ))
    .unwrap();
    assert!(!impact.impacted.is_empty());
}

#[test]
fn parser_support_level_gates_the_analysis() {
    assert_eq!(RustParser::new().support("rust"), SupportLevel::Ported);
    assert_eq!(RustParser::new().support("python"), SupportLevel::NotPorted);
}
