//! Declarative language specs for the generic tree-sitter extractor.
//!
//! One spec per language: the grammar constructor, the node kinds that declare
//! symbols, the node kinds that import and call, and the node kinds that are
//! transparent scopes (Rust `impl`). Everything else lives in
//! [`super::engine`], so adding a language is a data change, not new logic.
//!
//! Grammar versions are pinned in `Cargo.toml`. For the 31 languages that exist
//! in the locked upstream `Cargo.lock` the pin is exactly the baseline version;
//! the rest are recorded as deviations in their ledger slice (ADR-049 O-08).

use crate::SymbolKind;

/// How one language maps onto the generic extractor.
#[derive(Debug)]
pub struct LanguageSpec {
    /// Language identifier (must match the language registry).
    pub language: &'static str,
    /// Parser profile recorded in manifests (`tree-sitter-<lang>-<ver>/ts-<ver>`).
    pub profile: &'static str,
    /// Declaration node kind -> symbol kind.
    pub declarations: &'static [(&'static str, SymbolKind)],
    /// Field that holds a declaration's name.
    pub name_field: &'static str,
    /// Node kinds that represent a call.
    pub call_kinds: &'static [&'static str],
    /// Fields that may hold the callee of a call node, tried in order.
    pub call_fields: &'static [&'static str],
    /// Node kinds that represent an import/include.
    pub import_kinds: &'static [&'static str],
    /// Transparent scopes whose members belong to the type named in `type`.
    pub type_impl_kinds: &'static [&'static str],
    /// Fields that may hold a body (used to cut the signature).
    pub body_fields: &'static [&'static str],
    /// Form heads for S-expression/macro languages: `(defn name ..)` in Clojure
    /// or `def name` in Elixir carry no declaration node kind, so the node kind
    /// plus its head symbol decides the symbol kind.
    pub form_heads: &'static [(&'static str, &'static str, SymbolKind)],
    /// Declarations that are only valid when they contain a marker child, for
    /// grammars that reuse a kind name (Haskell `function` is both a definition
    /// and a function *type*; only a definition has a `match` child).
    pub declaration_child_kinds: &'static [(&'static str, &'static str)],
    /// Whether the name may be found as an identifier-like descendant when the
    /// grammar exposes no `name` field.
    pub name_from_descendant: bool,
    /// Identifier-like descendants to skip before taking the name, for grammars
    /// that put the return type first (`int compute(..)`).
    pub name_skip: &'static [(&'static str, usize)],
    /// Declaration kinds that must be ignored under a given parent kind, for
    /// grammars that nest the same construct twice (dart's `method_signature`
    /// contains a `function_signature`).
    pub declaration_parent_exclude: &'static [(&'static str, &'static str)],
    /// Whether the name sits inside a `declarator` chain (C, C++).
    pub declarator_names: bool,
}

impl LanguageSpec {
    /// Symbol kind for a declaration node kind.
    pub fn declaration(&self, kind: &str) -> Option<SymbolKind> {
        self.declarations
            .iter()
            .find(|(candidate, _)| *candidate == kind)
            .map(|(_, symbol_kind)| *symbol_kind)
    }

    /// Whether the node kind is a call.
    pub fn is_call(&self, kind: &str) -> bool {
        self.call_kinds.contains(&kind)
    }

    /// How many identifier-like descendants to skip for this node kind.
    pub fn name_skip_for(&self, kind: &str) -> usize {
        self.name_skip
            .iter()
            .find(|(candidate, _)| *candidate == kind)
            .map(|(_, skip)| *skip)
            .unwrap_or(0)
    }

    /// Whether a declaration node is really a declaration (it may contain the
    /// marker child its grammar requires).
    pub fn declaration_allowed_in(
        &self,
        node: tree_sitter::Node<'_>,
        parent_kind: Option<&str>,
    ) -> bool {
        let kind = node.kind();
        if self
            .declaration_parent_exclude
            .iter()
            .any(|(candidate, parent)| *candidate == kind && parent_kind == Some(*parent))
        {
            return false;
        }
        match self
            .declaration_child_kinds
            .iter()
            .find(|(candidate, _)| *candidate == kind)
        {
            Some((_, required)) => {
                let mut cursor = node.walk();
                let found = node
                    .children(&mut cursor)
                    .any(|child| child.kind() == *required);
                found
            }
            None => true,
        }
    }

    /// Whether the node kind is an import.
    pub fn is_import(&self, kind: &str) -> bool {
        self.import_kinds.contains(&kind)
    }

    /// Whether the node kind is a transparent type-implementation scope.
    pub fn is_type_impl(&self, kind: &str) -> bool {
        self.type_impl_kinds.contains(&kind)
    }
}

/// Symbol kinds that own members (a function declared inside one is a method).
pub fn is_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::Enum
    )
}

// ---------------------------------------------------------------------------
// Rust (locked-baseline grammar, S2-rust)
// ---------------------------------------------------------------------------

/// Rust.
#[cfg(feature = "parser-rust")]
pub static RUST: LanguageSpec = LanguageSpec {
    language: "rust",
    profile: "tree-sitter-rust-0.24.2/ts-0.25.10",
    declarations: &[
        ("mod_item", SymbolKind::Module),
        ("struct_item", SymbolKind::Struct),
        ("union_item", SymbolKind::Struct),
        ("enum_item", SymbolKind::Enum),
        ("trait_item", SymbolKind::Trait),
        ("function_item", SymbolKind::Function),
        ("function_signature_item", SymbolKind::Method),
        ("const_item", SymbolKind::Constant),
        ("static_item", SymbolKind::Constant),
        ("type_item", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["use_declaration"],
    type_impl_kinds: &["impl_item"],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

// ---------------------------------------------------------------------------
// Batch 1: python, go, java, bash, c, cpp
// ---------------------------------------------------------------------------

/// Python.
#[cfg(feature = "parser-s2-batch1")]
pub static PYTHON: LanguageSpec = LanguageSpec {
    language: "python",
    profile: "tree-sitter-python-0.25.0/ts-0.25.10",
    declarations: &[
        ("class_definition", SymbolKind::Class),
        ("function_definition", SymbolKind::Function),
        ("decorated_definition", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call"],
    call_fields: &["function"],
    import_kinds: &["import_statement", "import_from_statement"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Go.
#[cfg(feature = "parser-s2-batch1")]
pub static GO: LanguageSpec = LanguageSpec {
    language: "go",
    profile: "tree-sitter-go-0.25.0/ts-0.25.10",
    declarations: &[
        ("function_declaration", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("type_spec", SymbolKind::Struct),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Java.
#[cfg(feature = "parser-s2-batch1")]
pub static JAVA: LanguageSpec = LanguageSpec {
    language: "java",
    profile: "tree-sitter-java-0.23.5/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("record_declaration", SymbolKind::Class),
        ("method_declaration", SymbolKind::Function),
        ("constructor_declaration", SymbolKind::Method),
        ("field_declaration", SymbolKind::Field),
    ],
    name_field: "name",
    call_kinds: &["method_invocation"],
    call_fields: &["name"],
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Bash.
#[cfg(feature = "parser-s2-batch1")]
pub static BASH: LanguageSpec = LanguageSpec {
    language: "bash",
    profile: "tree-sitter-bash-0.25.1/ts-0.25.10",
    declarations: &[
        ("function_definition", SymbolKind::Function),
        ("variable_assignment", SymbolKind::Variable),
    ],
    name_field: "name",
    // Bash has no static call graph: a command is a lookup by name at run time,
    // so the best a call edge may claim is `static_possible`.
    call_kinds: &["command"],
    call_fields: &["name"],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// C.
#[cfg(feature = "parser-s2-batch1")]
pub static C: LanguageSpec = LanguageSpec {
    language: "c",
    profile: "tree-sitter-c-0.24.1/ts-0.25.10",
    declarations: &[
        ("function_definition", SymbolKind::Function),
        ("struct_specifier", SymbolKind::Struct),
        ("union_specifier", SymbolKind::Struct),
        ("enum_specifier", SymbolKind::Enum),
        ("type_definition", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["preproc_include"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: true,
};

/// C++.
#[cfg(feature = "parser-s2-batch1")]
pub static CPP: LanguageSpec = LanguageSpec {
    language: "cpp",
    profile: "tree-sitter-cpp-0.23.4/ts-0.25.10",
    declarations: &[
        ("function_definition", SymbolKind::Function),
        ("class_specifier", SymbolKind::Class),
        ("struct_specifier", SymbolKind::Struct),
        ("union_specifier", SymbolKind::Struct),
        ("enum_specifier", SymbolKind::Enum),
        ("namespace_definition", SymbolKind::Module),
        ("type_definition", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["preproc_include"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: true,
};

// ---------------------------------------------------------------------------
// Batch 2
// ---------------------------------------------------------------------------

/// TypeScript (also used for the js/jsx extensions of the baseline language set).
#[cfg(feature = "parser-s2-batch2")]
pub static TYPESCRIPT: LanguageSpec = LanguageSpec {
    language: "typescript",
    profile: "tree-sitter-typescript-0.23.2/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("abstract_class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("function_declaration", SymbolKind::Function),
        ("method_definition", SymbolKind::Method),
        ("type_alias_declaration", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import_statement"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Ruby.
#[cfg(feature = "parser-s2-batch2")]
pub static RUBY: LanguageSpec = LanguageSpec {
    language: "ruby",
    profile: "tree-sitter-ruby-0.23.1/ts-0.25.10",
    declarations: &[
        ("class", SymbolKind::Class),
        ("module", SymbolKind::Module),
        // a Ruby `def` at the top level is a method on Object, but for the code
        // graph it is a free function; inside a class it is promoted to Method
        ("method", SymbolKind::Function),
        ("singleton_method", SymbolKind::Method),
    ],
    name_field: "name",
    call_kinds: &["call"],
    call_fields: &["method"],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// PHP.
#[cfg(feature = "parser-s2-batch2")]
pub static PHP: LanguageSpec = LanguageSpec {
    language: "php",
    profile: "tree-sitter-php-0.24.2/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("trait_declaration", SymbolKind::Trait),
        ("enum_declaration", SymbolKind::Enum),
        ("function_definition", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("namespace_definition", SymbolKind::Module),
    ],
    name_field: "name",
    call_kinds: &["function_call_expression", "member_call_expression"],
    call_fields: &["function", "name"],
    import_kinds: &["namespace_use_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// C#.
#[cfg(feature = "parser-s2-batch2")]
pub static CSHARP: LanguageSpec = LanguageSpec {
    language: "csharp",
    profile: "tree-sitter-c-sharp-0.23.5/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("struct_declaration", SymbolKind::Struct),
        ("enum_declaration", SymbolKind::Enum),
        ("method_declaration", SymbolKind::Function),
        ("namespace_declaration", SymbolKind::Module),
    ],
    name_field: "name",
    call_kinds: &["invocation_expression"],
    call_fields: &["function"],
    import_kinds: &["using_directive"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Scala.
#[cfg(feature = "parser-s2-batch2")]
pub static SCALA: LanguageSpec = LanguageSpec {
    language: "scala",
    profile: "tree-sitter-scala-0.25.0/ts-0.25.10",
    declarations: &[
        ("class_definition", SymbolKind::Class),
        ("object_definition", SymbolKind::Class),
        ("trait_definition", SymbolKind::Trait),
        ("function_definition", SymbolKind::Function),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Swift.
#[cfg(feature = "parser-s2-batch2")]
pub static SWIFT: LanguageSpec = LanguageSpec {
    language: "swift",
    profile: "tree-sitter-swift-0.7.1/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("struct_declaration", SymbolKind::Struct),
        ("protocol_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("function_declaration", SymbolKind::Function),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Solidity.
#[cfg(feature = "parser-s2-batch2")]
pub static SOLIDITY: LanguageSpec = LanguageSpec {
    language: "solidity",
    profile: "tree-sitter-solidity-1.2.13/ts-0.25.10",
    declarations: &[
        ("contract_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("library_declaration", SymbolKind::Module),
        ("function_definition", SymbolKind::Function),
        ("struct_declaration", SymbolKind::Struct),
        ("enum_declaration", SymbolKind::Enum),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import_directive"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

/// Kotlin.
#[cfg(feature = "parser-s2-batch2")]
pub static KOTLIN: LanguageSpec = LanguageSpec {
    language: "kotlin",
    profile: "tree-sitter-kotlin-ng-1.1.0/ts-0.25.10 (deviation from baseline 0.3.5: ABI)",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("object_declaration", SymbolKind::Class),
        ("function_declaration", SymbolKind::Function),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: false,
    form_heads: &[],
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static CLOJURE: LanguageSpec = LanguageSpec {
    language: "clojure",
    profile: "tree-sitter-clojure-0.1.0/ts-0.25.10",
    declarations: &[],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[
        ("list_lit", "ns", SymbolKind::Module),
        ("list_lit", "defn", SymbolKind::Function),
        ("list_lit", "defn-", SymbolKind::Function),
        ("list_lit", "defmacro", SymbolKind::Function),
        ("list_lit", "def", SymbolKind::Constant),
        ("list_lit", "defonce", SymbolKind::Constant),
        ("list_lit", "defprotocol", SymbolKind::Interface),
        ("list_lit", "defrecord", SymbolKind::Struct),
    ],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static CSS: LanguageSpec = LanguageSpec {
    language: "css",
    profile: "tree-sitter-css-0.25.0/ts-0.25.10",
    declarations: &[
        ("class_selector", SymbolKind::Class),
        ("id_selector", SymbolKind::Class),
        ("keyframes_statement", SymbolKind::Other),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["import_statement"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static ELIXIR: LanguageSpec = LanguageSpec {
    language: "elixir",
    profile: "tree-sitter-elixir-0.3.5/ts-0.25.10",
    declarations: &[],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[
        ("call", "defmodule", SymbolKind::Module),
        ("call", "def", SymbolKind::Function),
        ("call", "defp", SymbolKind::Function),
        ("call", "defmacro", SymbolKind::Function),
        ("call", "defstruct", SymbolKind::Struct),
        ("call", "defprotocol", SymbolKind::Interface),
    ],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static ELM: LanguageSpec = LanguageSpec {
    language: "elm",
    profile: "tree-sitter-elm-5.9.0/ts-0.25.10",
    declarations: &[
        ("module_declaration", SymbolKind::Module),
        ("value_declaration", SymbolKind::Function),
        ("type_declaration", SymbolKind::Struct),
        ("type_alias_declaration", SymbolKind::Other),
        ("port_declaration", SymbolKind::Function),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["import_clause"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static ERLANG: LanguageSpec = LanguageSpec {
    language: "erlang",
    profile: "tree-sitter-erlang-0.15.0/ts-0.25.10",
    declarations: &[
        ("fun_decl", SymbolKind::Function),
        ("module_attribute", SymbolKind::Module),
        ("record_decl", SymbolKind::Struct),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static FORTRAN: LanguageSpec = LanguageSpec {
    language: "fortran",
    profile: "tree-sitter-fortran-0.5.1/ts-0.25.10",
    declarations: &[
        ("module", SymbolKind::Module),
        ("program", SymbolKind::Module),
        ("subroutine", SymbolKind::Function),
        ("function", SymbolKind::Function),
        ("type_definition", SymbolKind::Struct),
        ("interface", SymbolKind::Interface),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static GROOVY: LanguageSpec = LanguageSpec {
    language: "groovy",
    profile: "tree-sitter-groovy-0.1.2/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("method_declaration", SymbolKind::Function),
        ("constructor_declaration", SymbolKind::Method),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static HASKELL: LanguageSpec = LanguageSpec {
    language: "haskell",
    profile: "tree-sitter-haskell-0.23.1/ts-0.25.10",
    declarations: &[
        ("function", SymbolKind::Function),
        ("header", SymbolKind::Module),
        ("data_type", SymbolKind::Struct),
        ("class_declaration", SymbolKind::Interface),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["import"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[("function", "match")],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static HCL: LanguageSpec = LanguageSpec {
    language: "hcl",
    profile: "tree-sitter-hcl-1.1.0/ts-0.25.10",
    declarations: &[
        ("block", SymbolKind::Other),
        ("attribute", SymbolKind::Field),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static JULIA: LanguageSpec = LanguageSpec {
    language: "julia",
    profile: "tree-sitter-julia-0.23.1/ts-0.25.10",
    declarations: &[
        ("module_definition", SymbolKind::Module),
        ("function_definition", SymbolKind::Function),
        ("struct_definition", SymbolKind::Struct),
        ("macro_definition", SymbolKind::Function),
        ("abstract_definition", SymbolKind::Other),
    ],
    name_field: "",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["import_statement", "using_statement"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static LUA: LanguageSpec = LanguageSpec {
    language: "lua",
    profile: "tree-sitter-lua-0.5.0/ts-0.25.10",
    declarations: &[
        ("function_declaration", SymbolKind::Function),
        ("function_definition", SymbolKind::Function),
    ],
    name_field: "name",
    call_kinds: &["function_call"],
    call_fields: &["name"],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &["body"],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static OBJC: LanguageSpec = LanguageSpec {
    language: "objc",
    profile: "tree-sitter-objc-3.0.2/ts-0.25.10",
    declarations: &[
        ("function_definition", SymbolKind::Function),
        ("class_interface", SymbolKind::Class),
        ("class_implementation", SymbolKind::Class),
        ("method_declaration", SymbolKind::Method),
        ("method_definition", SymbolKind::Method),
        ("struct_specifier", SymbolKind::Struct),
        ("enum_specifier", SymbolKind::Enum),
    ],
    name_field: "",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["preproc_include"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: true,
};

#[cfg(feature = "parser-s2-batch2")]
pub static OCAML: LanguageSpec = LanguageSpec {
    language: "ocaml",
    profile: "tree-sitter-ocaml-0.24.2/ts-0.25.10",
    declarations: &[
        ("value_definition", SymbolKind::Function),
        ("module_definition", SymbolKind::Module),
        ("type_definition", SymbolKind::Other),
        ("external", SymbolKind::Function),
    ],
    name_field: "",
    call_kinds: &["application_expression"],
    call_fields: &["function"],
    import_kinds: &["open_module", "open_statement"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static TOML: LanguageSpec = LanguageSpec {
    language: "toml",
    profile: "tree-sitter-toml-ng-0.7.0/ts-0.25.10",
    declarations: &[
        ("table", SymbolKind::Module),
        ("table_array_element", SymbolKind::Module),
        ("pair", SymbolKind::Field),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static VERILOG: LanguageSpec = LanguageSpec {
    language: "verilog",
    profile: "tree-sitter-verilog-1.0.3/ts-0.25.10",
    declarations: &[
        ("module_declaration", SymbolKind::Module),
        ("function_declaration", SymbolKind::Function),
        ("task_declaration", SymbolKind::Function),
        ("class_declaration", SymbolKind::Class),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["package_import_declaration"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch2")]
pub static YAML: LanguageSpec = LanguageSpec {
    language: "yaml",
    profile: "tree-sitter-yaml-0.7.2/ts-0.25.10",
    declarations: &[("block_mapping_pair", SymbolKind::Field)],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static COBOL: LanguageSpec = LanguageSpec {
    language: "cobol",
    profile: "arborium-cobol-2.18.2/ts-0.25.10",
    declarations: &[
        ("program_definition", SymbolKind::Module),
        ("section", SymbolKind::Other),
        ("paragraph", SymbolKind::Other),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static DART: LanguageSpec = LanguageSpec {
    language: "dart",
    profile: "tree-sitter-dart-0.1.0/ts-0.25.10",
    declarations: &[
        ("class_declaration", SymbolKind::Class),
        ("enum_declaration", SymbolKind::Enum),
        ("mixin_declaration", SymbolKind::Trait),
        ("function_signature", SymbolKind::Function),
        ("method_signature", SymbolKind::Method),
    ],
    name_field: "name",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["import_or_export"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[("function_signature", "method_signature")],
    name_skip: &[("function_signature", 1), ("method_signature", 1)],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static DOCKERFILE: LanguageSpec = LanguageSpec {
    language: "dockerfile",
    profile: "tree-sitter-containerfile-0.9.2/ts-0.25.10",
    declarations: &[("from_instruction", SymbolKind::Module)],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static PERL: LanguageSpec = LanguageSpec {
    language: "perl",
    profile: "ts-parser-perl-2.0.0/ts-0.25.10",
    declarations: &[
        ("subroutine_declaration_statement", SymbolKind::Function),
        ("package_statement", SymbolKind::Module),
    ],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &["use_statement"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static R: LanguageSpec = LanguageSpec {
    language: "r",
    profile: "tree-sitter-r-1.3.0/ts-0.25.10",
    declarations: &[],
    name_field: "",
    call_kinds: &["call"],
    call_fields: &["function"],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[
        ("binary_operator", "<-", SymbolKind::Function),
        ("binary_operator", "=", SymbolKind::Function),
        ("binary_operator", "<<-", SymbolKind::Function),
    ],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static TCL: LanguageSpec = LanguageSpec {
    language: "tcl",
    profile: "bca-tree-sitter-tcl-2.4.0/ts-0.25.10",
    declarations: &[("procedure", SymbolKind::Function)],
    name_field: "",
    call_kinds: &[],
    call_fields: &[],
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

#[cfg(feature = "parser-s2-batch4")]
pub static ZIG: LanguageSpec = LanguageSpec {
    language: "zig",
    profile: "tree-sitter-zig-1.1.2/ts-0.25.10",
    declarations: &[
        ("variable_declaration", SymbolKind::Constant),
        ("function_declaration", SymbolKind::Function),
        ("container_declaration", SymbolKind::Struct),
    ],
    name_field: "",
    call_kinds: &["call_expression"],
    call_fields: &["function"],
    import_kinds: &["builtin_function"],
    type_impl_kinds: &[],
    body_fields: &[],
    form_heads: &[],
    declaration_child_kinds: &[],
    declaration_parent_exclude: &[],
    name_skip: &[],
    name_from_descendant: true,
    declarator_names: false,
};

/// Grammar for a language this build compiled in.
pub fn grammar_for(language: &str) -> Option<tree_sitter::Language> {
    #[allow(unreachable_patterns)]
    match language {
        #[cfg(feature = "parser-rust")]
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch1")]
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch1")]
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch1")]
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch1")]
        "bash" => Some(tree_sitter_bash::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch1")]
        "c" => Some(tree_sitter_c::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch1")]
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "scala" => Some(tree_sitter_scala::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "swift" => Some(tree_sitter_swift::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "solidity" => Some(tree_sitter_solidity::LANGUAGE.into()),
        // deviation (ADR-049 O-08): the fork exposes the grammar as a
        // `tree_sitter_language::LanguageFn` const named `LANGUAGE`; the
        // baseline `tree-sitter-kotlin` 0.3.5 exposes `language()` instead.
        #[cfg(feature = "parser-s2-batch2")]
        "kotlin" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "clojure" => Some(tree_sitter_clojure::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "css" => Some(tree_sitter_css::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "elixir" => Some(tree_sitter_elixir::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "elm" => Some(tree_sitter_elm::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "erlang" => Some(tree_sitter_erlang::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "fortran" => Some(tree_sitter_fortran::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "groovy" => Some(tree_sitter_groovy::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "haskell" => Some(tree_sitter_haskell::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "hcl" => Some(tree_sitter_hcl::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "julia" => Some(tree_sitter_julia::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "lua" => Some(tree_sitter_lua::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "objc" => Some(tree_sitter_objc::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "ocaml" => Some(tree_sitter_ocaml::LANGUAGE_OCAML.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "toml" => Some(tree_sitter_toml_ng::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "verilog" => Some(tree_sitter_verilog::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch2")]
        "yaml" => Some(tree_sitter_yaml::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch4")]
        "cobol" => Some(arborium_cobol::language().into()),
        #[cfg(feature = "parser-s2-batch4")]
        "dart" => Some(tree_sitter_dart::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch4")]
        "dockerfile" => Some(tree_sitter_containerfile::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch4")]
        "perl" => Some(ts_parser_perl::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch4")]
        "r" => Some(tree_sitter_r::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch4")]
        "tcl" => Some(tree_sitter_tcl::LANGUAGE.into()),
        #[cfg(feature = "parser-s2-batch4")]
        "zig" => Some(tree_sitter_zig::LANGUAGE.into()),
        _ => None,
    }
}

/// Every spec compiled into this build.
pub fn specs() -> Vec<&'static LanguageSpec> {
    #[allow(unused_mut)]
    let mut list: Vec<&'static LanguageSpec> = Vec::new();
    #[cfg(feature = "parser-rust")]
    list.push(&RUST);
    #[cfg(feature = "parser-s2-batch1")]
    {
        list.push(&PYTHON);
        list.push(&GO);
        list.push(&JAVA);
        list.push(&BASH);
        list.push(&C);
        list.push(&CPP);
    }
    #[cfg(feature = "parser-s2-batch2")]
    {
        list.push(&TYPESCRIPT);
        list.push(&RUBY);
        list.push(&PHP);
        list.push(&CSHARP);
        list.push(&SCALA);
        list.push(&SWIFT);
        list.push(&SOLIDITY);
        list.push(&KOTLIN);
    }
    #[cfg(feature = "parser-s2-batch2")]
    {
        list.push(&CLOJURE);
        list.push(&CSS);
        list.push(&ELIXIR);
        list.push(&ELM);
        list.push(&ERLANG);
        list.push(&FORTRAN);
        list.push(&GROOVY);
        list.push(&HASKELL);
        list.push(&HCL);
        list.push(&JULIA);
        list.push(&LUA);
        list.push(&OBJC);
        list.push(&OCAML);
        list.push(&TOML);
        list.push(&VERILOG);
        list.push(&YAML);
    }
    #[cfg(feature = "parser-s2-batch4")]
    {
        list.push(&COBOL);
        list.push(&DART);
        list.push(&DOCKERFILE);
        list.push(&PERL);
        list.push(&R);
        list.push(&TCL);
        list.push(&ZIG);
    }
    list
}

/// Look up the spec for a language.
pub fn spec(language: &str) -> Option<&'static LanguageSpec> {
    specs()
        .into_iter()
        .find(|candidate| candidate.language == language)
}

/// Guards the shape of the Kotlin fork's API (ADR-049 O-08).
///
/// `tree-sitter-kotlin-ng` 1.1.0 exposes the grammar as a `LanguageFn` const named
/// `LANGUAGE`; the baseline `tree-sitter-kotlin` 0.3.5 exposed `language()`. A pin
/// bump that changes this shape now fails here with a clear message, instead of
/// surfacing as a mystery diagnostic on an unrelated spec line.
#[cfg(all(test, feature = "parser-s2-batch2"))]
mod kotlin_api_tests {
    #[test]
    fn the_kotlin_grammar_is_reachable_through_the_const() {
        let grammar: tree_sitter::Language = tree_sitter_kotlin_ng::LANGUAGE.into();
        assert!(
            grammar.node_kind_count() > 100,
            "the pinned crate must expose the real Kotlin grammar"
        );
        assert!(
            super::grammar_for("kotlin").is_some(),
            "grammar_for must resolve kotlin when parser-s2-batch2 is on"
        );
        assert_eq!(super::KOTLIN.language, "kotlin");
    }
}
