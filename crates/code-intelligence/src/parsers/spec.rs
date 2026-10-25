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
    /// Grammar constructor.
    pub grammar: fn() -> tree_sitter::Language,
    /// Declaration node kind -> symbol kind.
    pub declarations: &'static [(&'static str, SymbolKind)],
    /// Field that holds a declaration's name.
    pub name_field: &'static str,
    /// Node kinds that represent a call.
    pub call_kinds: &'static [&'static str],
    /// Field that holds the callee of a call node.
    pub call_field: &'static str,
    /// Node kinds that represent an import/include.
    pub import_kinds: &'static [&'static str],
    /// Transparent scopes whose members belong to the type named in `type`.
    pub type_impl_kinds: &'static [&'static str],
    /// Fields that may hold a body (used to cut the signature).
    pub body_fields: &'static [&'static str],
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
    grammar: || tree_sitter_rust::LANGUAGE.into(),
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
    call_field: "function",
    import_kinds: &["use_declaration"],
    type_impl_kinds: &["impl_item"],
    body_fields: &["body"],
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
    grammar: || tree_sitter_python::LANGUAGE.into(),
    declarations: &[
        ("class_definition", SymbolKind::Class),
        ("function_definition", SymbolKind::Function),
        ("decorated_definition", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call"],
    call_field: "function",
    import_kinds: &["import_statement", "import_from_statement"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declarator_names: false,
};

/// Go.
#[cfg(feature = "parser-s2-batch1")]
pub static GO: LanguageSpec = LanguageSpec {
    language: "go",
    profile: "tree-sitter-go-0.25.0/ts-0.25.10",
    grammar: || tree_sitter_go::LANGUAGE.into(),
    declarations: &[
        ("function_declaration", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("type_spec", SymbolKind::Struct),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_field: "function",
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declarator_names: false,
};

/// Java.
#[cfg(feature = "parser-s2-batch1")]
pub static JAVA: LanguageSpec = LanguageSpec {
    language: "java",
    profile: "tree-sitter-java-0.23.5/ts-0.25.10",
    grammar: || tree_sitter_java::LANGUAGE.into(),
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
    call_field: "name",
    import_kinds: &["import_declaration"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declarator_names: false,
};

/// Bash.
#[cfg(feature = "parser-s2-batch1")]
pub static BASH: LanguageSpec = LanguageSpec {
    language: "bash",
    profile: "tree-sitter-bash-0.25.1/ts-0.25.10",
    grammar: || tree_sitter_bash::LANGUAGE.into(),
    declarations: &[
        ("function_definition", SymbolKind::Function),
        ("variable_assignment", SymbolKind::Variable),
    ],
    name_field: "name",
    // Bash has no static call graph: a command is a lookup by name at run time,
    // so the best a call edge may claim is `static_possible`.
    call_kinds: &["command"],
    call_field: "name",
    import_kinds: &[],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declarator_names: false,
};

/// C.
#[cfg(feature = "parser-s2-batch1")]
pub static C: LanguageSpec = LanguageSpec {
    language: "c",
    profile: "tree-sitter-c-0.24.1/ts-0.25.10",
    grammar: || tree_sitter_c::LANGUAGE.into(),
    declarations: &[
        ("function_definition", SymbolKind::Function),
        ("struct_specifier", SymbolKind::Struct),
        ("union_specifier", SymbolKind::Struct),
        ("enum_specifier", SymbolKind::Enum),
        ("type_definition", SymbolKind::Other),
    ],
    name_field: "name",
    call_kinds: &["call_expression"],
    call_field: "function",
    import_kinds: &["preproc_include"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declarator_names: true,
};

/// C++.
#[cfg(feature = "parser-s2-batch1")]
pub static CPP: LanguageSpec = LanguageSpec {
    language: "cpp",
    profile: "tree-sitter-cpp-0.23.4/ts-0.25.10",
    grammar: || tree_sitter_cpp::LANGUAGE.into(),
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
    call_field: "function",
    import_kinds: &["preproc_include"],
    type_impl_kinds: &[],
    body_fields: &["body"],
    declarator_names: true,
};

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
    list
}

/// Look up the spec for a language.
pub fn spec(language: &str) -> Option<&'static LanguageSpec> {
    specs()
        .into_iter()
        .find(|candidate| candidate.language == language)
}
