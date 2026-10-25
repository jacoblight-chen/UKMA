//! The single context-budget algorithm (§10.2 = §22.1 = §40.2, ADR-033).
//!
//! There MUST be exactly one implementation of this maths in the workspace;
//! no other crate may recompute or second-guess the reserves.

/// Floor of the usable budget; below this an agent must not start (§10.2).
pub const B_FLOOR: u32 = 4096;
/// Base cap used before depth scaling (§10.2).
pub const B_BASE_CAP: u32 = 131_072;
/// Depth divisor (§10.2: `4^d`).
pub const DEPTH_FACTOR: u32 = 4;

/// Budget failure (§31.3 closed ErrorCode set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    /// `C_usable < B_FLOOR`; agent creation/spawn must be refused.
    InsufficientContextBudget {
        /// The computed usable budget (may be negative).
        c_usable: i64,
    },
    /// A single operation exceeded an already allocated budget.
    ContextBudgetExceeded {
        /// Allocated tokens.
        limit: u32,
        /// Requested tokens.
        requested: u32,
    },
    /// Depth zero or negative reserves were passed.
    InvalidArgument {
        /// Offending field.
        field: &'static str,
        /// Why it is invalid.
        reason: String,
    },
}

impl BudgetError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            BudgetError::InsufficientContextBudget { .. } => "INSUFFICIENT_CONTEXT_BUDGET",
            BudgetError::ContextBudgetExceeded { .. } => "CONTEXT_BUDGET_EXCEEDED",
            BudgetError::InvalidArgument { .. } => "INVALID_ARGUMENT",
        }
    }
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetError::InsufficientContextBudget { c_usable } => {
                write!(
                    f,
                    "insufficient context budget: C_usable = {c_usable} < {B_FLOOR}"
                )
            }
            BudgetError::ContextBudgetExceeded { limit, requested } => {
                write!(f, "context budget exceeded: {requested} > {limit}")
            }
            BudgetError::InvalidArgument { field, reason } => write!(f, "{field}: {reason}"),
        }
    }
}

impl std::error::Error for BudgetError {}

/// Result alias for budget computations.
pub type BudgetResult<T> = Result<T, BudgetError>;

/// Token reserves taken from the model window (§10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reserves {
    /// Actual model context window in tokens.
    pub w_model: u32,
    /// System prompt + tool schemas + safety reserve (already includes all of them).
    pub r_system: u32,
    /// Output reserve.
    pub r_output: u32,
    /// Other runtime reserve.
    pub r_runtime: u32,
}

impl Reserves {
    /// Build reserves, rejecting nothing at construction time (validation is in
    /// [`c_usable`] so the error is reported by code, not by panic).
    pub fn new(w_model: u32, r_system: u32, r_output: u32, r_runtime: u32) -> Self {
        Self {
            w_model,
            r_system,
            r_output,
            r_runtime,
        }
    }

    /// `C_usable = W_model - R_system - R_output - R_runtime` (§22.1).
    ///
    /// `R_system` already contains every system/tool/security token, so it MUST
    /// NOT be deducted twice (ADR-033).
    pub fn c_usable(&self) -> BudgetResult<u32> {
        let reserves =
            i64::from(self.r_system) + i64::from(self.r_output) + i64::from(self.r_runtime);
        let usable = i64::from(self.w_model) - reserves;
        if usable < i64::from(B_FLOOR) {
            return Err(BudgetError::InsufficientContextBudget { c_usable: usable });
        }
        Ok(usable as u32)
    }
}

/// Effective input budget at agent depth `d` (§10.2):
/// `B_eff(d) = min(C_usable, max(B_floor, floor(min(C_usable, B_base_cap) / 4^d)))`.
pub fn b_eff(c_usable: u32, depth: u32) -> BudgetResult<u32> {
    if c_usable < B_FLOOR {
        return Err(BudgetError::InsufficientContextBudget {
            c_usable: i64::from(c_usable),
        });
    }
    let base = c_usable.min(B_BASE_CAP) as u64;
    let divisor = (DEPTH_FACTOR as u64).saturating_pow(depth);
    let scaled = base / divisor;
    let floored = scaled.max(u64::from(B_FLOOR));
    let effective = (floored as u32).min(c_usable);
    Ok(effective)
}

/// How the effective budget is split (§22.1 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// Working-memory share.
    pub wm: u32,
    /// Core-knowledge share.
    pub core: u32,
    /// Message-ring share (absorbs the rounding remainder).
    pub ring: u32,
}

impl Allocation {
    /// Sum of the shares; always exactly `B_eff` (§22.1).
    pub fn total(&self) -> u32 {
        self.wm + self.core + self.ring
    }
}

/// Per-share percentages `(wm, core, ring)` by depth bucket (§22.1).
pub fn shares(depth: u32) -> (u32, u32, u32) {
    match depth {
        0 => (60, 25, 15),
        1 | 2 => (56, 25, 19),
        _ => (55, 25, 20),
    }
}

/// Split `B_eff` per the §22.1 table: each share is floored and the remainder
/// goes to the message ring, so the three parts always sum to `B_eff`.
pub fn allocate(b_eff: u32, depth: u32) -> Allocation {
    let (wm_pct, core_pct, _ring_pct) = shares(depth);
    let wm = b_eff * wm_pct / 100;
    let core = b_eff * core_pct / 100;
    let ring = b_eff - wm - core;
    Allocation { wm, core, ring }
}

/// Compression thresholds (percent utilisation) by depth (§22.1).
pub fn compression_thresholds(depth: u32) -> &'static [u8] {
    match depth {
        0 => &[80, 85, 90, 93, 95],
        1 => &[75, 82, 88, 92, 95],
        2 => &[70, 78, 85, 90, 95],
        _ => &[65, 75, 82, 88, 95],
    }
}

/// The five compression stages in mandatory order (§22.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompressionStage {
    /// 1. Ring safe replacement.
    RingReplacement,
    /// 2. Episode full text -> summary.
    EpisodeSummary,
    /// 3. Summary -> facts.
    Facts,
    /// 4. Low-weight block folding.
    BlockFolding,
    /// 5. Policy-gated eviction.
    PolicyEviction,
}

/// Next compression stage for a utilisation percentage, if any (§22.1).
pub fn next_compression_stage(utilisation: u8, depth: u32) -> Option<CompressionStage> {
    let thresholds = compression_thresholds(depth);
    let stages = [
        CompressionStage::RingReplacement,
        CompressionStage::EpisodeSummary,
        CompressionStage::Facts,
        CompressionStage::BlockFolding,
        CompressionStage::PolicyEviction,
    ];
    thresholds
        .iter()
        .rposition(|limit| utilisation >= *limit)
        .map(|index| stages[index])
}

/// Check a single operation against an allocated share (§10.2).
pub fn check_allocation(limit: u32, requested: u32) -> BudgetResult<()> {
    if requested > limit {
        return Err(BudgetError::ContextBudgetExceeded { limit, requested });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §40.2 deterministic vectors: `(C_usable, depth, expected B_eff)`.
    #[test]
    fn golden_depth_vectors_from_spec_40_2() {
        let cases = [
            (65_536u32, 0u32, Some(65_536u32)),
            (65_536, 1, Some(16_384)),
            (65_536, 2, Some(4_096)),
            (131_072, 1, Some(32_768)),
            (32_768, 3, Some(4_096)),
            (4_095, 0, None), // INSUFFICIENT_CONTEXT_BUDGET
        ];
        for (c_usable, depth, expected) in cases {
            let outcome = b_eff(c_usable, depth);
            match expected {
                Some(value) => assert_eq!(
                    outcome.unwrap(),
                    value,
                    "B_eff({c_usable}, depth {depth}) must be {value}"
                ),
                None => {
                    let error = outcome.unwrap_err();
                    assert_eq!(error.code(), "INSUFFICIENT_CONTEXT_BUDGET");
                }
            }
        }
    }

    #[test]
    fn b_eff_is_monotonic_in_depth_and_never_exceeds_c_usable() {
        for depth in 0..8u32 {
            let effective = b_eff(100_000, depth).unwrap();
            assert!(effective <= 100_000);
            if depth > 0 {
                assert!(effective <= b_eff(100_000, depth - 1).unwrap());
            }
        }
    }

    #[test]
    fn reserves_are_not_deducted_twice() {
        // R_system already contains tool + security tokens (ADR-033).
        let reserves = Reserves::new(200_000, 8_000, 4_000, 2_000);
        assert_eq!(reserves.c_usable().unwrap(), 186_000);

        let too_small = Reserves::new(20_000, 8_000, 4_000, 8_100);
        let error = too_small.c_usable().unwrap_err();
        assert_eq!(error.code(), "INSUFFICIENT_CONTEXT_BUDGET");
        assert!(error.to_string().contains("-100"));
    }

    #[test]
    fn allocation_shares_sum_exactly_to_b_eff() {
        for depth in 0..6u32 {
            let effective = b_eff(131_072, depth).unwrap();
            let allocation = allocate(effective, depth);
            assert_eq!(
                allocation.total(),
                effective,
                "shares must sum to B_eff at depth {depth}"
            );
            let (wm_pct, _, _) = shares(depth);
            assert_eq!(allocation.wm, effective * wm_pct / 100);
        }
    }

    #[test]
    fn zero_and_negative_reserves_are_rejected() {
        let zero_window = Reserves::new(0, 0, 0, 0);
        assert_eq!(
            zero_window.c_usable().unwrap_err().code(),
            "INSUFFICIENT_CONTEXT_BUDGET"
        );
        let negative = Reserves::new(1_000, 2_000, 0, 0);
        assert!(negative.c_usable().is_err());
    }

    #[test]
    fn compression_stages_escalate_in_order() {
        assert_eq!(next_compression_stage(70, 0), None);
        assert_eq!(
            next_compression_stage(80, 0),
            Some(CompressionStage::RingReplacement)
        );
        assert_eq!(
            next_compression_stage(95, 0),
            Some(CompressionStage::PolicyEviction)
        );
        // depth 3 starts compressing earlier
        assert_eq!(
            next_compression_stage(65, 3),
            Some(CompressionStage::RingReplacement)
        );
    }

    #[test]
    fn per_operation_overrun_is_reported() {
        assert!(check_allocation(1000, 1000).is_ok());
        let error = check_allocation(1000, 1001).unwrap_err();
        assert_eq!(error.code(), "CONTEXT_BUDGET_EXCEEDED");
    }
}
