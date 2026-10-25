//! Tree-sitter based parsers (S2 slices).
//!
//! Each module is one migration slice. The grammar crate version MUST match the
//! locked upstream baseline so the parse substrate is identical; the extraction
//! logic is a native implementation and says so in its ledger.

#[cfg(feature = "parser-rust")]
pub mod rust;
