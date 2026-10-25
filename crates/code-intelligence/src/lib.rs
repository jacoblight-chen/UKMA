//! Code Intelligence domain — canonical types (§32.1) and the parser port.
//!
//! **Status:** types + port + language registry only. No grammar is ported yet,
//! so every language reports [`SupportLevel::NotPorted`] and parsing returns a
//! typed `CAPABILITY_UNAVAILABLE` instead of an empty result (the spec forbids
//! swallowing a parse failure into an empty document, §7.1).
//!
//! Honesty rules encoded here:
//! - a dynamic call is never upgraded to `static_exact` (`CODE-001`/`CODE-002`);
//! - completeness is `partial` whenever anything was skipped or unresolved;
//! - the registry lists exactly the languages the locked upstream baseline has
//!   parsers for (ADR-048 O-01).

pub mod domain;
pub mod parsers;
pub mod service;

use serde::{Deserialize, Serialize};

/// Why a code operation could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    /// No parser supports this language yet.
    CapabilityUnavailable {
        /// Language identifier.
        language: String,
    },
    /// The source could not be parsed; diagnostics carry the details.
    SourceParseFailed {
        /// Safe message.
        message: String,
    },
    /// Malformed argument.
    InvalidArgument {
        /// Offending field.
        field: &'static str,
        /// Why it is invalid.
        reason: String,
    },
    /// The resource does not exist in the bound revision.
    NotFound {
        /// Human readable resource.
        resource: String,
    },
    /// The requested revision index is stale (superseded or invalidated).
    IndexStale {
        /// Revision that is no longer fresh.
        revision: String,
    },
    /// The revision index is not published yet.
    IndexNotReady {
        /// Revision that is still being built.
        revision: String,
    },
    /// Unknown job id.
    JobNotFound {
        /// Job id.
        job_id: String,
    },
    /// Cancellation arrived after the commit point (§40.4).
    JobNotCancellable {
        /// Job id.
        job_id: String,
    },
    /// The page cursor is no longer valid.
    CursorExpired {
        /// Offending cursor.
        cursor: String,
    },
    /// A traversal or impact analysis hit a hard resource limit.
    ResourceLimit {
        /// Safe detail.
        detail: String,
    },
    /// The request names a different tenant/project than the resource.
    CrossScopeDenied {
        /// Scope the request asked for.
        requested: String,
        /// Scope that owns the resource.
        actual: String,
    },
    /// The caller does not hold a required scope.
    Forbidden {
        /// Missing scope.
        required_scope: String,
    },
    /// Optimistic concurrency mismatch.
    EtagMismatch {
        /// Etag the caller expected.
        expected: String,
        /// Current etag.
        actual: String,
    },
    /// The revision's source is not available.
    SourceUnavailable {
        /// Revision.
        revision: String,
    },
    /// The requested and actual source revisions differ.
    SourceRevisionMismatch {
        /// Requested revision.
        requested: String,
        /// Revision the index was built from.
        actual: String,
    },
    /// The edit-context budget cannot hold the request.
    ContextBudgetExceeded {
        /// Tokens required.
        required: u32,
        /// Tokens available.
        budget: u32,
    },
    /// An idempotency key was reused with different arguments.
    IdempotencyConflict {
        /// The reused key.
        key: String,
    },
}

impl CodeError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            CodeError::CapabilityUnavailable { .. } => "CAPABILITY_UNAVAILABLE",
            CodeError::SourceParseFailed { .. } => "SOURCE_PARSE_FAILED",
            CodeError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            CodeError::NotFound { .. } => "NOT_FOUND",
            CodeError::IndexStale { .. } => "INDEX_STALE",
            CodeError::IndexNotReady { .. } => "INDEX_NOT_READY",
            CodeError::JobNotFound { .. } => "JOB_NOT_FOUND",
            CodeError::JobNotCancellable { .. } => "JOB_NOT_CANCELLABLE",
            CodeError::CursorExpired { .. } => "CURSOR_EXPIRED",
            CodeError::ResourceLimit { .. } => "RESOURCE_LIMIT",
            CodeError::CrossScopeDenied { .. } => "CROSS_SCOPE_DENIED",
            CodeError::Forbidden { .. } => "FORBIDDEN",
            CodeError::EtagMismatch { .. } => "ETAG_MISMATCH",
            CodeError::SourceUnavailable { .. } => "SOURCE_UNAVAILABLE",
            CodeError::SourceRevisionMismatch { .. } => "SOURCE_REVISION_MISMATCH",
            CodeError::ContextBudgetExceeded { .. } => "CONTEXT_BUDGET_EXCEEDED",
            CodeError::IdempotencyConflict { .. } => "IDEMPOTENCY_CONFLICT",
        }
    }
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeError::CapabilityUnavailable { language } => {
                write!(f, "no parser ported for language `{language}`")
            }
            CodeError::SourceParseFailed { message } => write!(f, "parse failed: {message}"),
            CodeError::InvalidArgument { field, reason } => write!(f, "{field}: {reason}"),
            CodeError::NotFound { resource } => write!(f, "{resource} not found"),
            CodeError::IndexStale { revision } => write!(f, "index for {revision} is stale"),
            CodeError::IndexNotReady { revision } => write!(f, "index for {revision} is not ready"),
            CodeError::JobNotFound { job_id } => write!(f, "job {job_id} not found"),
            CodeError::JobNotCancellable { job_id } => {
                write!(f, "job {job_id} cannot be cancelled after the commit point")
            }
            CodeError::CursorExpired { cursor } => write!(f, "cursor {cursor} expired"),
            CodeError::ResourceLimit { detail } => write!(f, "resource limit: {detail}"),
            CodeError::CrossScopeDenied { requested, actual } => {
                write!(f, "scope {requested} cannot read {actual}")
            }
            CodeError::Forbidden { required_scope } => {
                write!(f, "missing scope {required_scope}")
            }
            CodeError::EtagMismatch { expected, actual } => {
                write!(f, "etag mismatch: expected {expected}, found {actual}")
            }
            CodeError::SourceUnavailable { revision } => {
                write!(f, "source for revision {revision} is unavailable")
            }
            CodeError::SourceRevisionMismatch { requested, actual } => {
                write!(
                    f,
                    "requested revision {requested} but the index is {actual}"
                )
            }
            CodeError::ContextBudgetExceeded { required, budget } => {
                write!(f, "context budget exceeded: need {required}, have {budget}")
            }
            CodeError::IdempotencyConflict { key } => {
                write!(
                    f,
                    "idempotency key {key} was reused with different arguments"
                )
            }
        }
    }
}

impl std::error::Error for CodeError {}

/// Result alias.
pub type CodeResult<T> = Result<T, CodeError>;

/// Symbol kind (§32.1 `Symbol.symbol_kind`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// Module or package.
    Module,
    /// Class.
    Class,
    /// Struct.
    Struct,
    /// Trait.
    Trait,
    /// Interface.
    Interface,
    /// Free function.
    Function,
    /// Method.
    Method,
    /// Struct/class field.
    Field,
    /// Variable.
    Variable,
    /// Constant.
    Constant,
    /// Enum.
    Enum,
    /// Anything else.
    Other,
}

/// How confident the resolver is (§32.1 `Symbol.resolution`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Fully resolved.
    Resolved,
    /// Partially resolved (some references unresolved).
    Partial,
    /// Not resolved.
    Unresolved,
}

/// A source location with 1-based lines (§31.3 span rules).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeLocation {
    /// File identity (logical ref id).
    pub file_id: String,
    /// Source revision.
    pub revision: String,
    /// 1-based first line.
    pub line_start: u32,
    /// 1-based last line, inclusive.
    pub line_end: u32,
    /// Qualified name when known.
    pub qualified_name: Option<String>,
}

impl CodeLocation {
    /// Validate the span invariants; `line_end` must not precede `line_start`.
    pub fn validate(&self) -> CodeResult<()> {
        if self.line_start == 0 || self.line_end == 0 {
            return Err(CodeError::InvalidArgument {
                field: "line",
                reason: "lines are 1-based".into(),
            });
        }
        if self.line_end < self.line_start {
            return Err(CodeError::InvalidArgument {
                field: "line_end",
                reason: format!("end {} precedes start {}", self.line_end, self.line_start),
            });
        }
        Ok(())
    }
}

/// A symbol definition (§32.1 `Symbol`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", schemars(rename = "CodeSymbol"))]
pub struct Symbol {
    /// Stable logical id.
    pub id: String,
    /// Simple name.
    pub name: String,
    /// Qualified name (may equal `name`).
    pub qualified_name: String,
    /// Kind.
    pub kind: SymbolKind,
    /// Declaration site.
    pub definition: CodeLocation,
    /// Language identifier.
    pub language: String,
    /// Resolution state.
    pub resolution: Resolution,
    /// Declaration signature (part of the §32.3 identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Declared visibility, when the language has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Evidence ids backing the claim.
    pub evidence_ids: Vec<String>,
}

/// Edge kind (§32.1 `CodeEdge.kind`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeEdgeKind {
    /// Function call.
    Calls,
    /// Import.
    Imports,
    /// Dependency.
    DependsOn,
    /// Implements a trait/interface.
    Implements,
    /// Generic reference.
    References,
    /// Defines.
    Defines,
}

/// Edge resolution (§32.1 `CodeEdge.resolution`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeResolution {
    /// Statically certain.
    StaticExact,
    /// Statically possible (dynamic dispatch, reflection, …).
    StaticPossible,
    /// Unresolved.
    Unresolved,
}

/// A code edge (§32.1 `CodeEdge`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeEdge {
    /// Source symbol id.
    pub source: String,
    /// Target symbol id, when known.
    pub target: Option<String>,
    /// Edge kind.
    pub kind: CodeEdgeKind,
    /// Resolution state.
    pub resolution: EdgeResolution,
    /// Confidence in `0.0..=1.0`, when the extractor can estimate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Evidence ids.
    pub evidence_ids: Vec<String>,
}

impl CodeEdge {
    /// Whether the edge may be presented as certain. A `static_possible` edge is
    /// never upgraded (§32.1).
    pub fn is_certain(&self) -> bool {
        self.resolution == EdgeResolution::StaticExact && self.target.is_some()
    }
}

/// A diagnostic emitted during parsing (§32.1 `CodeDiagnostic`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeDiagnostic {
    /// Severity.
    pub severity: DiagnosticSeverity,
    /// Stable machine code.
    pub code: String,
    /// Safe message.
    pub message: String,
}

/// Diagnostic severity.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Informational.
    Info,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

/// Analysis manifest (§32.1 `CodeAnalysisManifest`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAnalysisManifest {
    /// Repository identity.
    pub repository_id: String,
    /// Commit analysed.
    pub commit_sha: String,
    /// Parser profile used.
    pub parser_profile: String,
    /// Files indexed.
    pub indexed_files: u64,
    /// Files skipped.
    pub skipped_files: u64,
    /// Diagnostics.
    pub diagnostics: Vec<CodeDiagnostic>,
    /// Published index generation.
    pub graph_generation: String,
}

impl CodeAnalysisManifest {
    /// Completeness is `partial` whenever anything was skipped or any diagnostic
    /// is an error (§32.1: never claim a complete call graph we do not have).
    pub fn completeness(&self) -> &'static str {
        if self.skipped_files > 0
            || self
                .diagnostics
                .iter()
                .any(|d| d.severity == DiagnosticSeverity::Error)
        {
            "partial"
        } else {
            "complete"
        }
    }
}

/// How well a language is supported here.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportLevel {
    /// Grammar ported and covered by golden parity tests.
    Ported,
    /// Grammar ported, parity tests pending.
    PortedUnverified,
    /// Present in the locked upstream baseline, not ported yet.
    NotPorted,
}

/// One language in the registry.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageEntry {
    /// Language identifier (also the upstream crate suffix).
    pub language: String,
    /// Typical file extensions.
    pub extensions: Vec<String>,
    /// Current support level in this repository.
    pub support_level: SupportLevel,
    /// Migration-ledger slice that must exist before `Ported` is allowed.
    pub ledger_slice: String,
}

/// The 38 languages of the locked upstream baseline (ADR-048 O-01).
pub const UPSTREAM_LANGUAGES: [&str; 38] = [
    "bash",
    "c",
    "clojure",
    "cobol",
    "cpp",
    "csharp",
    "css",
    "dart",
    "dockerfile",
    "elixir",
    "elm",
    "erlang",
    "fortran",
    "go",
    "groovy",
    "haskell",
    "hcl",
    "java",
    "julia",
    "kotlin",
    "lua",
    "objc",
    "ocaml",
    "perl",
    "php",
    "python",
    "r",
    "ruby",
    "rust",
    "scala",
    "solidity",
    "swift",
    "tcl",
    "toml",
    "typescript",
    "verilog",
    "yaml",
    "zig",
];

/// Languages whose grammar is actually ported in this build.
///
/// A language may only be listed here when its ledger slice under
/// `docs/migration/ledger/` records `grammar_ported: true` with a passing
/// golden parity test (see `tests/s2_rust_parity.rs`). The registry and the
/// migration paperwork are cross-checked by `tests/s2_coverage.rs`, so editing
/// this list without the evidence fails the build.
pub const PORTED_LANGUAGES: &[&str] = &[
    "bash",
    "c",
    "clojure",
    "cobol",
    "cpp",
    "csharp",
    "css",
    "dart",
    "dockerfile",
    "elixir",
    "elm",
    "erlang",
    "fortran",
    "go",
    "groovy",
    "haskell",
    "hcl",
    "java",
    "julia",
    "kotlin",
    "lua",
    "objc",
    "ocaml",
    "perl",
    "php",
    "python",
    "r",
    "ruby",
    "rust",
    "scala",
    "solidity",
    "swift",
    "tcl",
    "toml",
    "typescript",
    "verilog",
    "yaml",
    "zig",
];

fn extensions_for(language: &str) -> Vec<String> {
    let list: &[&str] = match language {
        "bash" => &["sh", "bash"],
        "c" => &["c", "h"],
        "clojure" => &["clj", "cljs"],
        "cobol" => &["cob", "cbl"],
        "cpp" => &["cpp", "cc", "hpp"],
        "csharp" => &["cs"],
        "css" => &["css"],
        "dart" => &["dart"],
        "dockerfile" => &["dockerfile"],
        "elixir" => &["ex", "exs"],
        "elm" => &["elm"],
        "erlang" => &["erl", "hrl"],
        "fortran" => &["f90", "f95"],
        "go" => &["go"],
        "groovy" => &["groovy"],
        "haskell" => &["hs"],
        "hcl" => &["hcl", "tf"],
        "java" => &["java"],
        "julia" => &["jl"],
        "kotlin" => &["kt", "kts"],
        "lua" => &["lua"],
        "objc" => &["m", "mm"],
        "ocaml" => &["ml", "mli"],
        "perl" => &["pl", "pm"],
        "php" => &["php"],
        "python" => &["py"],
        "r" => &["r"],
        "ruby" => &["rb"],
        "rust" => &["rs"],
        "scala" => &["scala"],
        "solidity" => &["sol"],
        "swift" => &["swift"],
        "tcl" => &["tcl"],
        "toml" => &["toml"],
        "typescript" => &["ts", "tsx", "js", "jsx"],
        "verilog" => &["v", "sv"],
        "yaml" => &["yaml", "yml"],
        "zig" => &["zig"],
        _ => &[],
    };
    list.iter().map(|ext| (*ext).to_string()).collect()
}

/// Registry of the languages this build knows about.
#[derive(Debug, Clone)]
pub struct LanguageRegistry {
    entries: Vec<LanguageEntry>,
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageRegistry {
    /// Registry of all 38 upstream languages with the support level this build
    /// can actually back with a grammar and a passing parity test.
    pub fn new() -> Self {
        let entries = UPSTREAM_LANGUAGES
            .iter()
            .map(|language| LanguageEntry {
                language: (*language).to_string(),
                extensions: extensions_for(language),
                support_level: if PORTED_LANGUAGES.contains(language) {
                    SupportLevel::Ported
                } else {
                    SupportLevel::NotPorted
                },
                ledger_slice: format!("S2-{language}"),
            })
            .collect();
        Self { entries }
    }

    /// All entries.
    pub fn entries(&self) -> &[LanguageEntry] {
        &self.entries
    }

    /// Look up one language.
    pub fn get(&self, language: &str) -> Option<&LanguageEntry> {
        self.entries.iter().find(|entry| entry.language == language)
    }

    /// Look up by file extension (case-insensitive, leading dot optional).
    pub fn by_extension(&self, extension: &str) -> Option<&LanguageEntry> {
        let needle = extension.trim_start_matches('.').to_ascii_lowercase();
        self.entries
            .iter()
            .find(|entry| entry.extensions.contains(&needle))
    }

    /// Languages that still need a migration ledger slice before they may claim
    /// `Ported`.
    pub fn not_ported(&self) -> Vec<&LanguageEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.support_level == SupportLevel::NotPorted)
            .collect()
    }
}

/// Parsed content (§7.1 `ParsedContent`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedSource {
    /// Language of the source.
    pub language: String,
    /// `sha256:<hex>` of the parsed bytes.
    pub content_hash: String,
    /// Parser identity.
    pub parser_version: String,
    /// Extracted symbols.
    pub symbols: Vec<Symbol>,
    /// Extracted edges.
    pub edges: Vec<CodeEdge>,
    /// Diagnostics.
    pub diagnostics: Vec<CodeDiagnostic>,
}

/// The parser port (§7.1).
pub trait ParserPort: Send + Sync {
    /// Support level for a language.
    fn support(&self, language: &str) -> SupportLevel;

    /// Parse one source file.
    ///
    /// MUST NOT return an empty `ParsedSource` on failure (§7.1): a failure is
    /// [`CodeError::SourceParseFailed`] or [`CodeError::CapabilityUnavailable`].
    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource>;
}

/// A parser port that has no grammars wired: every language is `NotPorted` and
/// every parse attempt fails with a typed error. It is the reference shape of a
/// parser port and the honest fallback for languages whose S2 slice has not
/// landed yet.
#[derive(Debug, Default)]
pub struct UnportedParser {
    registry: LanguageRegistry,
}

impl UnportedParser {
    /// New parser bound to the default registry.
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
        }
    }

    /// Registry used by this parser.
    pub fn registry(&self) -> &LanguageRegistry {
        &self.registry
    }
}

impl ParserPort for UnportedParser {
    fn support(&self, _language: &str) -> SupportLevel {
        // This port has no grammars at all, so it cannot claim any language as
        // ported even when the registry knows the language exists.
        SupportLevel::NotPorted
    }

    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        if path.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "path",
                reason: "must be non-empty".into(),
            });
        }
        if self.registry.get(language).is_none() {
            return Err(CodeError::InvalidArgument {
                field: "language",
                reason: format!("`{language}` is not in the 38-language baseline"),
            });
        }
        let _ = source;
        Err(CodeError::CapabilityUnavailable {
            language: language.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_all_38_upstream_languages() {
        let registry = LanguageRegistry::new();
        assert_eq!(registry.entries().len(), 38);
        for language in UPSTREAM_LANGUAGES {
            assert!(
                registry.get(language).is_some(),
                "{language} must be registered"
            );
        }
        assert_eq!(
            registry.not_ported().len(),
            UPSTREAM_LANGUAGES.len() - PORTED_LANGUAGES.len(),
            "landed slices: {PORTED_LANGUAGES:?}"
        );
        for language in PORTED_LANGUAGES {
            assert_eq!(
                registry.get(language).unwrap().support_level,
                SupportLevel::Ported,
                "{language}: listed in PORTED_LANGUAGES"
            );
        }
    }

    #[test]
    fn registry_resolves_common_extensions() {
        let registry = LanguageRegistry::new();
        assert_eq!(registry.by_extension("rs").unwrap().language, "rust");
        assert_eq!(registry.by_extension(".py").unwrap().language, "python");
        assert_eq!(registry.by_extension(".TS").unwrap().language, "typescript");
        assert!(registry.by_extension("unknown-ext").is_none());
    }

    #[test]
    fn every_language_has_a_ledger_slice_id() {
        let registry = LanguageRegistry::new();
        for entry in registry.entries() {
            assert_eq!(entry.ledger_slice, format!("S2-{}", entry.language));
        }
    }

    #[test]
    fn unported_parser_fails_with_typed_error_not_empty_output() {
        let parser = UnportedParser::new();
        assert_eq!(parser.support("rust"), SupportLevel::NotPorted);
        let error = parser
            .parse("src/lib.rs", "rust", "fn main() {}")
            .unwrap_err();
        assert_eq!(error.code(), "CAPABILITY_UNAVAILABLE");
        assert!(error.to_string().contains("rust"));
    }

    #[test]
    fn unported_parser_rejects_unknown_language() {
        let parser = UnportedParser::new();
        let error = parser.parse("a.x", "brainfuck", "").unwrap_err();
        assert_eq!(error.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn dynamic_calls_are_never_promoted_to_certain() {
        let possible = CodeEdge {
            source: "a".into(),
            target: Some("b".into()),
            kind: CodeEdgeKind::Calls,
            resolution: EdgeResolution::StaticPossible,
            confidence: None,
            evidence_ids: vec!["e1".into()],
        };
        assert!(!possible.is_certain());

        let unresolved = CodeEdge {
            source: "a".into(),
            target: None,
            kind: CodeEdgeKind::Calls,
            resolution: EdgeResolution::Unresolved,
            confidence: None,
            evidence_ids: vec![],
        };
        assert!(!unresolved.is_certain());

        let exact = CodeEdge {
            source: "a".into(),
            target: Some("b".into()),
            kind: CodeEdgeKind::Calls,
            resolution: EdgeResolution::StaticExact,
            confidence: None,
            evidence_ids: vec!["e1".into()],
        };
        assert!(exact.is_certain());
    }

    #[test]
    fn manifest_reports_partial_when_anything_was_skipped() {
        let mut manifest = CodeAnalysisManifest {
            repository_id: "repo:1".into(),
            commit_sha: "0".repeat(40),
            parser_profile: "default".into(),
            indexed_files: 10,
            skipped_files: 0,
            diagnostics: vec![],
            graph_generation: "gen-1".into(),
        };
        assert_eq!(manifest.completeness(), "complete");
        manifest.skipped_files = 1;
        assert_eq!(manifest.completeness(), "partial");
        manifest.skipped_files = 0;
        manifest.diagnostics.push(CodeDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "PARSE_ERROR".into(),
            message: "unexpected token".into(),
        });
        assert_eq!(manifest.completeness(), "partial");
    }

    #[test]
    fn code_location_enforces_span_rules() {
        let valid = CodeLocation {
            file_id: "f1".into(),
            revision: "r1".into(),
            line_start: 3,
            line_end: 9,
            qualified_name: None,
        };
        assert!(valid.validate().is_ok());

        let mut zero = valid.clone();
        zero.line_start = 0;
        assert_eq!(zero.validate().unwrap_err().code(), "INVALID_ARGUMENT");

        let mut reversed = valid.clone();
        reversed.line_end = 1;
        assert_eq!(reversed.validate().unwrap_err().code(), "INVALID_ARGUMENT");
    }
}
