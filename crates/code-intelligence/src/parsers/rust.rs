//! `S2-rust` — Rust symbol/edge extraction over the locked grammar version.
//!
//! Substrate: `tree-sitter 0.25.10` + `tree-sitter-rust 0.24.2`, the exact
//! versions in the locked upstream `Cargo.lock`, so the concrete syntax tree is
//! the same one CodeGraph parsed.
//!
//! Extraction rules (documented because they define what "parity" means here):
//! - one `Module` symbol represents the file, named after the file stem;
//! - `mod`/`struct`/`enum`/`trait`/`fn`/`const`/`static`/`type` items become
//!   symbols; `fn` inside an `impl` becomes a `Method` qualified as `Type::name`;
//! - containment between items becomes a `Defines` edge with
//!   `EdgeResolution::StaticExact` (the CST proves it);
//! - calls are **never** claimed exact: a callee matching a symbol defined in the
//!   same file is `StaticPossible`, everything else is `Unresolved` with no
//!   target (§32.1: no name resolution is performed);
//! - `use` declarations become `Imports` edges, always `Unresolved`;
//! - a tree containing errors still yields the symbols it found **plus**
//!   `PARSE_ERROR` diagnostics (never an empty successful parse, §7.1).

use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser};

use crate::{
    CodeAnalysisManifest, CodeDiagnostic, CodeEdge, CodeEdgeKind, CodeError, CodeLocation,
    CodeResult, DiagnosticSeverity, EdgeResolution, ParsedSource, ParserPort, Resolution,
    SupportLevel, Symbol, SymbolKind,
};

/// Rust parser bound to the pinned grammar.
pub struct RustParser {
    /// Parser profile recorded in manifests.
    profile: String,
}

impl Default for RustParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RustParser {
    /// New parser with the default profile.
    pub fn new() -> Self {
        Self {
            profile: "tree-sitter-rust-0.24.2/ts-0.25.10".to_string(),
        }
    }

    /// Parser profile string.
    pub fn profile(&self) -> &str {
        &self.profile
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
            parser_profile: self.profile.clone(),
            indexed_files: 1,
            skipped_files: 0,
            diagnostics: parsed.diagnostics.clone(),
            graph_generation: generation.to_string(),
        }
    }
}

fn content_hash(source: &str) -> String {
    let digest = Sha256::digest(source.as_bytes());
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.strip_suffix(".rs").unwrap_or(name).to_string()
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

fn field_name<'a>(source: &'a str, node: Node<'_>) -> Option<&'a str> {
    node.child_by_field_name("name")
        .map(|child| text(source, child))
}

fn item_kind(kind: &str) -> Option<SymbolKind> {
    Some(match kind {
        "mod_item" => SymbolKind::Module,
        "struct_item" | "union_item" => SymbolKind::Struct,
        "enum_item" => SymbolKind::Enum,
        "trait_item" => SymbolKind::Trait,
        "function_item" => SymbolKind::Function,
        // trait/foreign declarations have signatures without bodies
        "function_signature_item" => SymbolKind::Method,
        "const_item" | "static_item" => SymbolKind::Constant,
        "type_item" => SymbolKind::Other,
        _ => return None,
    })
}

fn callee_name(callee: &str) -> Option<String> {
    // `path::to::f`, `self.method`, `value.method` -> last segment
    let name = callee
        .rsplit("::")
        .next()
        .unwrap_or(callee)
        .rsplit('.')
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
    symbols: Vec<Symbol>,
    defines: Vec<(String, String, u32)>,
    imports: Vec<(String, u32)>,
    calls: Vec<(String, String, u32)>,
    edges: Vec<CodeEdge>,
    diagnostics: Vec<CodeDiagnostic>,
}

impl<'a> Extractor<'a> {
    fn new(source: &'a str, path: &'a str) -> Self {
        Self {
            source,
            path,
            stem: file_stem(path),
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

    fn push_symbol(
        &mut self,
        name: &str,
        qualified: String,
        kind: SymbolKind,
        node: Node<'_>,
        language: &str,
    ) -> String {
        let id = self.symbol_id(&qualified);
        if self.symbols.iter().any(|symbol| symbol.id == id) {
            return id;
        }
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
            language: language.to_string(),
            // Symbol existence is proven by the CST; references are not.
            resolution: Resolution::Resolved,
            evidence_ids: vec![format!("ev:{}:{}", self.path, line_of(node, true))],
        });
        id
    }

    fn qualified_child(&self, parent: Option<&str>, name: &str) -> String {
        match parent.and_then(|parent_id| self.symbols.iter().find(|s| s.id == parent_id)) {
            Some(symbol) => format!("{}::{name}", symbol.qualified_name),
            None => name.to_string(),
        }
    }

    fn walk(&mut self, node: Node<'_>, parent: Option<&str>) {
        let kind = node.kind();
        let mut child_parent = parent.map(str::to_string);

        if let Some(symbol_kind) = item_kind(kind) {
            if let Some(name) = field_name(self.source, node) {
                let qualified = self.qualified_child(parent, name);
                let id = self.push_symbol(name, qualified, symbol_kind, node, "rust");
                if let Some(parent_id) = parent {
                    self.defines
                        .push((parent_id.to_string(), id.clone(), line_of(node, true)));
                }
                child_parent = Some(id);
            }
        } else if kind == "impl_item" {
            // `impl` blocks are transparent scopes: the methods belong to the
            // type, so they are emitted here instead of by the generic path.
            self.handle_impl(node, parent);
            return;
        } else if kind == "use_declaration" {
            if let Some(parent_id) = parent {
                self.imports
                    .push((parent_id.to_string(), line_of(node, true)));
            }
        } else if kind == "call_expression" {
            if let (Some(parent_id), Some(function)) =
                (parent, node.child_by_field_name("function"))
            {
                if let Some(simple) = callee_name(text(self.source, function)) {
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

    /// `impl Type { .. }` / `impl Trait for Type { .. }`: the type names the
    /// owner of the methods.
    fn handle_impl(&mut self, node: Node<'_>, parent: Option<&str>) {
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

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() != "declaration_list" {
                continue;
            }
            let mut inner = child.walk();
            for item in child.children(&mut inner) {
                if item.kind() != "function_item" {
                    continue;
                }
                let Some(name) = field_name(self.source, item) else {
                    continue;
                };
                let qualified = format!("{owner_qualified}::{name}");
                let id = self.push_symbol(name, qualified, SymbolKind::Method, item, "rust");
                if let Some(owner) = &owner_id {
                    self.defines
                        .push((owner.clone(), id.clone(), line_of(item, true)));
                }
                // walk the body so call edges keep the method as their source
                self.walk_children(item, Some(&id));
            }
        }
    }

    /// Resolve the deferred references once every symbol in the file is known,
    /// so the result does not depend on declaration order.
    fn finish(&mut self) {
        let evidence = |line: u32| vec![format!("ev:{}:{}", self.path, line)];
        for (parent, child, line) in std::mem::take(&mut self.defines) {
            self.edges.push(CodeEdge {
                source: parent,
                target: Some(child),
                kind: CodeEdgeKind::Defines,
                // containment is proven by the CST itself
                resolution: EdgeResolution::StaticExact,
                evidence_ids: evidence(line),
            });
        }
        for (parent, line) in std::mem::take(&mut self.imports) {
            self.edges.push(CodeEdge {
                source: parent,
                target: None,
                kind: CodeEdgeKind::Imports,
                resolution: EdgeResolution::Unresolved,
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
                // a call to the enclosing function (`BTreeMap::new()` inside
                // `new`) is not evidence of a self edge
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
                evidence_ids: evidence(line),
            });
        }
    }

    fn collect_diagnostics(&mut self, root: Node<'_>) {
        if root.has_error() {
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
}

impl ParserPort for RustParser {
    fn support(&self, language: &str) -> SupportLevel {
        if language == "rust" {
            SupportLevel::Ported
        } else {
            SupportLevel::NotPorted
        }
    }

    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        if path.trim().is_empty() {
            return Err(CodeError::InvalidArgument {
                field: "path",
                reason: "must be non-empty".into(),
            });
        }
        if language != "rust" {
            return Err(CodeError::CapabilityUnavailable {
                language: language.to_string(),
            });
        }
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| CodeError::SourceParseFailed {
                message: format!("grammar load failed: {error}"),
            })?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| CodeError::SourceParseFailed {
                message: "parser returned no tree".to_string(),
            })?;
        let root = tree.root_node();

        let mut extractor = Extractor::new(source, path);
        let stem = extractor.stem.clone();
        let module_id =
            extractor.push_symbol(&stem, stem.clone(), SymbolKind::Module, root, "rust");
        extractor.collect_diagnostics(root);
        extractor.walk(root, Some(&module_id));
        extractor.finish();

        Ok(ParsedSource {
            language: "rust".to_string(),
            content_hash: content_hash(source),
            parser_version: self.profile.clone(),
            symbols: extractor.symbols,
            edges: extractor.edges,
            diagnostics: extractor.diagnostics,
        })
    }
}
