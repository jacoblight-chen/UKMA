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
    /// Fields that may hold the callee of a call node, tried in order.
    pub call_fields: &'static [&'static str],
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
    call_fields: &["function"],
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
    call_fields: &["function"],
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
    call_fields: &["function"],
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
    call_fields: &["name"],
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
    call_fields: &["name"],
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
    call_fields: &["function"],
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
    call_fields: &["function"],
    import_kinds: &["preproc_include"],
    type_impl_kinds: &[],
    body_fields: &["body"],
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
    grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
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
    declarator_names: false,
};

/// Ruby.
#[cfg(feature = "parser-s2-batch2")]
pub static RUBY: LanguageSpec = LanguageSpec {
    language: "ruby",
    profile: "tree-sitter-ruby-0.23.1/ts-0.25.10",
    grammar: || tree_sitter_ruby::LANGUAGE.into(),
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
    declarator_names: false,
};

/// PHP.
#[cfg(feature = "parser-s2-batch2")]
pub static PHP: LanguageSpec = LanguageSpec {
    language: "php",
    profile: "tree-sitter-php-0.24.2/ts-0.25.10",
    grammar: || tree_sitter_php::LANGUAGE_PHP.into(),
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
    declarator_names: false,
};

/// C#.
#[cfg(feature = "parser-s2-batch2")]
pub static CSHARP: LanguageSpec = LanguageSpec {
    language: "csharp",
    profile: "tree-sitter-c-sharp-0.23.5/ts-0.25.10",
    grammar: || tree_sitter_c_sharp::LANGUAGE.into(),
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
    declarator_names: false,
};

/// Scala.
#[cfg(feature = "parser-s2-batch2")]
pub static SCALA: LanguageSpec = LanguageSpec {
    language: "scala",
    profile: "tree-sitter-scala-0.25.0/ts-0.25.10",
    grammar: || tree_sitter_scala::LANGUAGE.into(),
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
    declarator_names: false,
};

/// Swift.
#[cfg(feature = "parser-s2-batch2")]
pub static SWIFT: LanguageSpec = LanguageSpec {
    language: "swift",
    profile: "tree-sitter-swift-0.7.1/ts-0.25.10",
    grammar: || tree_sitter_swift::LANGUAGE.into(),
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
    declarator_names: false,
};

/// Solidity.
#[cfg(feature = "parser-s2-batch2")]
pub static SOLIDITY: LanguageSpec = LanguageSpec {
    language: "solidity",
    profile: "tree-sitter-solidity-1.2.13/ts-0.25.10",
    grammar: || tree_sitter_solidity::LANGUAGE.into(),
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
    declarator_names: false,
};

/// Kotlin.
#[cfg(feature = "parser-s2-batch2")]
pub static KOTLIN: LanguageSpec = LanguageSpec {
    language: "kotlin",
    profile: "tree-sitter-kotlin-ng-1.1.0/ts-0.25.10 (deviation from baseline 0.3.5: ABI)",
    grammar: || tree_sitter_kotlin_ng::LANGUAGE.into(),
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
    declarator_names: false,
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
    list
}

/// Look up the spec for a language.
pub fn spec(language: &str) -> Option<&'static LanguageSpec> {
    specs()
        .into_iter()
        .find(|candidate| candidate.language == language)
}
