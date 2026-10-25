//! Parity operations (`CAPI-14..27`) ??CodeGraph gap closure.
//!
//! Every operation is exercised through the §41.4 sub-cases that apply to it:
//! `valid`, `invalid_schema`, `cross_scope` (authorization), `version`
//! (unknown/stale revision), and `failure` (operational failure). The service
//! must return a typed `CodeError` for each negative path, never a panic.

#![cfg(feature = "parser-rust")]

use std::sync::Arc;

use code_intelligence::domain::{
    AiContextQuery, CircularDepsQuery, ComplexityQuery, ContextIntent, CuratedContextQuery,
    DeadImportsQuery, EntryPointsQuery, ErrorSearchQuery, HotPathsQuery, IndexFile, IndexFiles,
    ModuleSummaryQuery, PageRequest, PatternScope, PatternSearchQuery, PrContextFormat,
    PrContextQuery, RelatedTestsQuery, RepoRevision, SignatureQuery, SourceFile,
};
use code_intelligence::parsers::rust::RustParser;
use code_intelligence::service::{
    CodeIntelligenceService, InMemorySources, SCOPE_INDEX, SCOPE_INDEX_ADMIN, SCOPE_READ,
};
use code_intelligence::CodeError;
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const REPO: &str = "repo-1";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
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

fn source_set() -> InMemorySources {
    InMemorySources::new()
        .with_commit(
            COMMIT_A,
            vec![
                SourceFile::new("src/lib.rs", LIB_RS),
                SourceFile::new("tests/check.rs", CHECK_RS),
            ],
        )
        .with_commit(
            COMMIT_B,
            vec![SourceFile::new("src/lib.rs", "fn main() { let _ = 1; }\n")],
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

fn read_ctx() -> RequestContext {
    base_ctx("tenant-1", &[SCOPE_READ])
}

fn index_ctx() -> RequestContext {
    base_ctx("tenant-1", &[SCOPE_READ, SCOPE_INDEX, SCOPE_INDEX_ADMIN])
}

/// A ctx whose scope is `tenant-2`, so any `tenant-1` revision is cross-scope.
fn other_tenant_ctx() -> RequestContext {
    base_ctx("tenant-2", &[SCOPE_READ])
}

/// The index is always built by tenant-1's index-scoped principal. Read,
/// authorization and cross-tenant tests then query with their own context.
fn analyze(svc: &CodeIntelligenceService, commit: &str) {
    let ctx = index_ctx();
    svc.analyze_repository(
        &ctx,
        &code_intelligence::domain::AnalyzeRepository {
            revision: RepoRevision::git(REPO, commit),
            parser_profile: PROFILE.to_string(),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            asynchronous: true,
        },
    )
    .expect("analysis must be accepted");
}

fn is_forbidden(error: &CodeError) -> bool {
    matches!(error, CodeError::Forbidden { .. })
}

fn is_resource_limit(error: &CodeError) -> bool {
    matches!(error, CodeError::ResourceLimit { .. })
}

fn id_of(svc: &CodeIntelligenceService, ctx: &RequestContext, name: &str) -> String {
    let page = svc
        .find_symbols(
            ctx,
            &code_intelligence::domain::SymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
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
        .unwrap_or_else(|| panic!("`{name}` not found"))
}

fn is_not_found(error: &CodeError) -> bool {
    matches!(error, CodeError::NotFound { .. })
}

fn is_invalid(error: &CodeError) -> bool {
    matches!(error, CodeError::InvalidArgument { .. })
}

// ---------------------------------------------------------------------------
// CAPI-14 code.get_ai_context
// ---------------------------------------------------------------------------

#[test]
fn ai_context_valid_returns_sections() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let add_id = id_of(&svc, &ctx, "add");
    let result = svc
        .get_ai_context(
            &ctx,
            &AiContextQuery {
                intent: ContextIntent::Explain,
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_refs: vec![add_id],
                max_tokens: 4096,
            },
        )
        .expect("valid context request succeeds");
    assert!(!result.sections.is_empty());
    assert!(result.token_total > 0);
}

#[test]
fn ai_context_invalid_when_budget_is_zero() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_ai_context(
            &ctx,
            &AiContextQuery {
                intent: ContextIntent::Explain,
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_refs: vec!["add".to_string()],
                max_tokens: 0,
            },
        )
        .expect_err("a zero budget is invalid");
    assert!(is_invalid(&error) || matches!(&error, CodeError::ContextBudgetExceeded { .. }));
}

#[test]
fn ai_context_requires_the_read_scope() {
    let svc = service();
    let ctx = base_ctx("tenant-1", &[]);
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_ai_context(
            &ctx,
            &AiContextQuery {
                intent: ContextIntent::Explain,
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_refs: vec!["add".to_string()],
                max_tokens: 4096,
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn ai_context_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_ai_context(
            &ctx,
            &AiContextQuery {
                intent: ContextIntent::Explain,
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                target_refs: vec!["add".to_string()],
                max_tokens: 4096,
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn ai_context_cross_scope_is_forbidden() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_ai_context(
            &other_tenant_ctx(),
            &AiContextQuery {
                intent: ContextIntent::Explain,
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_refs: vec!["add".to_string()],
                max_tokens: 4096,
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-15 code.get_curated_context
// ---------------------------------------------------------------------------

#[test]
fn curated_context_valid_ranks_matches() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let result = svc
        .get_curated_context(
            &ctx,
            &CuratedContextQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "add twice arithmetic".to_string(),
                max_results: 5,
            },
        )
        .expect("valid curated request succeeds");
    assert_eq!(result.matches.len(), result.matches.len().min(5));
}

#[test]
fn curated_context_invalid_when_max_results_out_of_range() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_curated_context(
            &ctx,
            &CuratedContextQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "add".to_string(),
                max_results: 100,
            },
        )
        .expect_err("max_results > 25 is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn curated_context_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_curated_context(
            &base_ctx("tenant-1", &[]),
            &CuratedContextQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "add".to_string(),
                max_results: 5,
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn curated_context_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_curated_context(
            &ctx,
            &CuratedContextQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                query: "add".to_string(),
                max_results: 5,
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn curated_context_empty_query_is_invalid() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_curated_context(
            &ctx,
            &CuratedContextQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                query: "".to_string(),
                max_results: 5,
            },
        )
        .expect_err("an empty query is invalid");
    assert!(is_invalid(&error));
}

// ---------------------------------------------------------------------------
// CAPI-16 code.analyze_complexity
// ---------------------------------------------------------------------------

#[test]
fn complexity_valid_reports_functions() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let reports = svc
        .analyze_complexity(
            &ctx,
            &ComplexityQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_ids: Vec::new(),
                max_results: 50,
            },
        )
        .expect("valid complexity request succeeds");
    assert!(!reports.is_empty());
    for report in &reports {
        assert!(report.cyclomatic >= 1);
    }
}

#[test]
fn complexity_invalid_when_max_results_out_of_range() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .analyze_complexity(
            &ctx,
            &ComplexityQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_ids: Vec::new(),
                max_results: 101,
            },
        )
        .expect_err("max_results > 100 is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn complexity_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .analyze_complexity(
            &base_ctx("tenant-1", &[]),
            &ComplexityQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_ids: Vec::new(),
                max_results: 50,
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn complexity_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .analyze_complexity(
            &ctx,
            &ComplexityQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                symbol_ids: Vec::new(),
                max_results: 50,
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn complexity_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .analyze_complexity(
            &other_tenant_ctx(),
            &ComplexityQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_ids: Vec::new(),
                max_results: 50,
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-17 code.find_circular_deps
// ---------------------------------------------------------------------------

#[test]
fn circular_deps_valid_returns_a_page() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .find_circular_deps(
            &ctx,
            &CircularDepsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                edge_kinds: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect("valid circular-deps request succeeds");
    // The fixture has no module cycles, so the page is empty but well-formed.
    assert!(page.items.iter().all(|cycle| !cycle.files.is_empty()));
}

#[test]
fn circular_deps_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_circular_deps(
            &base_ctx("tenant-1", &[]),
            &CircularDepsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                edge_kinds: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn circular_deps_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_circular_deps(
            &ctx,
            &CircularDepsQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                edge_kinds: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn circular_deps_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_circular_deps(
            &other_tenant_ctx(),
            &CircularDepsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                edge_kinds: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-18 code.find_hot_paths
// ---------------------------------------------------------------------------

#[test]
fn hot_paths_valid_ranks_by_callers() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let paths = svc
        .find_hot_paths(
            &ctx,
            &HotPathsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                max_results: 20,
            },
        )
        .expect("valid hot-paths request succeeds");
    assert!(paths.len() <= 20);
}

#[test]
fn hot_paths_invalid_when_max_results_out_of_range() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_hot_paths(
            &ctx,
            &HotPathsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                max_results: 101,
            },
        )
        .expect_err("max_results > 100 is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn hot_paths_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_hot_paths(
            &base_ctx("tenant-1", &[]),
            &HotPathsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                max_results: 20,
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn hot_paths_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_hot_paths(
            &ctx,
            &HotPathsQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                max_results: 20,
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn hot_paths_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_hot_paths(
            &other_tenant_ctx(),
            &HotPathsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                max_results: 20,
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-19 code.find_dead_imports
// ---------------------------------------------------------------------------

#[test]
fn dead_imports_valid_returns_a_page() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .find_dead_imports(
            &ctx,
            &DeadImportsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect("valid dead-imports request succeeds");
    // Every reported import must name an importing file and a target.
    for import in &page.items {
        assert!(!import.importer_file.is_empty());
        assert!(!import.target.is_empty());
    }
}

#[test]
fn dead_imports_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_dead_imports(
            &base_ctx("tenant-1", &[]),
            &DeadImportsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn dead_imports_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_dead_imports(
            &ctx,
            &DeadImportsQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn dead_imports_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_dead_imports(
            &other_tenant_ctx(),
            &DeadImportsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-20 code.get_module_summary
// ---------------------------------------------------------------------------

#[test]
fn module_summary_valid_groups_by_directory() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .get_module_summary(
            &ctx,
            &ModuleSummaryQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                path_prefix: None,
                page: PageRequest::new(50),
            },
        )
        .expect("valid module-summary request succeeds");
    assert!(!page.items.is_empty());
    for module in &page.items {
        assert!(module.file_count >= 1);
    }
}

#[test]
fn module_summary_invalid_when_page_limit_is_zero() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_module_summary(
            &ctx,
            &ModuleSummaryQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                path_prefix: None,
                page: PageRequest::new(0),
            },
        )
        .expect_err("page.limit 0 is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn module_summary_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_module_summary(
            &base_ctx("tenant-1", &[]),
            &ModuleSummaryQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                path_prefix: None,
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn module_summary_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_module_summary(
            &ctx,
            &ModuleSummaryQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                path_prefix: None,
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn module_summary_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .get_module_summary(
            &other_tenant_ctx(),
            &ModuleSummaryQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                path_prefix: None,
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-21 code.search_by_pattern
// ---------------------------------------------------------------------------

#[test]
fn pattern_search_valid_finds_hits() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .search_by_pattern(
            &ctx,
            &PatternSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                pattern: "twice".to_string(),
                scope: PatternScope::All,
                page: PageRequest::new(50),
            },
        )
        .expect("valid pattern search succeeds");
    assert!(!page.items.is_empty());
}

#[test]
fn pattern_search_invalid_regex_is_invalid_argument() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_pattern(
            &ctx,
            &PatternSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                pattern: "([unclosed".to_string(),
                scope: PatternScope::All,
                page: PageRequest::new(50),
            },
        )
        .expect_err("a malformed regex is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn pattern_search_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_pattern(
            &base_ctx("tenant-1", &[]),
            &PatternSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                pattern: "twice".to_string(),
                scope: PatternScope::All,
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn pattern_search_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_pattern(
            &ctx,
            &PatternSearchQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                pattern: "twice".to_string(),
                scope: PatternScope::All,
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn pattern_search_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_pattern(
            &other_tenant_ctx(),
            &PatternSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                pattern: "twice".to_string(),
                scope: PatternScope::All,
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-22 code.search_by_error
// ---------------------------------------------------------------------------

#[test]
fn error_search_valid_returns_a_page() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .search_by_error(
            &ctx,
            &ErrorSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                error_types: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect("valid error search succeeds");
    for match_ in &page.items {
        assert!(!match_.role.is_empty());
    }
}

#[test]
fn error_search_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_error(
            &base_ctx("tenant-1", &[]),
            &ErrorSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                error_types: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn error_search_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_error(
            &ctx,
            &ErrorSearchQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                error_types: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn error_search_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .search_by_error(
            &other_tenant_ctx(),
            &ErrorSearchQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                error_types: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-23 code.find_by_signature
// ---------------------------------------------------------------------------

#[test]
fn signature_search_valid_returns_symbols() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .find_by_signature(
            &ctx,
            &SignatureQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                param_count: Some(2),
                return_type: None,
                modifiers: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect("valid signature search succeeds");
    // `add(a, b)` and `sub`-free fixture: at least the 2-arg `add` matches.
    assert!(!page.items.is_empty());
}

#[test]
fn signature_search_invalid_when_page_limit_is_zero() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_by_signature(
            &ctx,
            &SignatureQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                param_count: Some(2),
                return_type: None,
                modifiers: Vec::new(),
                page: PageRequest::new(0),
            },
        )
        .expect_err("page.limit 0 is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn signature_search_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_by_signature(
            &base_ctx("tenant-1", &[]),
            &SignatureQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                param_count: Some(2),
                return_type: None,
                modifiers: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn signature_search_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_by_signature(
            &ctx,
            &SignatureQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                param_count: Some(2),
                return_type: None,
                modifiers: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn signature_search_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_by_signature(
            &other_tenant_ctx(),
            &SignatureQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                param_count: Some(2),
                return_type: None,
                modifiers: Vec::new(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-24 code.find_entry_points
// ---------------------------------------------------------------------------

#[test]
fn entry_points_valid_finds_main() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let page = svc
        .find_entry_points(
            &ctx,
            &EntryPointsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect("valid entry-points request succeeds");
    assert!(
        page.items.iter().any(|entry| entry.name == "main"),
        "`main` must be an entry point"
    );
}

#[test]
fn entry_points_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_entry_points(
            &base_ctx("tenant-1", &[]),
            &EntryPointsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn entry_points_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_entry_points(
            &ctx,
            &EntryPointsQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn entry_points_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_entry_points(
            &other_tenant_ctx(),
            &EntryPointsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-25 code.find_related_tests
// ---------------------------------------------------------------------------

#[test]
fn related_tests_valid_returns_a_page() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let symbol_id = id_of(&svc, &ctx, "add");
    let page = svc
        .find_related_tests(
            &ctx,
            &RelatedTestsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id,
                page: PageRequest::new(50),
            },
        )
        .expect("valid related-tests request succeeds");
    for test in &page.items {
        assert!(!test.name.is_empty());
    }
}

#[test]
fn related_tests_invalid_for_unknown_symbol() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_related_tests(
            &ctx,
            &RelatedTestsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: "does-not-exist".to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unknown symbol is not found");
    assert!(is_not_found(&error));
}

#[test]
fn related_tests_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_related_tests(
            &base_ctx("tenant-1", &[]),
            &RelatedTestsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: "any".to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn related_tests_unknown_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_related_tests(
            &ctx,
            &RelatedTestsQuery {
                repository_id: REPO.to_string(),
                revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                symbol_id: "any".to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("an unindexed revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn related_tests_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .find_related_tests(
            &other_tenant_ctx(),
            &RelatedTestsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: "any".to_string(),
                page: PageRequest::new(50),
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}

// ---------------------------------------------------------------------------
// CAPI-26 code.index_files
// ---------------------------------------------------------------------------

#[test]
fn index_files_valid_extends_the_base_revision() {
    let svc = service();
    let ctx = index_ctx();
    analyze(&svc, COMMIT_A);
    let result = svc
        .index_files(
            &ctx,
            &IndexFiles {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                files: vec![IndexFile {
                    path: "src/new.rs".to_string(),
                    content: "fn added() -> i32 { 1 }\n".to_string(),
                }],
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect("valid index_files succeeds");
    assert_eq!(result.indexed_files, 1);
    assert!(result.dirty);
    assert_ne!(result.revision, COMMIT_A);
}

#[test]
fn index_files_invalid_when_no_files() {
    let svc = service();
    let ctx = index_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .index_files(
            &ctx,
            &IndexFiles {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                files: Vec::new(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("an empty file list is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn index_files_invalid_when_path_escapes_the_repo() {
    let svc = service();
    let ctx = index_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .index_files(
            &ctx,
            &IndexFiles {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                files: vec![IndexFile {
                    path: "../escape.rs".to_string(),
                    content: "fn x() {}\n".to_string(),
                }],
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("a `..` path is invalid");
    assert!(is_invalid(&error));
}

#[test]
fn index_files_requires_the_index_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    let error = svc
        .index_files(
            &base_ctx("tenant-1", &[SCOPE_READ]),
            &IndexFiles {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                files: vec![IndexFile {
                    path: "src/new.rs".to_string(),
                    content: "fn added() -> i32 { 1 }\n".to_string(),
                }],
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("writing without the index scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn index_files_failure_on_unsupported_language_leaves_no_half_index() {
    let svc = service();
    let ctx = index_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .index_files(
            &ctx,
            &IndexFiles {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                files: vec![IndexFile {
                    path: "src/unknown.xyz".to_string(),
                    content: "not a supported language".to_string(),
                }],
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("an unsupported language fails the whole request");
    assert!(matches!(error, CodeError::CapabilityUnavailable { .. }));
    // The base revision is still intact and readable.
    let page = svc
        .get_module_summary(
            &ctx,
            &ModuleSummaryQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                path_prefix: None,
                page: PageRequest::new(50),
            },
        )
        .expect("the base revision is untouched");
    assert!(!page.items.is_empty());
}

// ---------------------------------------------------------------------------
// CAPI-27 code.pr_context
// ---------------------------------------------------------------------------

#[test]
fn pr_context_valid_reports_the_change() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    analyze(&svc, COMMIT_B);
    let report = svc
        .pr_context(
            &ctx,
            &PrContextQuery {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                head_revision: COMMIT_B.to_string(),
                format: PrContextFormat::Json,
                max_depth: 3,
                max_nodes: 100,
            },
        )
        .expect("valid pr_context succeeds");
    // `add` and `twice` were removed between the revisions, so `changed` is non-empty.
    assert!(!report.changed.is_empty());
}

#[test]
fn pr_context_invalid_when_depth_exceeds_the_traversal_bound() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    analyze(&svc, COMMIT_B);
    let error = svc
        .pr_context(
            &ctx,
            &PrContextQuery {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                head_revision: COMMIT_B.to_string(),
                format: PrContextFormat::Json,
                max_depth: code_intelligence::service::MAX_TRAVERSAL_DEPTH + 1,
                max_nodes: 100,
            },
        )
        .expect_err("max_depth beyond the bound is a resource limit");
    assert!(is_resource_limit(&error));
}

#[test]
fn pr_context_requires_the_read_scope() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    analyze(&svc, COMMIT_B);
    let error = svc
        .pr_context(
            &base_ctx("tenant-1", &[]),
            &PrContextQuery {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                head_revision: COMMIT_B.to_string(),
                format: PrContextFormat::Json,
                max_depth: 3,
                max_nodes: 100,
            },
        )
        .expect_err("missing scope is forbidden");
    assert!(is_forbidden(&error));
}

#[test]
fn pr_context_unknown_head_revision_is_not_found() {
    let svc = service();
    let ctx = read_ctx();
    analyze(&svc, COMMIT_A);
    let error = svc
        .pr_context(
            &ctx,
            &PrContextQuery {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                head_revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
                format: PrContextFormat::Json,
                max_depth: 3,
                max_nodes: 100,
            },
        )
        .expect_err("an unindexed head revision is not found");
    assert!(is_not_found(&error));
}

#[test]
fn pr_context_cross_scope_is_invisible() {
    let svc = service();
    analyze(&svc, COMMIT_A);
    analyze(&svc, COMMIT_B);
    let error = svc
        .pr_context(
            &other_tenant_ctx(),
            &PrContextQuery {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                head_revision: COMMIT_B.to_string(),
                format: PrContextFormat::Json,
                max_depth: 3,
                max_nodes: 100,
            },
        )
        .expect_err("another tenant's revision is invisible");
    assert!(is_not_found(&error) || is_forbidden(&error));
}
