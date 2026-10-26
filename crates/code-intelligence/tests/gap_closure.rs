//! Phase 8 gap-closure domain tests: `code.find_implementors` (CAPI-28),
//! `code.traverse` (CAPI-29), `code.get_detailed_symbol` (CAPI-30),
//! `code.index_directory` (CAPI-31), `code.export_graph` (CAPI-32).
//!
//! Every operation gets the five required classes: positive, negative,
//! authorization, version (stale revision / content addressing), failure
//! (cross-scope / not-found). The fake parser port proves the
//! `implements` / `references` semantics without depending on a grammar.

#![cfg(feature = "parser-rust")]

use std::sync::Arc;

use code_intelligence::domain::{
    AnalyzeRepository, Direction, ExportGraphQuery, FindImplementorsQuery, FindImportersQuery,
    GetDetailedSymbolQuery, ImportMatchMode, IndexDirectory, PageRequest, RepoRevision, SourceFile,
    TraverseQuery,
};
use code_intelligence::parsers::rust::RustParser;
use code_intelligence::service::{CodeIntelligenceService, InMemorySources};
use code_intelligence::{
    CodeEdge, CodeEdgeKind, CodeError, CodeLocation, CodeResult, EdgeResolution, ParsedSource,
    ParserPort, Resolution, SupportLevel, Symbol, SymbolKind,
};
use ukma_contracts::{Consistency, RequestContext, ScopeRef};

const REPO: &str = "repo-1";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
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

fn service() -> CodeIntelligenceService {
    CodeIntelligenceService::new(
        Arc::new(RustParser::new()),
        Arc::new(
            InMemorySources::new()
                .with_commit(COMMIT_A, vec![SourceFile::new("src/lib.rs", LIB_RS)]),
        ),
    )
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
    base_ctx("tenant-1", &["code:read", "code:index", "code:index_admin"])
}

fn analyze(svc: &CodeIntelligenceService, ctx: &RequestContext, commit: &str) {
    svc.analyze_repository(
        ctx,
        &AnalyzeRepository {
            revision: RepoRevision::git(REPO, commit),
            parser_profile: PROFILE.to_string(),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            asynchronous: true,
        },
    )
    .expect("analysis must be accepted");
}

/// Find a symbol id by qualified-name suffix (deterministic lookup).
fn symbol_id(
    svc: &CodeIntelligenceService,
    ctx: &RequestContext,
    commit: &str,
    name: &str,
) -> String {
    let page = svc
        .find_symbols(
            ctx,
            &code_intelligence::domain::SymbolQuery {
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
// CAPI-28 code.find_implementors
// ---------------------------------------------------------------------------

/// A parser port that fabricates an `implements` edge, a class-like
/// `references` fallback edge and a non-class `references` edge that must be
/// excluded.
#[derive(Debug, Default)]
struct FakeTraitParser;

fn symbol(id: &str, name: &str, qualified: &str, kind: SymbolKind) -> Symbol {
    Symbol {
        id: id.to_string(),
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        kind,
        definition: CodeLocation {
            file_id: "file:src/lib.rs".to_string(),
            revision: COMMIT_A.to_string(),
            line_start: 1,
            line_end: 2,
            qualified_name: Some(qualified.to_string()),
        },
        language: "rust".to_string(),
        resolution: Resolution::Resolved,
        signature: None,
        visibility: None,
        evidence_ids: vec![format!("ev-{id}")],
    }
}

impl ParserPort for FakeTraitParser {
    fn support(&self, language: &str) -> SupportLevel {
        if language == "rust" {
            SupportLevel::Ported
        } else {
            SupportLevel::NotPorted
        }
    }

    fn parse(&self, path: &str, language: &str, _source: &str) -> CodeResult<ParsedSource> {
        if path != "src/lib.rs" {
            return Ok(ParsedSource {
                language: language.to_string(),
                content_hash: "sha256:00".to_string(),
                parser_version: "fake-1".to_string(),
                symbols: Vec::new(),
                edges: Vec::new(),
                diagnostics: Vec::new(),
            });
        }
        let trait_symbol = symbol("p-trait", "Greeter", "crate::Greeter", SymbolKind::Trait);
        let dog = symbol("p-dog", "Dog", "crate::Dog", SymbolKind::Class);
        let cat = symbol("p-cat", "Cat", "crate::Cat", SymbolKind::Class);
        let bark = symbol("p-bark", "bark", "crate::bark", SymbolKind::Function);
        Ok(ParsedSource {
            language: language.to_string(),
            content_hash: "sha256:01".to_string(),
            parser_version: "fake-1".to_string(),
            symbols: vec![trait_symbol, dog, cat, bark],
            edges: vec![
                CodeEdge {
                    source: "p-dog".to_string(),
                    target: Some("p-trait".to_string()),
                    kind: CodeEdgeKind::Implements,
                    resolution: EdgeResolution::StaticExact,
                    confidence: None,
                    evidence_ids: vec!["ev-implements".to_string()],
                    import_path: None,
                },
                CodeEdge {
                    source: "p-cat".to_string(),
                    target: Some("p-trait".to_string()),
                    kind: CodeEdgeKind::References,
                    resolution: EdgeResolution::StaticPossible,
                    confidence: None,
                    evidence_ids: vec!["ev-references".to_string()],
                    import_path: None,
                },
                CodeEdge {
                    source: "p-bark".to_string(),
                    target: Some("p-trait".to_string()),
                    kind: CodeEdgeKind::References,
                    resolution: EdgeResolution::StaticPossible,
                    confidence: None,
                    evidence_ids: vec!["ev-bark".to_string()],
                    import_path: None,
                },
            ],
            diagnostics: Vec::new(),
        })
    }
}

fn trait_service() -> CodeIntelligenceService {
    CodeIntelligenceService::new(
        Arc::new(FakeTraitParser),
        Arc::new(InMemorySources::new().with_commit(
            COMMIT_A,
            vec![SourceFile::new("src/lib.rs", "trait Greeter {}\n")],
        )),
    )
}

#[test]
fn capi14_find_implementors_reports_implements_and_class_references() {
    let svc = trait_service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let trait_id = symbol_id(&svc, &ctx, COMMIT_A, "Greeter");
    let page = svc
        .find_implementors(
            &ctx,
            &FindImplementorsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_symbol_id: trait_id,
                page: PageRequest::new(100),
            },
        )
        .expect("positive path");
    let names: Vec<&str> = page.items.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"Dog"), "implements edge: {names:?}");
    assert!(
        names.contains(&"Cat"),
        "class-like references fallback: {names:?}"
    );
    assert!(
        !names.contains(&"bark"),
        "non-class references must not match: {names:?}"
    );
    assert!(
        page.items
            .iter()
            .any(|m| m.evidence.iter().any(|e| e.contains("ev-implements"))),
        "evidence must come from the edge"
    );
}

#[test]
fn capi14_find_implementors_negative_paths() {
    let svc = trait_service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let trait_id = symbol_id(&svc, &ctx, COMMIT_A, "Greeter");

    // Unknown target symbol.
    let err = svc
        .find_implementors(
            &ctx,
            &FindImplementorsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_symbol_id: "missing".to_string(),
                page: PageRequest::new(10),
            },
        )
        .expect_err("unknown target must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));

    // Invalid page limit.
    let err = svc
        .find_implementors(
            &ctx,
            &FindImplementorsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_symbol_id: trait_id.clone(),
                page: PageRequest::new(0),
            },
        )
        .expect_err("limit 0 must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));

    // Unknown revision.
    let err = svc
        .find_implementors(
            &ctx,
            &FindImplementorsQuery {
                repository_id: REPO.to_string(),
                revision: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(),
                target_symbol_id: trait_id,
                page: PageRequest::new(10),
            },
        )
        .expect_err("unknown revision must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

#[test]
fn capi14_find_implementors_requires_the_read_scope() {
    let svc = trait_service();
    let ctx = base_ctx("tenant-1", &[]);
    analyze(&svc, &full_ctx(), COMMIT_A);
    let err = svc
        .find_implementors(
            &ctx,
            &FindImplementorsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_symbol_id: "x".to_string(),
                page: PageRequest::new(10),
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, CodeError::Forbidden { .. }));
}

#[test]
fn capi14_find_implementors_cross_scope_is_not_found() {
    let svc = trait_service();
    analyze(&svc, &full_ctx(), COMMIT_A);
    let other = base_ctx("tenant-2", &["code:read"]);
    let err = svc
        .find_implementors(
            &other,
            &FindImplementorsQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                target_symbol_id: "x".to_string(),
                page: PageRequest::new(10),
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// CAPI-29 code.traverse
// ---------------------------------------------------------------------------

#[test]
fn capi15_traverse_positive_collects_bounded_edges() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let twice = symbol_id(&svc, &ctx, COMMIT_A, "twice");
    let result = svc
        .traverse_graph(
            &ctx,
            &TraverseQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                start_symbol_id: twice,
                direction: Direction::Out,
                edge_kinds: Vec::new(),
                max_depth: 2,
                max_nodes: 100,
                page: PageRequest::new(100),
            },
        )
        .expect("positive path");
    assert!(
        result
            .edges
            .items
            .iter()
            .any(|e| e.kind == CodeEdgeKind::Calls),
        "traversal must collect the calls edge: {:?}",
        result.edges.items
    );
    assert!(!result.truncated);
    assert!(result.visited_count >= 2);
}

#[test]
fn capi15_traverse_truncates_at_max_nodes_instead_of_failing() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let twice = symbol_id(&svc, &ctx, COMMIT_A, "twice");
    let result = svc
        .traverse_graph(
            &ctx,
            &TraverseQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                start_symbol_id: twice,
                direction: Direction::Out,
                edge_kinds: Vec::new(),
                max_depth: 8,
                max_nodes: 1,
                page: PageRequest::new(100),
            },
        )
        .expect("positive path");
    assert!(result.truncated, "max_nodes=1 must truncate");
    assert_eq!(result.visited_count, 1);
}

#[test]
fn capi15_traverse_negative_paths() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let twice = symbol_id(&svc, &ctx, COMMIT_A, "twice");

    // max_depth 0.
    let err = svc
        .traverse_graph(
            &ctx,
            &TraverseQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                start_symbol_id: twice.clone(),
                direction: Direction::Out,
                edge_kinds: Vec::new(),
                max_depth: 0,
                max_nodes: 10,
                page: PageRequest::new(10),
            },
        )
        .expect_err("depth 0 must fail");
    assert!(matches!(err, CodeError::ResourceLimit { .. }));

    // Unknown start symbol.
    let err = svc
        .traverse_graph(
            &ctx,
            &TraverseQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                start_symbol_id: "missing".to_string(),
                direction: Direction::Out,
                edge_kinds: Vec::new(),
                max_depth: 2,
                max_nodes: 10,
                page: PageRequest::new(10),
            },
        )
        .expect_err("unknown symbol must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

#[test]
fn capi15_traverse_requires_the_read_scope() {
    let svc = service();
    let ctx = base_ctx("tenant-1", &[]);
    analyze(&svc, &full_ctx(), COMMIT_A);
    let err = svc
        .traverse_graph(
            &ctx,
            &TraverseQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                start_symbol_id: "x".to_string(),
                direction: Direction::Out,
                edge_kinds: Vec::new(),
                max_depth: 2,
                max_nodes: 10,
                page: PageRequest::new(10),
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, CodeError::Forbidden { .. }));
}

#[test]
fn capi15_traverse_cross_scope_is_not_found() {
    let svc = service();
    analyze(&svc, &full_ctx(), COMMIT_A);
    let other = base_ctx("tenant-2", &["code:read"]);
    let err = svc
        .traverse_graph(
            &other,
            &TraverseQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                start_symbol_id: "x".to_string(),
                direction: Direction::Out,
                edge_kinds: Vec::new(),
                max_depth: 2,
                max_nodes: 10,
                page: PageRequest::new(10),
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// CAPI-30 code.get_detailed_symbol
// ---------------------------------------------------------------------------

#[test]
fn capi16_detailed_symbol_carries_complexity_and_calls() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let twice = symbol_id(&svc, &ctx, COMMIT_A, "twice");
    let detail = svc
        .get_detailed_symbol(
            &ctx,
            &GetDetailedSymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: twice,
            },
        )
        .expect("positive path");
    assert!(
        detail.complexity.is_some(),
        "a function must have complexity"
    );
    assert!(
        detail.callees.iter().any(|c| c.name == "add"),
        "callees must include add: {:?}",
        detail.callees
    );
    assert_eq!(detail.callee_count, detail.callees.len() as u32);
    assert_eq!(detail.caller_count, detail.callers.len() as u32);
}

#[test]
fn capi16_detailed_symbol_without_complexity_for_non_functions() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let core = symbol_id(&svc, &ctx, COMMIT_A, "core");
    let detail = svc
        .get_detailed_symbol(
            &ctx,
            &GetDetailedSymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: core,
            },
        )
        .expect("positive path");
    assert!(
        detail.complexity.is_none(),
        "a module must not have complexity"
    );
}

#[test]
fn capi16_detailed_symbol_negative_paths() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    let err = svc
        .get_detailed_symbol(
            &ctx,
            &GetDetailedSymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: "missing".to_string(),
            },
        )
        .expect_err("unknown symbol must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));

    let err = svc
        .get_detailed_symbol(
            &ctx,
            &GetDetailedSymbolQuery {
                repository_id: REPO.to_string(),
                revision: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(),
                symbol_id: "x".to_string(),
            },
        )
        .expect_err("unknown revision must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

#[test]
fn capi16_detailed_symbol_requires_the_read_scope() {
    let svc = service();
    let ctx = base_ctx("tenant-1", &[]);
    analyze(&svc, &full_ctx(), COMMIT_A);
    let err = svc
        .get_detailed_symbol(
            &ctx,
            &GetDetailedSymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: "x".to_string(),
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, CodeError::Forbidden { .. }));
}

#[test]
fn capi16_detailed_symbol_cross_scope_is_not_found() {
    let svc = service();
    analyze(&svc, &full_ctx(), COMMIT_A);
    let other = base_ctx("tenant-2", &["code:read"]);
    let err = svc
        .get_detailed_symbol(
            &other,
            &GetDetailedSymbolQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                symbol_id: "x".to_string(),
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// CAPI-31 code.index_directory
// ---------------------------------------------------------------------------

#[test]
fn capi17_index_directory_indexes_only_the_prefixed_files() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let receipt = svc
        .index_directory(
            &ctx,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                path_prefix: "src/".to_string(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect("positive path");
    assert_eq!(receipt.indexed_files, 1, "only src/lib.rs matches");
    assert_ne!(receipt.revision, COMMIT_A, "content-addressed id differs");
    assert!(receipt.dirty);
}

#[test]
fn capi17_index_directory_negative_paths() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    // Empty prefix.
    let err = svc
        .index_directory(
            &ctx,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                path_prefix: " ".to_string(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("empty prefix must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));

    // No files under the prefix.
    let err = svc
        .index_directory(
            &ctx,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                path_prefix: "docs/".to_string(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("no files under prefix must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));

    // Empty parser profile.
    let err = svc
        .index_directory(
            &ctx,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                path_prefix: "src/".to_string(),
                parser_profile: "".to_string(),
            },
        )
        .expect_err("empty profile must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));
}

#[test]
fn capi17_index_directory_requires_the_index_scope() {
    let svc = service();
    let ctx = base_ctx("tenant-1", &["code:read"]);
    analyze(&svc, &full_ctx(), COMMIT_A);
    let err = svc
        .index_directory(
            &ctx,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                path_prefix: "src/".to_string(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, CodeError::Forbidden { .. }));
}

#[test]
fn capi17_index_directory_failure_paths() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    // Unregistered base revision.
    let err = svc
        .index_directory(
            &ctx,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(),
                path_prefix: "src/".to_string(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("unknown base revision must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));

    // Cross-scope.
    let other = base_ctx("tenant-2", &["code:index"]);
    let err = svc
        .index_directory(
            &other,
            &IndexDirectory {
                repository_id: REPO.to_string(),
                base_revision: COMMIT_A.to_string(),
                path_prefix: "src/".to_string(),
                parser_profile: PROFILE.to_string(),
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// CAPI-32 code.export_graph
// ---------------------------------------------------------------------------

fn export(
    svc: &CodeIntelligenceService,
    ctx: &RequestContext,
    format: &str,
) -> code_intelligence::domain::GraphExport {
    svc.export_graph(
        ctx,
        &ExportGraphQuery {
            repository_id: REPO.to_string(),
            revision: COMMIT_A.to_string(),
            format: format.to_string(),
            edge_kinds: Vec::new(),
            max_nodes: 0,
        },
    )
    .expect("export must succeed")
}

#[test]
fn capi18_export_renders_all_four_formats_deterministically() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    for format in ["dot", "json", "csv", "triples"] {
        let first = export(&svc, &ctx, format);
        let second = export(&svc, &ctx, format);
        assert_eq!(
            first.content, second.content,
            "{format} must be deterministic"
        );
        assert!(first.node_count > 0, "{format} node count");
        assert!(!first.content.is_empty());
        assert!(!first.truncated);
    }
}

#[test]
fn capi18_export_truncates_to_max_nodes_and_drops_edges() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let full = export(&svc, &ctx, "dot");
    let capped = svc
        .export_graph(
            &ctx,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                format: "dot".to_string(),
                edge_kinds: Vec::new(),
                max_nodes: 1,
            },
        )
        .expect("capped export");
    assert!(capped.truncated);
    assert_eq!(capped.node_count, 1);
    assert_eq!(
        capped.edge_count, 0,
        "edges touching dropped nodes are dropped"
    );
    assert!(capped.node_count < full.node_count);
}

#[test]
fn capi18_export_negative_paths() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    // Unknown format.
    let err = svc
        .export_graph(
            &ctx,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                format: "parquet".to_string(),
                edge_kinds: Vec::new(),
                max_nodes: 0,
            },
        )
        .expect_err("unknown format must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));

    // max_nodes above the cap.
    let err = svc
        .export_graph(
            &ctx,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                format: "dot".to_string(),
                edge_kinds: Vec::new(),
                max_nodes: 20_000,
            },
        )
        .expect_err("max_nodes above the cap must fail");
    assert!(matches!(err, CodeError::ResourceLimit { .. }));

    // Unknown revision.
    let err = svc
        .export_graph(
            &ctx,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(),
                format: "dot".to_string(),
                edge_kinds: Vec::new(),
                max_nodes: 0,
            },
        )
        .expect_err("unknown revision must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

#[test]
fn capi18_export_requires_the_read_scope() {
    let svc = service();
    let ctx = base_ctx("tenant-1", &[]);
    analyze(&svc, &full_ctx(), COMMIT_A);
    let err = svc
        .export_graph(
            &ctx,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                format: "dot".to_string(),
                edge_kinds: Vec::new(),
                max_nodes: 0,
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, CodeError::Forbidden { .. }));
}

#[test]
fn capi18_export_cross_scope_is_not_found() {
    let svc = service();
    analyze(&svc, &full_ctx(), COMMIT_A);
    let other = base_ctx("tenant-2", &["code:read"]);
    let err = svc
        .export_graph(
            &other,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                format: "dot".to_string(),
                edge_kinds: Vec::new(),
                max_nodes: 0,
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

#[test]
fn capi18_export_filters_by_edge_kind() {
    let svc = service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);
    let calls_only = svc
        .export_graph(
            &ctx,
            &ExportGraphQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                format: "csv".to_string(),
                edge_kinds: vec![CodeEdgeKind::Calls],
                max_nodes: 0,
            },
        )
        .expect("filtered export");
    assert!(
        calls_only.content.contains("calls"),
        "only calls edges are rendered: {}",
        calls_only.content
    );
    assert!(!calls_only.content.contains("defines"), "defines excluded");
}

// ---------------------------------------------------------------------------
// CAPI-33 `code.find_importers`
// ---------------------------------------------------------------------------

const IMPORTER_RS: &str = "\
use std::collections::BTreeMap;
use std::fmt::Display;

pub struct Graph {
    pub nodes: BTreeMap<String, usize>,
}
";

fn importer_service() -> CodeIntelligenceService {
    CodeIntelligenceService::new(
        Arc::new(RustParser::new()),
        Arc::new(
            InMemorySources::new()
                .with_commit(COMMIT_A, vec![SourceFile::new("src/lib.rs", IMPORTER_RS)]),
        ),
    )
}

fn find_importers(
    svc: &CodeIntelligenceService,
    ctx: &RequestContext,
    module: &str,
    mode: ImportMatchMode,
    limit: u32,
) -> code_intelligence::domain::Page<code_intelligence::domain::ImporterMatch> {
    svc.find_importers(
        ctx,
        &FindImportersQuery {
            repository_id: REPO.to_string(),
            revision: COMMIT_A.to_string(),
            module_name: module.to_string(),
            match_mode: mode,
            page: PageRequest::new(limit),
        },
    )
    .expect("find_importers must succeed")
}

#[test]
fn capi19_find_importers_lists_files_importing_the_module() {
    let svc = importer_service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    // Default (contains) — the `std::collections` module is imported once.
    let page = find_importers(
        &svc,
        &ctx,
        "std::collections",
        ImportMatchMode::Contains,
        20,
    );
    assert_eq!(page.items.len(), 1, "one importing declaration");
    let item = &page.items[0];
    assert_eq!(item.importer_file, "src/lib.rs");
    assert_eq!(item.imported_module, "std::collections::BTreeMap");
    assert_eq!(item.line, 1);
    assert!(!item.evidence.is_empty(), "evidence must be present");

    // Prefix `std::` — both `use std::...` declarations match.
    let page = find_importers(&svc, &ctx, "std::", ImportMatchMode::Prefix, 20);
    assert_eq!(page.items.len(), 2, "both std:: imports match the prefix");

    // Exact `std::collections::BTreeMap` — the single fully-qualified name.
    let page = find_importers(
        &svc,
        &ctx,
        "std::collections::BTreeMap",
        ImportMatchMode::Exact,
        20,
    );
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].imported_module, "std::collections::BTreeMap");

    // Exact `std::collections` — no declaration is exactly that.
    let page = find_importers(&svc, &ctx, "std::collections", ImportMatchMode::Exact, 20);
    assert_eq!(page.items.len(), 0, "exact is full-string equality");

    // Contains is case-sensitive: `btreemap` does not match.
    let page = find_importers(&svc, &ctx, "btreemap", ImportMatchMode::Contains, 20);
    assert_eq!(page.items.len(), 0, "contains is case-sensitive");

    // Fuzzy is case-insensitive: `btreemap` matches `BTreeMap`.
    let page = find_importers(&svc, &ctx, "btreemap", ImportMatchMode::Fuzzy, 20);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].imported_module, "std::collections::BTreeMap");

    // An unknown module is a valid empty page, not an error (parity).
    let page = find_importers(&svc, &ctx, "nope::nope", ImportMatchMode::Exact, 20);
    assert_eq!(page.items.len(), 0, "unknown module is an empty page");
}

#[test]
fn capi19_find_importers_keeps_edges_parity_safe() {
    let parser = RustParser::new();
    let parsed = parser.parse("src/lib.rs", "rust", IMPORTER_RS).unwrap();
    let imports = parsed
        .edges
        .iter()
        .filter(|edge| edge.kind == CodeEdgeKind::Imports)
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 2, "two import declarations");
    for edge in &imports {
        assert!(
            edge.target.is_none(),
            "imports edges keep `target: None` (parity gate)"
        );
        assert_eq!(
            edge.resolution,
            EdgeResolution::Unresolved,
            "imports edges keep `Unresolved` (parity gate)"
        );
        assert!(
            edge.import_path.is_some(),
            "import_path is carried on the edge"
        );
    }
}

#[test]
fn capi19_find_importers_negative_paths() {
    let svc = importer_service();
    let ctx = full_ctx();
    analyze(&svc, &ctx, COMMIT_A);

    // Empty module name.
    let err = svc
        .find_importers(
            &ctx,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                module_name: "   ".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(10),
            },
        )
        .expect_err("empty module name must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));

    // limit 0.
    let err = svc
        .find_importers(
            &ctx,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                module_name: "std".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(0),
            },
        )
        .expect_err("limit 0 must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));

    // limit above the cap.
    let err = svc
        .find_importers(
            &ctx,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                module_name: "std".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(201),
            },
        )
        .expect_err("limit 201 must fail");
    assert!(matches!(err, CodeError::InvalidArgument { .. }));

    // Unknown revision.
    let err = svc
        .find_importers(
            &ctx,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string(),
                module_name: "std".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(10),
            },
        )
        .expect_err("unknown revision must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}

#[test]
fn capi19_find_importers_requires_the_read_scope() {
    let svc = importer_service();
    let ctx = base_ctx("tenant-1", &[]);
    analyze(&svc, &full_ctx(), COMMIT_A);
    let err = svc
        .find_importers(
            &ctx,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                module_name: "std".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(10),
            },
        )
        .expect_err("missing scope must fail");
    assert!(matches!(err, CodeError::Forbidden { .. }));
}

#[test]
fn capi19_find_importers_enforces_required_generation() {
    let svc = importer_service();
    let ctx = full_ctx();
    let handle = svc
        .analyze_repository(
            &ctx,
            &AnalyzeRepository {
                revision: RepoRevision::git(REPO, COMMIT_A),
                parser_profile: PROFILE.to_string(),
                include_globs: Vec::new(),
                exclude_globs: Vec::new(),
                asynchronous: true,
            },
        )
        .expect("analysis must be accepted");
    svc.analysis_manifest(&ctx, &handle.job_id)
        .expect("manifest must be readable");

    let mut strict = ctx.clone();
    strict.consistency = ukma_contracts::Consistency::StrictGeneration;
    strict.required_generation = Some("gen-does-not-exist".to_string());
    let err = svc
        .find_importers(
            &strict,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                module_name: "std".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(10),
            },
        )
        .expect_err("stale generation must fail");
    assert_eq!(err.code(), "INDEX_STALE");
}

#[test]
fn capi19_find_importers_cross_scope_is_not_found() {
    let svc = importer_service();
    analyze(&svc, &full_ctx(), COMMIT_A);
    let other = base_ctx("tenant-2", &["code:read"]);
    let err = svc
        .find_importers(
            &other,
            &FindImportersQuery {
                repository_id: REPO.to_string(),
                revision: COMMIT_A.to_string(),
                module_name: "std".to_string(),
                match_mode: ImportMatchMode::Contains,
                page: PageRequest::new(10),
            },
        )
        .expect_err("cross-scope must fail");
    assert!(matches!(err, CodeError::NotFound { .. }));
}
