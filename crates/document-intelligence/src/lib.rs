//! Document Intelligence domain: 14 operations (DAPI-01..14).
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md` §9, §33.
//!
//! Constraints that apply to this crate from day one:
//! - MUST NOT depend on any database SDK or vendor backend crate (ADR-021).
//! - MUST NOT define a second public API registry or error enum (ADR-034, ADR-035).
//! - Capability gaps MUST be reported, never silently degraded (ADR-022, ADR-038):
//!   there is no OCR, so an image region is an `unparsed` block plus a diagnostic
//!   and the document is `partial`.

pub mod domain;
pub mod parsers;
pub mod service;

pub use domain::{
    DocumentError, DocumentSource, MediaFormat, ParsedDocument, RequirementCandidate, Result,
};
