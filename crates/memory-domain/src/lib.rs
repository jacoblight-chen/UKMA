//! Fractal Memory domain: canonical types (§21, §34.1), FSRS/TTL (§23),
//! folding and eviction (§22), paging and reconsolidation (§23.4), agent scopes
//! (§26.4), trust (§26.1), facts and conflicts (§25.4, §27.1), the nine-stage
//! consolidation pipeline (§27.1), procedures (§27.2), poisoning defence
//! (§26.3), multilingual gates (§25), maintenance jobs (§27.3), entropy
//! summarisation (§23.3), deletion (§34) and the 25 memory operations.
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md`.
//!
//! Constraints that apply to this crate from day one:
//! - MUST NOT depend on any database SDK or vendor backend crate (ADR-021).
//! - MUST NOT define a second public API registry or error enum (ADR-034, ADR-035).
//! - The context budget and the folding/eviction/paging algorithms come from
//!   `context-engine`; BM25, cosine and RRF come from `retrieval-engine`. This
//!   crate never carries a second implementation of any of them.
//! - Capability gaps MUST be reported, never silently degraded (ADR-022).

pub mod consolidation;
pub mod deletion;
pub mod domain;
pub mod entropy;
pub mod episodic;
pub mod facts;
pub mod fsrs;
pub mod jobs;
pub mod language;
pub mod poisoning;
pub mod procedures;
pub mod reconsolidation;
pub mod scope;
pub mod search;
pub mod service;
pub mod time;
pub mod trust;

pub use domain::{
    BudgetView, ChangeReceipt, DeletionReceipt, EpisodeOutcome, EpisodeState, EpisodeType,
    EvictionPlan, FoldedResult, MemoryError, MemoryKind, MemoryPatch, MemoryQuery, MemoryView,
    Page, ProcedureState, ProcedureView, Ref, Result, Trust, WorkingState,
};
pub use episodic::{Episode, SweepAction, SweepDecision};
pub use facts::{ClaimRelation, PromotionGate, SemanticFact};
pub use fsrs::{FsrsState, RetentionGate};
pub use procedures::{Procedure, ProcedureReview};
pub use reconsolidation::{MemoryRecord, MemoryRevision, Window};
pub use service::{FixedClock, FractalMemoryPort, MemoryService, SystemClock};
pub use trust::{AuthzDecision, BetaState, KalmanState, TrustColumns, TrustRank};
