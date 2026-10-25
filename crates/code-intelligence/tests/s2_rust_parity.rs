//! `S2-rust` golden parity test (`CODE-001`).
//!
//! The expectation below was written by hand from `sample.rs`, not generated
//! from the implementation, so it is a real check: symbol identity, kind, spans
//! and edge kind/resolution must match exactly.
//!
//! Parity scope (see `docs/migration/s2-parity-report.yaml`): symbol identity +
//! 1-based spans + containment/import/call edges over the same tree-sitter
//! grammar version as the locked upstream baseline. This is **not** a claim of
//! cross-file name resolution — call edges are never `static_exact`.

use std::path::PathBuf;

use code_intelligence::parsers::rust::RustParser;
use code_intelligence::{
    CodeEdgeKind, CodeError, EdgeResolution, ParsedSource, ParserPort, SupportLevel, Symbol,
    SymbolKind,
};

fn parse_sample() -> ParsedSource {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/s2-rust/sample.rs"),
    )
    .expect("sample.rs fixture");
    RustParser::new()
        .parse("src/sample.rs", "rust", &source)
        .expect("rust source must parse")
}

fn qualified(parsed: &ParsedSource, id: &str) -> String {
    parsed
        .symbols
        .iter()
        .find(|symbol| symbol.id == id)
        .map(|symbol| symbol.qualified_name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// `"name Kind start-end"`, sorted. The file module is excluded because its
/// span is the whole file.
fn symbol_rows(parsed: &ParsedSource) -> Vec<String> {
    let mut rows: Vec<String> = parsed
        .symbols
        .iter()
        .filter(|symbol| !(symbol.kind == SymbolKind::Module && symbol.qualified_name == "sample"))
        .map(|symbol| {
            format!(
                "{} {:?} {}-{}",
                symbol.qualified_name,
                symbol.kind,
                symbol.definition.line_start,
                symbol.definition.line_end
            )
        })
        .collect();
    rows.sort();
    rows
}

/// `"source Kind Resolution target"`, sorted.
fn edge_rows(parsed: &ParsedSource) -> Vec<String> {
    let mut rows: Vec<String> = parsed
        .edges
        .iter()
        .map(|edge| {
            format!(
                "{} {:?} {:?} {}",
                qualified(parsed, &edge.source),
                edge.kind,
                edge.resolution,
                edge.target
                    .as_deref()
                    .map(|target| qualified(parsed, target))
                    .unwrap_or_else(|| "-".to_string())
            )
        })
        .collect();
    rows.sort();
    rows
}

fn find<'a>(parsed: &'a ParsedSource, name: &str) -> &'a Symbol {
    parsed
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == name)
        .unwrap_or_else(|| panic!("`{name}` was not extracted"))
}

#[test]
fn symbols_and_spans_match_the_hand_written_expectation() {
    let parsed = parse_sample();
    // Qualified names are module-scoped (`<file module>::<item>`), so two files
    // that both declare `Graph` cannot collide. See ADR-031a.
    let expected = vec![
        "sample::Graph Struct 5-7",
        "sample::Graph::add Method 25-27",
        "sample::Graph::check Method 29-31",
        "sample::Graph::new Method 19-23",
        "sample::Kind Enum 9-12",
        "sample::LIMIT Constant 3-3",
        "sample::Visitor Trait 14-16",
        "sample::Visitor::visit Method 15-15",
        "sample::compute Function 40-42",
        "sample::inner Module 34-38",
        "sample::inner::helper Function 35-37",
    ];
    assert_eq!(symbol_rows(&parsed), expected);
    assert_eq!(find(&parsed, "sample").kind, SymbolKind::Module);
}

#[test]
fn spans_are_one_based_and_satisfy_the_location_invariants() {
    let parsed = parse_sample();
    for symbol in &parsed.symbols {
        assert!(symbol.definition.line_start >= 1, "spans are 1-based");
        assert!(symbol.definition.line_end >= symbol.definition.line_start);
        symbol
            .definition
            .validate()
            .expect("span invariants from §32.1 hold");
    }
}

#[test]
fn containment_is_exact_while_imports_stay_unresolved() {
    let parsed = parse_sample();
    let rows = edge_rows(&parsed);

    for child in [
        "sample::LIMIT",
        "sample::Graph",
        "sample::Kind",
        "sample::Visitor",
        "sample::inner",
        "sample::compute",
    ] {
        assert!(
            rows.contains(&format!("sample Defines StaticExact {child}")),
            "sample must define {child}"
        );
    }
    for child in [
        "sample::Graph::new",
        "sample::Graph::add",
        "sample::Graph::check",
    ] {
        assert!(
            rows.contains(&format!("sample::Graph Defines StaticExact {child}")),
            "sample::Graph must define {child}"
        );
    }
    assert!(
        rows.contains(&"sample::Visitor Defines StaticExact sample::Visitor::visit".to_string())
    );
    assert!(rows.contains(&"sample::inner Defines StaticExact sample::inner::helper".to_string()));

    let imports: Vec<&String> = rows
        .iter()
        .filter(|row| row.split(' ').nth(1) == Some("Imports"))
        .collect();
    assert_eq!(imports.len(), 1, "exactly one `use` declaration");
    assert_eq!(imports[0], "sample Imports Unresolved -");
}

#[test]
fn call_edges_are_never_claimed_exact() {
    let parsed = parse_sample();
    let rows = edge_rows(&parsed);
    let calls: Vec<&String> = rows
        .iter()
        .filter(|row| row.split(' ').nth(1) == Some("Calls"))
        .collect();
    assert!(!calls.is_empty(), "the fixture contains calls");
    for call in &calls {
        assert!(
            !call.contains("StaticExact"),
            "no name resolution is performed, so a call cannot be static_exact: {call}"
        );
    }
    assert!(
        rows.contains(&"sample::Graph::add Calls StaticPossible sample::Graph::check".to_string())
    );
    assert!(
        rows.contains(&"sample::inner::helper Calls StaticPossible sample::compute".to_string())
    );
    assert!(rows.contains(&"sample::Graph::check Calls Unresolved -".to_string()));
    assert!(
        !rows.contains(&"sample::Graph::new Calls StaticPossible sample::Graph::new".to_string())
    );
}

#[test]
fn only_rust_is_declared_ported() {
    let parser = RustParser::new();
    assert_eq!(parser.support("rust"), SupportLevel::Ported);
    assert_eq!(parser.support("python"), SupportLevel::NotPorted);
    assert_eq!(
        parser
            .parse("a.py", "python", "def f(): pass")
            .unwrap_err()
            .code(),
        "CAPABILITY_UNAVAILABLE"
    );
}

#[test]
fn invalid_arguments_are_typed_not_silent() {
    let parser = RustParser::new();
    assert_eq!(
        parser.parse("   ", "rust", "fn f() {}").unwrap_err().code(),
        "INVALID_ARGUMENT"
    );
    match parser.parse("weird", "cobol", "IDENTIFICATION DIVISION.") {
        Err(CodeError::CapabilityUnavailable { language }) => assert_eq!(language, "cobol"),
        other => panic!("expected CAPABILITY_UNAVAILABLE, got {other:?}"),
    }
}

#[test]
fn unparsable_source_yields_diagnostics_not_an_empty_success() {
    let parsed = RustParser::new()
        .parse("src/broken.rs", "rust", "fn f( {")
        .expect("tree-sitter always produces a tree");
    assert!(
        !parsed.diagnostics.is_empty(),
        "a source with syntax errors must report diagnostics (§7.1)"
    );
    assert_eq!(parsed.diagnostics[0].code, "PARSE_ERROR");
    assert!(!parsed.content_hash.is_empty());
}

#[test]
fn parsing_is_deterministic() {
    let first = parse_sample();
    let second = parse_sample();
    assert_eq!(symbol_rows(&first), symbol_rows(&second));
    assert_eq!(edge_rows(&first), edge_rows(&second));
    assert_eq!(first.content_hash, second.content_hash);
    assert_eq!(first.parser_version, "tree-sitter-rust-0.24.2/ts-0.25.10");
}

#[test]
fn manifest_reports_the_pinned_parser_profile() {
    let parsed = parse_sample();
    let manifest = RustParser::new().manifest("repo:demo", &"0".repeat(40), "gen-1", &parsed);
    assert_eq!(
        manifest.parser_profile,
        "tree-sitter-rust-0.24.2/ts-0.25.10"
    );
    assert_eq!(manifest.completeness(), "complete");
    assert_eq!(manifest.indexed_files, 1);
    assert_eq!(manifest.skipped_files, 0);
}

#[test]
fn symbol_kinds_cover_the_declaration_forms_the_fixture_exercises() {
    let parsed = parse_sample();
    let kinds: Vec<SymbolKind> = {
        let mut kinds: Vec<SymbolKind> = parsed.symbols.iter().map(|s| s.kind).collect();
        kinds.sort_by_key(|kind| format!("{kind:?}"));
        kinds.dedup();
        kinds
    };
    for expected in [
        SymbolKind::Module,
        SymbolKind::Struct,
        SymbolKind::Enum,
        SymbolKind::Trait,
        SymbolKind::Function,
        SymbolKind::Method,
        SymbolKind::Constant,
    ] {
        assert!(kinds.contains(&expected), "expected a {expected:?} symbol");
    }
    // an `impl` block is not itself a symbol; only its methods are
    assert!(!parsed
        .symbols
        .iter()
        .any(|symbol| symbol.name.starts_with("impl ")));
    let edge_kinds: Vec<CodeEdgeKind> = parsed.edges.iter().map(|edge| edge.kind).collect();
    assert!(edge_kinds.contains(&CodeEdgeKind::Defines));
    assert!(edge_kinds.contains(&CodeEdgeKind::Imports));
    assert!(edge_kinds.contains(&CodeEdgeKind::Calls));
    assert!(parsed
        .edges
        .iter()
        .all(|edge| edge.resolution != EdgeResolution::StaticExact
            || edge.kind == CodeEdgeKind::Defines));
}
