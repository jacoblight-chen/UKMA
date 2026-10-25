//! Context engine: the single budget algorithm, folding, eviction and paging.
//!
//! `FO-001`..`FO-005`, `CT-001`..`CT-004` and the §40.2 deterministic budget
//! vectors are covered by the tests in each module.
//!
//! Nothing here mutates source artifacts: folding and eviction only ever touch
//! working/episodic context (`PROP-05`).

pub mod budget;
pub mod eviction;
pub mod folding;
pub mod paging;

pub use budget::{
    allocate, b_eff, check_allocation, compression_thresholds, next_compression_stage, shares,
    Allocation, BudgetError, BudgetResult, CompressionStage, Reserves, B_BASE_CAP, B_FLOOR,
    DEPTH_FACTOR,
};
pub use eviction::{
    Candidate, DeterministicPolicy, EvictionPolicy, RankedEvictions, ShadowRecord, ShadowRlPolicy,
};
pub use folding::{
    classify, decide, trust_after_folding, FallbackReason, Feature, FeatureVector, FoldDecision,
    FoldLevel, PreserveRule, FEATURE_COUNT, FOLD_HIGH, FOLD_LOW, FOLD_TRUST_KAPPA,
};
pub use paging::{checksum, page_in, page_out, OpenedPage, PageError, PageRecord};
