//! Grammar lookup: every optional tree-sitter crate is referenced here and only
//! here (ADR-049).
//!
//! Keeping the optional grammars in one module means `spec.rs` is pure data with
//! no backend dependency, and a grammar pin that changes shape fails in a single,
//! obvious place instead of somewhere unrelated.

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
        assert_eq!(crate::parsers::spec::KOTLIN.language, "kotlin");
    }
}
