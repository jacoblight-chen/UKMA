//! Rust parser (S2-rust).
//!
//! The generic engine ([`super::engine::TreeSitterParser`]) specialised with the
//! Rust spec ([`super::spec::RUST`]). Keeping this thin wrapper means the golden
//! parity test keeps guarding the same public surface it always did.

use super::engine::TreeSitterParser;
use super::spec;
use crate::{CodeAnalysisManifest, CodeResult, ParsedSource, ParserPort, SupportLevel};

/// Rust parser bound to the pinned grammar.
pub struct RustParser {
    inner: TreeSitterParser,
}

impl Default for RustParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RustParser {
    /// New parser with the pinned profile.
    pub fn new() -> Self {
        Self {
            inner: TreeSitterParser::new(&spec::RUST),
        }
    }

    /// Parser profile string.
    pub fn profile(&self) -> &str {
        self.inner.profile()
    }

    /// Build an analysis manifest for a parsed source.
    pub fn manifest(
        &self,
        repository_id: &str,
        commit_sha: &str,
        generation: &str,
        parsed: &ParsedSource,
    ) -> CodeAnalysisManifest {
        self.inner
            .manifest(repository_id, commit_sha, generation, parsed)
    }
}

impl ParserPort for RustParser {
    fn support(&self, language: &str) -> SupportLevel {
        self.inner.support(language)
    }

    fn parse(&self, path: &str, language: &str, source: &str) -> CodeResult<ParsedSource> {
        self.inner.parse(path, language, source)
    }
}
