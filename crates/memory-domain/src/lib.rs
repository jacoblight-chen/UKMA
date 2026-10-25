//! Fractal Memory domain: canonical types (§21, §34.1), FSRS/TTL (§23),
//! folding and eviction (§22), paging and reconsolidation (§23.4), agent scopes
//! (§26.4), deletion (§34) and the core operations (MAPI-01..09, 13..17, 23..25).
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md`.
//!
//! Constraints that apply to this crate from day one:
//! - MUST NOT depend on any database SDK or vendor backend crate (ADR-021).
//! - MUST NOT define a second public API registry or error enum (ADR-034, ADR-035).
//! - The context budget and the folding/eviction/paging algorithms come from
//!   `context-engine`; this crate never carries a second implementation.
//! - Capability gaps MUST be reported, never silently degraded (ADR-022): the
//!   advanced operations of §24/§27 answer `CAPABILITY_UNAVAILABLE` until P6.

pub mod deletion;
pub mod domain;
pub mod episodic;
pub mod fsrs;
pub mod reconsolidation;
pub mod scope;
pub mod search;
pub mod service;
pub mod time;

pub use domain::{
    BudgetView, ChangeReceipt, DeletionReceipt, EpisodeOutcome, EpisodeState, EpisodeType,
    EvictionPlan, FoldedResult, MemoryError, MemoryKind, MemoryPatch, MemoryQuery, MemoryView,
    Page, Ref, Result, Trust, WorkingState,
};
pub use episodic::{Episode, SweepAction, SweepDecision};
pub use fsrs::{FsrsState, RetentionGate};
pub use reconsolidation::{MemoryRecord, MemoryRevision, Window};
pub use service::{FixedClock, FractalMemoryPort, MemoryService, SystemClock};
