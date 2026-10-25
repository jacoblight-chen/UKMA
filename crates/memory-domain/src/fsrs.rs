//! FSRS retrievability, active TTL and the retention gates (§23.1–§23.3).
//!
//! Every formula here is the registered one; nothing is approximated. `R` is a
//! retrievability heuristic and is never treated as truth or authorization.

use crate::time::{elapsed_days_checked, TimeError};

/// Initial stability in days (§23.1 `S0=2.0 days`).
pub const S0_DAYS: f64 = 2.0;
/// Initial difficulty (§23.1 `D0=5.0`).
pub const D0: f64 = 5.0;
/// Difficulty bounds (§23.1 `1..10`).
pub const D_MIN: f64 = 1.0;
/// Difficulty bounds (§23.1 `1..10`).
pub const D_MAX: f64 = 10.0;
/// Stability floor in days (§23.1 `S_min>0`, profile default `0.1 day`).
pub const S_MIN_DAYS: f64 = 0.1;
/// Stability ceiling in days (§23.1 `3650`).
pub const S_MAX_DAYS: f64 = 3_650.0;
/// Scheduling reference only (§23.1 `request_retention=0.9`).
pub const TARGET_RETENTION: f64 = 0.9;
/// Delete-candidate threshold: `R < 0.3` (§23.1).
pub const RETENTION_DELETE_BELOW: f64 = 0.3;
/// Compress-candidate threshold: `R >= 0.7` is usually kept (§23.1).
pub const RETENTION_KEEP_FROM: f64 = 0.7;
/// Active TTL at depth 0, in hours (§23.2 `72hours/2^d`).
pub const ACTIVE_TTL_BASE_HOURS: f64 = 72.0;
/// Active TTL floor, in hours (§23.2 `max(1hour, ...)`).
pub const ACTIVE_TTL_FLOOR_HOURS: f64 = 1.0;
/// Sub-agent follow-up window after completion (§23.2 `quick_followup_window=1hour`).
pub const QUICK_FOLLOWUP_HOURS: f64 = 1.0;

/// Why an FSRS computation was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum FsrsError {
    /// `t`, `S` or `D` was NaN/infinite.
    NonFinite {
        /// Offending field.
        field: &'static str,
    },
    /// A stability of exactly zero would divide by zero.
    ZeroStability,
    /// A negative stability or elapsed time is not a valid input.
    Negative {
        /// Offending field.
        field: &'static str,
        /// Offending value.
        value: f64,
    },
    /// A value outside its registered range.
    OutOfRange {
        /// Offending field.
        field: &'static str,
        /// Offending value.
        value: f64,
    },
    /// The elapsed interval points into the future.
    TimeSkew(TimeError),
}

impl FsrsError {
    /// Registered error code.
    pub fn code(&self) -> &'static str {
        match self {
            FsrsError::NonFinite { .. }
            | FsrsError::ZeroStability
            | FsrsError::Negative { .. }
            | FsrsError::OutOfRange { .. }
            | FsrsError::TimeSkew(_) => "INVALID_ARGUMENT",
        }
    }
}

impl std::fmt::Display for FsrsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsrsError::NonFinite { field } => write!(f, "{field} must be finite"),
            FsrsError::ZeroStability => write!(f, "stability must be greater than zero"),
            FsrsError::Negative { field, value } => {
                write!(f, "{field} must be >= 0, got {value}")
            }
            FsrsError::OutOfRange { field, value } => {
                write!(f, "{field} is out of range: {value}")
            }
            FsrsError::TimeSkew(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FsrsError {}

/// What retention semantics require for a record (§23.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionGate {
    /// `R < 0.3`: candidate for deletion (still subject to the purge guards).
    DeleteCandidate,
    /// `0.3 <= R < 0.7`: candidate for compression.
    CompressCandidate,
    /// `R >= 0.7`: usually kept.
    Keep,
}

impl RetentionGate {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            RetentionGate::DeleteCandidate => "delete_candidate",
            RetentionGate::CompressCandidate => "compress_candidate",
            RetentionGate::Keep => "keep",
        }
    }
}

/// Durable FSRS state of one record (§21.3 `fsrs_state`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FsrsState {
    /// Stability in days.
    pub stability_days: f64,
    /// Difficulty in `[1,10]`.
    pub difficulty: f64,
    /// Number of reviews that produced this state.
    pub reviews: u32,
}

impl Default for FsrsState {
    fn default() -> Self {
        Self {
            stability_days: S0_DAYS,
            difficulty: D0,
            reviews: 0,
        }
    }
}

impl FsrsState {
    /// State for a record that has never been reviewed, with an explicit
    /// starting stability and difficulty.
    pub fn new(stability_days: f64, difficulty: f64) -> Result<Self, FsrsError> {
        let state = Self {
            stability_days,
            difficulty,
            reviews: 0,
        };
        state.validate()?;
        Ok(state)
    }

    /// Validate the invariants (`S>0`, `D` in `[1,10]`, finite).
    pub fn validate(&self) -> Result<(), FsrsError> {
        if !self.stability_days.is_finite() {
            return Err(FsrsError::NonFinite {
                field: "stability_days",
            });
        }
        if self.stability_days <= 0.0 {
            return Err(FsrsError::ZeroStability);
        }
        if !self.difficulty.is_finite() {
            return Err(FsrsError::NonFinite {
                field: "difficulty",
            });
        }
        if !(D_MIN..=D_MAX).contains(&self.difficulty) {
            return Err(FsrsError::OutOfRange {
                field: "difficulty",
                value: self.difficulty,
            });
        }
        Ok(())
    }
}

/// `R(t) = [1 + (19/9) * (t/S)]^-1` (§23.1).
pub fn retrievability(elapsed_days: f64, stability_days: f64) -> Result<f64, FsrsError> {
    if !elapsed_days.is_finite() {
        return Err(FsrsError::NonFinite {
            field: "elapsed_days",
        });
    }
    if !stability_days.is_finite() {
        return Err(FsrsError::NonFinite {
            field: "stability_days",
        });
    }
    if elapsed_days < 0.0 {
        return Err(FsrsError::Negative {
            field: "elapsed_days",
            value: elapsed_days,
        });
    }
    if stability_days <= 0.0 {
        return Err(FsrsError::ZeroStability);
    }
    Ok(1.0 / (1.0 + (19.0 / 9.0) * (elapsed_days / stability_days)))
}

/// Retrievability between two instants, refusing skew (§23.1 `t>=0`).
pub fn retrievability_at(
    last_review: i64,
    now: i64,
    stability_days: f64,
) -> Result<f64, FsrsError> {
    let elapsed = elapsed_days_checked(last_review, now).map_err(FsrsError::TimeSkew)?;
    retrievability(elapsed, stability_days)
}

/// Stability after a successful retrieval (§23.1).
///
/// `S_new = min(S * [1 + 0.5*(11-D)*(exp(1.5*(1-R)) - 1)], 3650)`.
pub fn on_success(state: FsrsState, retrievability: f64) -> Result<FsrsState, FsrsError> {
    state.validate()?;
    if !retrievability.is_finite() {
        return Err(FsrsError::NonFinite {
            field: "retrievability",
        });
    }
    if !(0.0..=1.0).contains(&retrievability) {
        return Err(FsrsError::OutOfRange {
            field: "retrievability",
            value: retrievability,
        });
    }
    let gain = 1.0 + 0.5 * (11.0 - state.difficulty) * ((1.5 * (1.0 - retrievability)).exp() - 1.0);
    let stability = (state.stability_days * gain).clamp(S_MIN_DAYS, S_MAX_DAYS);
    Ok(FsrsState {
        stability_days: stability,
        difficulty: state.difficulty,
        reviews: state.reviews + 1,
    })
}

/// Stability after a failed retrieval (§23.1): `S_new = max(S_min, S*0.5)`.
pub fn on_failure(state: FsrsState) -> Result<FsrsState, FsrsError> {
    state.validate()?;
    Ok(FsrsState {
        stability_days: (state.stability_days * 0.5).max(S_MIN_DAYS),
        difficulty: state.difficulty,
        reviews: state.reviews + 1,
    })
}

/// Retention gate for a retrievability value (§23.1).
pub fn retention_gate(retrievability: f64) -> Result<RetentionGate, FsrsError> {
    if !retrievability.is_finite() || !(0.0..=1.0).contains(&retrievability) {
        return Err(FsrsError::OutOfRange {
            field: "retrievability",
            value: retrievability,
        });
    }
    if retrievability < RETENTION_DELETE_BELOW {
        Ok(RetentionGate::DeleteCandidate)
    } else if retrievability < RETENTION_KEEP_FROM {
        Ok(RetentionGate::CompressCandidate)
    } else {
        Ok(RetentionGate::Keep)
    }
}

/// Active TTL in hours at a delegation depth (§23.2 `max(1h, 72h/2^d)`).
pub fn active_ttl_hours(depth: u32) -> f64 {
    let divisor = 2f64.powi(i32::try_from(depth).unwrap_or(i32::MAX));
    (ACTIVE_TTL_BASE_HOURS / divisor).max(ACTIVE_TTL_FLOOR_HOURS)
}

/// `effective_active_until = min(creation + T_active(d), completion + 1h)` (§23.2).
///
/// An unfinished task has no completion instant, so only the creation horizon
/// applies; the caller must archive rather than purge in that case (§23.2).
pub fn effective_active_until(created_at: i64, completed_at: Option<i64>, depth: u32) -> i64 {
    let ttl = active_ttl_hours(depth);
    let from_creation = crate::time::add_hours(created_at, ttl);
    match completed_at {
        Some(completed) => crate::time::earliest(
            from_creation,
            crate::time::add_hours(completed, QUICK_FOLLOWUP_HOURS),
        ),
        None => from_creation,
    }
}

/// `T_fold = T_orig * exp(-0.15 * fold_level)` (§22.2), bounded to `[0,1]`.
pub fn folded_trust(original: f64, fold_level: u32) -> f64 {
    context_engine::trust_after_folding(original, fold_level)
}

/// Entropy signal of one payload (§23.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntropySignal {
    /// Nothing to measure: `raw_bytes_len = 0` -> `SKIPPED_EMPTY`.
    SkippedEmpty,
    /// `gzip_ratio` of the payload.
    Ratio(f64),
}

impl EntropySignal {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            EntropySignal::SkippedEmpty => "SKIPPED_EMPTY",
            EntropySignal::Ratio(_) => "MEASURED",
        }
    }

    /// Whether the payload is redundant enough to be a summarisation candidate.
    pub fn is_redundant(self) -> bool {
        match self {
            EntropySignal::SkippedEmpty => false,
            EntropySignal::Ratio(ratio) => ratio < ENTROPY_GZIP_THRESHOLD,
        }
    }
}

/// Redundancy threshold (§29.2 `gzip_ratio_threshold=.4`).
pub const ENTROPY_GZIP_THRESHOLD: f64 = 0.4;

/// Compute the entropy signal of a payload.
///
/// `gzip_ratio = gzip_size(raw_bytes) / raw_bytes_len` exactly as §23.3 defines
/// it. The encoder settings are fixed (default gzip level, zero mtime) so the
/// ratio is reproducible for the same bytes.
pub fn entropy_signal(payload: &[u8]) -> EntropySignal {
    if payload.is_empty() {
        return EntropySignal::SkippedEmpty;
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    if std::io::Write::write_all(&mut encoder, payload).is_err() {
        return EntropySignal::SkippedEmpty;
    }
    match encoder.finish() {
        Ok(compressed) => EntropySignal::Ratio(compressed.len() as f64 / payload.len() as f64),
        Err(_) => EntropySignal::SkippedEmpty,
    }
}

/// Whether a record is a summarisation candidate: redundant and mid-retention
/// (§23.3: `0.3 <= R < 0.7` and `gzip_ratio < 0.4`).
pub fn is_compression_candidate(retrievability: f64, signal: EntropySignal) -> bool {
    let mid = (RETENTION_DELETE_BELOW..RETENTION_KEEP_FROM).contains(&retrievability);
    mid && signal.is_redundant()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrievability_matches_the_registered_formula() {
        // R(0) = 1
        assert!((retrievability(0.0, S0_DAYS).unwrap() - 1.0).abs() < 1e-12);
        // R(S) = 1 / (1 + 19/9)
        let at_stability = retrievability(S0_DAYS, S0_DAYS).unwrap();
        assert!((at_stability - 1.0 / (1.0 + 19.0 / 9.0)).abs() < 1e-12);
        // R(2*S) is strictly smaller
        assert!(retrievability(2.0 * S0_DAYS, S0_DAYS).unwrap() < at_stability);
    }

    #[test]
    fn retrievability_refuses_invalid_inputs() {
        assert_eq!(
            retrievability(0.0, 0.0).unwrap_err(),
            FsrsError::ZeroStability
        );
        assert!(matches!(
            retrievability(-1.0, 1.0).unwrap_err(),
            FsrsError::Negative { .. }
        ));
        assert!(matches!(
            retrievability(f64::NAN, 1.0).unwrap_err(),
            FsrsError::NonFinite { .. }
        ));
        let skew = retrievability_at(1_000, 500, 2.0).unwrap_err();
        assert!(matches!(skew, FsrsError::TimeSkew(_)));
        assert_eq!(skew.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn success_increases_and_failure_halves_stability() {
        let state = FsrsState::default();
        let r = retrievability(1.0, state.stability_days).unwrap();
        let after = on_success(state, r).unwrap();
        assert!(after.stability_days > state.stability_days);
        assert_eq!(after.difficulty, state.difficulty);
        assert_eq!(after.reviews, 1);

        let failed = on_failure(state).unwrap();
        assert_eq!(failed.stability_days, 1.0);
        assert_eq!(failed.reviews, 1);
    }

    #[test]
    fn stability_never_drops_below_the_floor_or_above_the_ceiling() {
        let floor = on_failure(FsrsState::new(S_MIN_DAYS, 5.0).unwrap()).unwrap();
        assert_eq!(floor.stability_days, S_MIN_DAYS);
        let ceiling = on_success(FsrsState::new(S_MAX_DAYS, 1.0).unwrap(), 0.0).unwrap();
        assert_eq!(ceiling.stability_days, S_MAX_DAYS);
    }

    #[test]
    fn state_validation_rejects_zero_stability_and_bad_difficulty() {
        assert!(FsrsState::default().validate().is_ok());
        let zero = FsrsState {
            stability_days: 0.0,
            ..FsrsState::default()
        };
        assert_eq!(zero.validate().unwrap_err(), FsrsError::ZeroStability);
        assert!(FsrsState::new(0.0, 5.0).is_err());
        assert!(FsrsState::new(2.0, 0.5).is_err());
        assert!(FsrsState::new(2.0, 10.5).is_err());
        assert!(FsrsState::new(2.0, f64::INFINITY).is_err());
        assert!(FsrsState::new(2.0, 7.0).is_ok());
    }

    #[test]
    fn retention_gates_use_the_registered_thresholds() {
        assert_eq!(retention_gate(0.0).unwrap(), RetentionGate::DeleteCandidate);
        assert_eq!(
            retention_gate(0.2999).unwrap(),
            RetentionGate::DeleteCandidate
        );
        assert_eq!(
            retention_gate(0.3).unwrap(),
            RetentionGate::CompressCandidate
        );
        assert_eq!(
            retention_gate(0.6999).unwrap(),
            RetentionGate::CompressCandidate
        );
        assert_eq!(retention_gate(0.7).unwrap(), RetentionGate::Keep);
        assert_eq!(retention_gate(1.0).unwrap(), RetentionGate::Keep);
        assert!(retention_gate(1.5).is_err());
        assert!(retention_gate(f64::NAN).is_err());
    }

    #[test]
    fn active_ttl_halves_per_depth_until_the_floor() {
        assert_eq!(active_ttl_hours(0), 72.0);
        assert_eq!(active_ttl_hours(1), 36.0);
        assert_eq!(active_ttl_hours(2), 18.0);
        assert_eq!(active_ttl_hours(3), 9.0);
        assert_eq!(active_ttl_hours(10), 1.0);
        assert_eq!(active_ttl_hours(64), 1.0);
    }

    #[test]
    fn effective_active_until_prefers_the_earlier_horizon() {
        let created = 1_000_000;
        // Unfinished: creation horizon only.
        assert_eq!(
            effective_active_until(created, None, 0),
            created + 72 * 3_600
        );
        // Completed early: follow-up window wins.
        assert_eq!(
            effective_active_until(created, Some(created + 3_600), 0),
            created + 3_600 + 3_600
        );
        // Completed late: creation horizon wins.
        assert_eq!(
            effective_active_until(created, Some(created + 200 * 3_600), 0),
            created + 72 * 3_600
        );
    }

    #[test]
    fn entropy_signal_skips_empty_and_flags_redundancy() {
        assert_eq!(entropy_signal(&[]), EntropySignal::SkippedEmpty);
        assert_eq!(entropy_signal(&[]).as_str(), "SKIPPED_EMPTY");
        assert!(!entropy_signal(&[]).is_redundant());
        let repetitive = vec![b'a'; 4096];
        let signal = entropy_signal(&repetitive);
        assert!(signal.is_redundant(), "signal={signal:?}");
        let mut noisy = Vec::new();
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..4096 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            noisy.push((state & 0xff) as u8);
        }
        let noisy_signal = entropy_signal(&noisy);
        assert!(!noisy_signal.is_redundant(), "signal={noisy_signal:?}");
    }

    #[test]
    fn compression_candidate_needs_both_mid_retention_and_redundancy() {
        let redundant = entropy_signal(&[b'x'; 1024]);
        assert!(is_compression_candidate(0.5, redundant));
        assert!(!is_compression_candidate(0.1, redundant));
        assert!(!is_compression_candidate(0.9, redundant));
        assert!(!is_compression_candidate(0.5, EntropySignal::SkippedEmpty));
    }

    #[test]
    fn folded_trust_decays_with_fold_level() {
        assert_eq!(folded_trust(1.0, 0), 1.0);
        let once = folded_trust(1.0, 1);
        let twice = folded_trust(1.0, 2);
        assert!(once < 1.0 && twice < once);
        assert!((once - (-0.15f64).exp()).abs() < 1e-12);
    }
}
