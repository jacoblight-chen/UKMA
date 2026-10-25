//! Tree-sitter based parsers (S2 slices).
//!
//! One generic engine ([`engine`]) plus one declarative spec per language
//! ([`spec`]). Grammar crate versions are pinned in `Cargo.toml`; for the
//! languages that exist in the locked upstream `Cargo.lock` the pin is exactly
//! the baseline version.

pub mod engine;
pub mod spec;

#[cfg(feature = "parser-rust")]
pub mod rust;
