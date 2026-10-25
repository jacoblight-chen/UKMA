//! S2 multi-language golden parity (`CODE-001`).
//!
//! One table, one extractor: every language is checked with hand-written
//! expectations over its fixture in `tests/golden/s2-<language>/`. The properties
//! asserted here are identical for every language, which is the point — a
//! language may not quietly behave differently from the others.
//!
//! Parity scope (recorded per language in `docs/migration/s2-parity-report.yaml`):
//! symbol identity + 1-based spans + containment/import/call edge kinds and
//! resolutions. Cross-file resolution, macro expansion and type inference are out
//! of scope, so call edges are never `static_exact`.

use std::path::PathBuf;

use code_intelligence::parsers::engine::UnifiedParser;
use code_intelligence::{
    CodeEdgeKind, EdgeResolution, ParsedSource, ParserPort, SupportLevel, Symbol, SymbolKind,
};

struct Case {
    language: &'static str,
    file: &'static str,
    /// `(qualified_name, kind, line_start, line_end)`, excluding the file module.
    symbols: &'static [(&'static str, SymbolKind, u32, u32)],
    /// Expected number of import edges (unresolved by definition).
    imports: usize,
    /// Expected call edges that resolve to a same-file symbol.
    resolved_calls: usize,
    /// Substring that must appear in at least one symbol signature.
    signature_hint: &'static str,
}

const CASES: &[Case] = &[
    Case {
        language: "python",
        file: "s2-python/sample.py",
        symbols: &[
            ("sample::Graph", SymbolKind::Class, 4, 9),
            ("sample::Graph::add", SymbolKind::Method, 5, 6),
            ("sample::Graph::check", SymbolKind::Method, 8, 9),
            ("sample::compute", SymbolKind::Function, 12, 13),
        ],
        imports: 1,
        resolved_calls: 1,
        signature_hint: "def add",
    },
    Case {
        language: "go",
        file: "s2-go/sample.go",
        symbols: &[
            ("sample::Graph", SymbolKind::Struct, 5, 7),
            ("sample::Add", SymbolKind::Method, 9, 11),
            ("sample::Check", SymbolKind::Method, 13, 15),
            ("sample::compute", SymbolKind::Function, 17, 19),
        ],
        imports: 1,
        resolved_calls: 1,
        signature_hint: "func (g Graph) Add",
    },
    Case {
        language: "java",
        file: "s2-java/sample.java",
        symbols: &[
            ("sample::Graph", SymbolKind::Class, 6, 16),
            ("sample::Graph::add", SymbolKind::Method, 9, 11),
            ("sample::Graph::check", SymbolKind::Method, 13, 15),
            ("sample::Helper", SymbolKind::Class, 18, 22),
            ("sample::Helper::compute", SymbolKind::Method, 19, 21),
        ],
        imports: 1,
        resolved_calls: 1,
        signature_hint: "public boolean add",
    },
    Case {
        language: "bash",
        file: "s2-bash/sample.sh",
        symbols: &[
            ("sample::greet", SymbolKind::Function, 3, 5),
            ("sample::main", SymbolKind::Function, 7, 9),
        ],
        imports: 0,
        resolved_calls: 1,
        signature_hint: "greet()",
    },
    Case {
        language: "c",
        file: "s2-c/sample.c",
        symbols: &[
            ("sample::Graph", SymbolKind::Struct, 3, 5),
            ("sample::compute", SymbolKind::Function, 7, 9),
            ("sample::main", SymbolKind::Function, 11, 13),
        ],
        imports: 1,
        resolved_calls: 1,
        signature_hint: "int compute",
    },
    Case {
        language: "cpp",
        file: "s2-cpp/sample.cpp",
        symbols: &[
            ("sample::demo", SymbolKind::Module, 3, 20),
            ("sample::demo::Graph", SymbolKind::Class, 5, 14),
            ("sample::demo::Graph::add", SymbolKind::Method, 7, 9),
            ("sample::demo::Graph::check", SymbolKind::Method, 11, 13),
            ("sample::demo::compute", SymbolKind::Function, 16, 18),
        ],
        imports: 1,
        resolved_calls: 1,
        signature_hint: "int add",
    },
];

fn fixture(case: &Case) -> ParsedSource {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(case.file);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let relative = case.file.replace("s2-", "src/");
    UnifiedParser::new()
        .parse(&relative, case.language, &source)
        .unwrap_or_else(|error| panic!("{}: parse failed: {error}", case.language))
}

fn qualified(parsed: &ParsedSource, id: &str) -> String {
    parsed
        .symbols
        .iter()
        .find(|symbol| symbol.id == id)
        .map(|symbol| symbol.qualified_name.clone())
        .unwrap_or_else(|| id.to_string())
}

fn rows(parsed: &ParsedSource) -> Vec<String> {
    let module = parsed
        .symbols
        .first()
        .map(|symbol| symbol.qualified_name.clone())
        .unwrap_or_default();
    let mut rows: Vec<String> = parsed
        .symbols
        .iter()
        .filter(|symbol| symbol.kind != SymbolKind::Module || symbol.qualified_name != module)
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

#[test]
fn every_language_extracts_the_expected_symbols_and_spans() {
    for case in CASES {
        let parsed = fixture(case);
        let mut expected: Vec<String> = case
            .symbols
            .iter()
            .map(|(name, kind, start, end)| format!("{name} {kind:?} {start}-{end}"))
            .collect();
        expected.sort();
        assert_eq!(
            rows(&parsed),
            expected,
            "{}: symbol/span mismatch (actual: {:#?})",
            case.language,
            rows(&parsed)
        );
        for symbol in &parsed.symbols {
            symbol
                .definition
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error}", case.language));
            assert_eq!(symbol.language, case.language);
            assert_eq!(symbol.resolution, code_intelligence::Resolution::Resolved);
        }
    }
}

#[test]
fn containment_and_imports_follow_the_shared_rules() {
    for case in CASES {
        let parsed = fixture(case);
        for edge in &parsed.edges {
            match edge.kind {
                CodeEdgeKind::Defines => {
                    assert_eq!(
                        edge.resolution,
                        EdgeResolution::StaticExact,
                        "{}: containment is proven by the CST",
                        case.language
                    );
                    assert!(edge.target.is_some());
                }
                CodeEdgeKind::Imports => {
                    assert_eq!(
                        edge.resolution,
                        EdgeResolution::Unresolved,
                        "{}: imports are never resolved",
                        case.language
                    );
                    assert!(edge.target.is_none());
                }
                CodeEdgeKind::Calls => assert_ne!(
                    edge.resolution,
                    EdgeResolution::StaticExact,
                    "{}: a call can never be static_exact",
                    case.language
                ),
                _ => {}
            }
        }
        let imports = parsed
            .edges
            .iter()
            .filter(|edge| edge.kind == CodeEdgeKind::Imports)
            .count();
        assert_eq!(imports, case.imports, "{}: import edges", case.language);

        let resolved_calls = parsed
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == CodeEdgeKind::Calls
                    && edge.resolution == EdgeResolution::StaticPossible
            })
            .count();
        assert_eq!(
            resolved_calls, case.resolved_calls,
            "{}: same-file call edges (edges: {:#?})",
            case.language, parsed.edges
        );
    }
}

#[test]
fn signatures_are_captured_for_functions_and_methods() {
    for case in CASES {
        let parsed = fixture(case);
        let signature = parsed
            .symbols
            .iter()
            .filter_map(|symbol| symbol.signature.as_deref())
            .find(|signature| signature.contains(case.signature_hint))
            .or_else(|| {
                parsed
                    .symbols
                    .iter()
                    .filter(|symbol| {
                        matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                    })
                    .find_map(|symbol| symbol.signature.as_deref())
            });
        let signature = signature
            .unwrap_or_else(|| panic!("{}: no function signature captured", case.language));
        assert!(
            signature.contains(case.signature_hint),
            "{}: expected signature containing `{}`, got `{signature}`",
            case.language,
            case.signature_hint
        );
    }
}

#[test]
fn every_case_calls_back_into_its_own_file_where_expected() {
    for case in CASES {
        let parsed = fixture(case);
        let call_targets: Vec<String> = parsed
            .edges
            .iter()
            .filter(|edge| edge.kind == CodeEdgeKind::Calls)
            .filter_map(|edge| edge.target.as_deref().map(|id| qualified(&parsed, id)))
            .collect();
        if case.resolved_calls > 0 {
            let declared: Vec<&str> = case.symbols.iter().map(|(name, _, _, _)| *name).collect();
            assert!(
                call_targets
                    .iter()
                    .any(|target| declared.iter().any(|name| name == target)),
                "{}: expected a resolved call to a declared symbol, got {call_targets:?}",
                case.language
            );
        }
        assert!(
            call_targets.iter().all(|target| !target.is_empty()),
            "{}: resolved calls must name a target",
            case.language
        );
    }
}

#[test]
fn broken_source_reports_diagnostics_for_every_language() {
    for case in CASES {
        let parsed = UnifiedParser::new()
            .parse("src/broken", case.language, "{{{{ ))) not source")
            .unwrap_or_else(|error| panic!("{}: {error}", case.language));
        assert!(
            !parsed.diagnostics.is_empty(),
            "{}: unparsable input must produce diagnostics (§7.1)",
            case.language
        );
        assert_eq!(parsed.diagnostics[0].code, "PARSE_ERROR");
    }
}

#[test]
fn unported_languages_stay_unavailable() {
    let parser = UnifiedParser::new();
    for language in ["cobol", "perl", "zig", "not-a-language"] {
        assert_eq!(parser.support(language), SupportLevel::NotPorted);
        assert_eq!(
            parser.parse("a.x", language, "nothing").unwrap_err().code(),
            "CAPABILITY_UNAVAILABLE"
        );
    }
}

#[test]
fn every_case_is_deterministic() {
    for case in CASES {
        let first = fixture(case);
        let second = fixture(case);
        assert_eq!(rows(&first), rows(&second), "{}", case.language);
        assert_eq!(first.content_hash, second.content_hash, "{}", case.language);
        assert_eq!(first.parser_version, second.parser_version);
        assert!(first.parser_version.starts_with("tree-sitter-"));
    }
}

#[test]
fn file_module_symbol_is_named_after_the_file() {
    for case in CASES {
        let parsed = fixture(case);
        let module: &Symbol = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.kind == SymbolKind::Module && symbol.definition.line_start == 1)
            .unwrap_or_else(|| panic!("{}: missing file module", case.language));
        assert!(case.file.contains(&module.name), "{}", case.language);
    }
}
