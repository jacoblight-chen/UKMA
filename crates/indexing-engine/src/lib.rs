//! indexing-engine — Ingestion job state machine, generation publication, projection workers.
//!
//! **STATUS: skeleton only.** No behaviour is implemented yet. Progress and
//! evidence are tracked in `docs/implementation-status.yaml`; the plan is
//! `docs/UKMA_v1.2.3_IMPLEMENTATION_PLAN.md`.
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md` §7.5, §40.4.
//!
//! Constraints that apply to this crate from day one:
//! - MUST NOT depend on any database SDK or vendor backend crate (ADR-021).
//! - MUST NOT define a second public API registry or error enum (ADR-034, ADR-035).
//! - Capability gaps MUST be reported, never silently degraded (ADR-022, ADR-038).
