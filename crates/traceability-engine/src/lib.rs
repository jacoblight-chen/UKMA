//! Traceability domain: canonical trace types (§35.1), the `TraceLink` state
//! machine and three-dimensional validity (§35.2, §40.3), coverage/impact/gap/
//! evidence analysis, and the 13 operations (TAPI-01..13).
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md`.
//!
//! Constraints that apply to this crate from day one:
//! - MUST NOT depend on any database SDK or vendor backend crate (ADR-021).
//! - MUST NOT define a second public API registry or error enum (ADR-034, ADR-035).
//! - `TraceState` is a workflow state only: historical validity and index
//!   freshness are separate dimensions and are never collapsed into it (§40.3).
//! - Capability gaps MUST be reported, never silently degraded (ADR-022): an
//!   analysis that cannot decide says `unknown`/`partial` instead of guessing.

pub mod analysis;
pub mod domain;
pub mod link;
pub mod service;
pub mod time;

pub use domain::{
    Completeness, Conclusion, CoverageReport, CurrentApplicability, EvidenceReport, GapReport,
    HistoricalValidity, IndexFreshness, Result, TraceError, TraceImpact, TraceKind, TraceLink,
    TraceState, ValidityVector,
};
pub use link::{can_transition, transition, EndpointHeads};
pub use service::{FixedClock, TraceabilityPort, TraceabilityService};
