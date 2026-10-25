//! Generic tree-sitter extractor.
//!
//! One CST walker serves every language: [`super::spec::LanguageSpec`] supplies
//! the node kinds, this module supplies the traversal, the §32.3-relevant spans,
//! the containment/import/call edges and the "never claim more than the evidence"
//! resolution rules.
//!
//! Invariants (identical for every language, so a language cannot quietly behave
//! differently):
//! - symbol existence and containment are `static_exact` (the CST proves them);
//! - a call edge is `static_possible` only when a *same file* symbol matches the
//!   callee name, and never `static_exact`;
//! - imports are `unresolved`;
//! - a syntax error produces `PARSE_ERROR` diagnostics, never an empty document;
//! - references are resolved in a second pass, so output does not depend on
//!   declaration order.

use tree_sitter::{Node, Parser};

use super::spec::{self, is_container, LanguageSpec};
use crate::{
    CodeAnalysisManifest, CodeDiagnostic, CodeEdge, CodeEdgeKind, CodeError, CodeLocation,
    CodeResult, DiagnosticSeverity, EdgeResolution, ParsedSource, ParserPort, Resolution,
    SupportLevel, Symbol, SymbolKind,
};

/// sha256 of the parsed bytes.
fn content_hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(source.as_bytes());
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let name = name.split('.').next().unwrap_or(name);
    name.to_string()
}

fn line_of(node: Node<'_>, start: bool) -> u32 {
    let position = if start {
        node.start_position()
    } else {
        node.end_position()
    };
    (position.row as u32).saturating_add(1)
}

fn text<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    source.get(node.byte_range()).unwrap_or_default()
}

fn field_name<'a>(source: &'a str, node: Node<'_>, field: &str) -> Option<&'a str> {
    node.child_by_field_name(field)
        .map(|child| text(source, child))
}

/// Declaration header text (`fn name(args) -> ret`), used as the signature.
fn signature_of(source: &str, node: Node<'_>, body_fields: &[&str]) -> Option<String> {
    let body = body_fields
        .iter()
        .find_map(|field| node.child_by_field_name(field))?;
    let end = body.start_byte();
    if end <= node.start_byte() {
        return None;
    }
    let header = source.get(node.start_byte()..end)?;
    let collapsed = header.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// Last path/dot segment of a callee expression (`self.check(x)` -> `check`).
fn callee_name(callee: &str) -> Option<String> {
    let name = callee
        .rsplit("::")
        .next()
        .unwrap_or(callee)
        .rsplit(['.', '>'])
        .next()
        .unwrap_or(callee)
        .trim_end_matches(['(', ')'])
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

struct Extractor<'a> {
    source: &'a str,
    path: &'a str,
    stem: String,
    spec: &'static LanguageSpec,
    symbols: Vec<Symbol>,
    defines: Vec<(String, String, u32)>,
    imports: Vec<(String, u32)>,
    calls: Vec<(String, String, u32)>,
    edges: Vec<CodeEdge>,
    diagnostics: Vec<CodeDiagnostic>,
}

impl<'a> Extractor<'a> {
    fn new(source: &'a str, path: &'a str, spec: &'static LanguageSpec) -> Self {
        Self {
            source,
            path,
            stem: file_stem(path),
            spec,
            symbols: Vec::new(),
            defines: Vec::new(),
            imports: Vec::new(),
            calls: Vec::new(),
            edges: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn symbol_id(&self, qualified: &str) -> String {
        format!("sym:{}:{}", self.stem, qualified)
    }

    fn symbol(&self, id: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|symbol| symbol.id == id)
    }

    fn push_symbol(
        &mut self,
        name: &str,
        qualified: String,
        kind: SymbolKind,
        node: Node<'_>,
    ) -> String {
        let id = self.symbol_id(&qualified);
        if self.symbols.iter().any(|symbol| symbol.id == id) {
            return id;
        }
        let signature = match kind {
            SymbolKind::Function | SymbolKind::Method => {
                signature_of(self.source, node, self.spec.body_fields)
            }
            _ => None,
        };
        self.symbols.push(Symbol {
            id: id.clone(),
            name: name.to_string(),
            qualified_name: qualified,
            kind,
            definition: CodeLocation {
                file_id: format!("file:{}", self.path),
                revision: "worktree".to_string(),
                line_start: line_of(node, true),
                line_end: line_of(node, false),
                qualified_name: None,
            },
            language: self.spec.language.to_string(),
            // Symbol existence is proven by the CST; references are not.
            resolution: Resolution::Resolved,
            signature,
            visibility: None,
            evidence_ids: vec![format!("ev:{}:{}", self.path, line_of(node, true))],
        });
        id
    }

    /// Resolve a declaration's name: the `name` field when the grammar has one,
    /// otherwise the identifier reached through a chain of `declarator` fields
    /// (C/C++ put the name inside the declarator).
    fn resolve_name(&self, node: Node<'_>) -> Option<&'a str> {
        if let Some(name) = field_name(self.source, node, self.spec.name_field) {
            return Some(name);
        }
        if !self.spec.declarator_names {
            return None;
        }
        let mut current = node;
        for _ in 0..4 {
            let child = current.child_by_field_name("declarator")?;
            if let Some(name) = field_name(self.source, child, self.spec.name_field) {
                return Some(name);
            }
            if matches!(
                child.kind(),
                "identifier"
                    | "field_identifier"
                    | "type_identifier"
                    | "operator_name"
                    | "destructor_name"
            ) {
                return Some(text(self.source, child));
            }
            current = child;
        }
        None
    }

    fn qualified_child(&self, parent: Option<&str>, name: &str) -> String {
        match parent.and_then(|parent_id| self.symbol(parent_id)) {
            Some(symbol) => format!("{}::{name}", symbol.qualified_name),
            None => name.to_string(),
        }
    }

    fn walk(&mut self, node: Node<'_>, parent: Option<&str>) {
        let kind = node.kind();
        let mut child_parent = parent.map(str::to_string);

        if let Some(declared) = self.spec.declaration(kind) {
            if let Some(name) = self.resolve_name(node) {
                // A function declared directly inside a type is a method.
                let kind = if declared == SymbolKind::Function
                    && parent
                        .and_then(|parent_id| self.symbol(parent_id))
                        .map(|symbol| is_container(symbol.kind))
                        .unwrap_or(false)
                {
                    SymbolKind::Method
                } else {
                    declared
                };
                let qualified = self.qualified_child(parent, name);
                let id = self.push_symbol(name, qualified, kind, node);
                if let Some(parent_id) = parent {
                    self.defines
                        .push((parent_id.to_string(), id.clone(), line_of(node, true)));
                }
                child_parent = Some(id);
            }
        } else if self.spec.is_type_impl(kind) {
            // `impl Type { .. }` (and friends): the members belong to the type.
            self.handle_type_impl(node, parent);
            return;
        } else if self.spec.is_import(kind) {
            if let Some(parent_id) = parent {
                self.imports
                    .push((parent_id.to_string(), line_of(node, true)));
            }
        } else if self.spec.is_call(kind) {
            if let (Some(parent_id), Some(callee)) = (
                parent,
                node.child_by_field_name(self.spec.call_field)
                    .map(|child| text(self.source, child)),
            ) {
                if let Some(simple) = callee_name(callee) {
                    self.calls
                        .push((parent_id.to_string(), simple, line_of(node, true)));
                }
            }
        }

        self.walk_children(node, child_parent.as_deref());
    }

    fn walk_children(&mut self, node: Node<'_>, parent: Option<&str>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child, parent);
        }
    }

    fn handle_type_impl(&mut self, node: Node<'_>, parent: Option<&str>) {
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let raw_owner = text(self.source, type_node);
        let owner_name = raw_owner
            .split('<')
            .next()
            .unwrap_or(raw_owner)
            .trim()
            .to_string();
        let owner = self
            .symbols
            .iter()
            .find(|symbol| symbol.name == owner_name && symbol.kind != SymbolKind::Method)
            .map(|symbol| (symbol.id.clone(), symbol.qualified_name.clone()));
        let (owner_id, owner_qualified) = match owner {
            Some((id, qualified)) => (Some(id), qualified),
            None => (None, self.qualified_child(parent, &owner_name)),
        };

        let mut functions: Vec<Node<'_>> = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "declaration_list" {
                let mut inner = child.walk();
                for item in child.children(&mut inner) {
                    if self.spec.declaration(item.kind()) == Some(SymbolKind::Function) {
                        functions.push(item);
                    }
                }
            }
        }
        for item in functions {
            let Some(name) = field_name(self.source, item, self.spec.name_field) else {
                continue;
            };
            let qualified = format!("{owner_qualified}::{name}");
            let id = self.push_symbol(name, qualified, SymbolKind::Method, item);
            if let Some(owner) = &owner_id {
                self.defines
                    .push((owner.clone(), id.clone(), line_of(item, true)));
            }
            self.walk_children(item, Some(&id));
        }
    }

    /// Resolve deferred references once every symbol in the file is known.
    fn finish(&mut self) {
        let evidence = |line: u32| vec![format!("ev:{}:{}", self.path, line)];
        for (parent, child, line) in std::mem::take(&mut self.defines) {
            self.edges.push(CodeEdge {
                source: parent,
                target: Some(child),
                kind: CodeEdgeKind::Defines,
                // containment is proven by the CST itself
                resolution: EdgeResolution::StaticExact,
                confidence: None,
                evidence_ids: evidence(line),
            });
        }
        for (parent, line) in std::mem::take(&mut self.imports) {
            self.edges.push(CodeEdge {
                source: parent,
                target: None,
                kind: CodeEdgeKind::Imports,
                resolution: EdgeResolution::Unresolved,
                confidence: None,
                evidence_ids: evidence(line),
            });
        }
        for (parent, simple, line) in std::mem::take(&mut self.calls) {
            // A same-file name match is only "possible": no name resolution is
            // performed, and a call is never claimed as `static_exact`.
            let target = self
                .symbols
                .iter()
                .find(|symbol| {
                    symbol.name == simple
                        && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                })
                .map(|symbol| symbol.id.clone())
                // a call to the enclosing function is not a self edge
                .filter(|candidate| *candidate != parent);
            let resolution = if target.is_some() {
                EdgeResolution::StaticPossible
            } else {
                EdgeResolution::Unresolved
            };
            if self.edges.iter().any(|edge| {
                edge.source == parent && edge.kind == CodeEdgeKind::Calls && edge.target == target
            }) {
                continue;
            }
            self.edges.push(CodeEdge {
                source: parent,
                target,
                kind: CodeEdgeKind::Calls,
                resolution,
                confidence: None,
                evidence_ids: evidence(line),
            });
        }
    }

    fn collect_diagnostics(&mut self, root: Node<'_>) {
        if !root.has_error() {
            return;
        }
        let mut cursor = root.walk();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.is_error() || node.is_missing() {
                self.diagnostics.push(CodeDiagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "PARSE_ERROR".to_string(),
                    message: format!("unparsable construct `{}`", node.kind()),
                });
            }
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        if self.diagnostics.is_empty() {
            self.diagnostics.push(CodeDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: "PARSE_ERROR".to_string(),
                message: "syntax tree contains errors".to_string(),
            });
        }
    }
}

/// A parser bound to one language spec.
pub struct TreeSitterParser {
    spec: &'static LanguageSpec,
}

impl TreeSitterParser {
    /// Parser for `spec`.
    pub fn new(spec: &'static LanguageSpec) -> Self {
        Self { spec }
    }

    /// The spec this parser is bound to.
    pub fn spec(&self) -> &'static LanguageSpec {
        self.spec
    }

    /// Parser profile recorded in manifests.
    pub fn profile(&self) -> &str {
        self.spec.profile
    }

    /// Build an analysis manifest for a parsed source.
    pub fn manifest(
        &self,
        repository_id: &str,
        commit_sha: &str,
        generation: &str,
        parsed: &ParsedSource,
    ) -> CodeAnalysisManifest {
        CodeAnalysisManifest {
            repository_id: repository_id.to_string(),
            commit_sha: commit_sha.to_string(),
            parser_profile: self.spec.profile.to_string(),
            indexed_files: 1,
            skipped_files: 0,
            diagnostics: parsed.diagnostics.clone(),
            graph_generation: generation.to_string(),
        }
    }

    fn parse_with(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        if path.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "path",
                reason: "must be non-empty".into(),
            });
        }
        if language != self.spec.language {
            return Err(CodeError::CapabilityUnavailable {
                language: language.to_string(),
            });
        }
        let mut parser = Parser::new();
        parser
            .set_language(&(self.spec.grammar)())
            .map_err(|error| CodeError::SourceParseFailed {
                message: format!("grammar load failed: {error}"),
            })?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| CodeError::SourceParseFailed {
                message: "parser returned no tree".to_string(),
            })?;
        let root = tree.root_node();

        let mut extractor = Extractor::new(source, path, self.spec);
        let stem = extractor.stem.clone();
        let module_id = extractor.push_symbol(&stem, stem.clone(), SymbolKind::Module, root);
        extractor.collect_diagnostics(root);
        extractor.walk(root, Some(&module_id));
        extractor.finish();

        Ok(ParsedSource {
            language: self.spec.language.to_string(),
            content_hash: content_hash(source),
            parser_version: self.spec.profile.to_string(),
            symbols: extractor.symbols,
            edges: extractor.edges,
            diagnostics: extractor.diagnostics,
        })
    }
}

impl ParserPort for TreeSitterParser {
    fn support(&self, language: &str) -> SupportLevel {
        if language == self.spec.language {
            SupportLevel::Ported
        } else {
            SupportLevel::NotPorted
        }
    }

    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        self.parse_with(path, language, source)
    }
}

/// One parser for every language this build has a spec and a grammar for.
#[derive(Debug, Default)]
pub struct UnifiedParser;

impl UnifiedParser {
    /// New unified parser.
    pub fn new() -> Self {
        Self
    }

    /// Languages this build can actually parse.
    pub fn ported_languages() -> Vec<&'static str> {
        spec::specs()
            .into_iter()
            .map(|spec| spec.language)
            .collect()
    }
}

impl ParserPort for UnifiedParser {
    fn support(&self, language: &str) -> SupportLevel {
        match spec::spec(language) {
            Some(_) => SupportLevel::Ported,
            None => SupportLevel::NotPorted,
        }
    }

    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        let spec = spec::spec(language).ok_or_else(|| CodeError::CapabilityUnavailable {
            language: language.to_string(),
        })?;
        TreeSitterParser::new(spec).parse(path, language, source)
    }
}
