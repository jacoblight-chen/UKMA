//! Bayesian + Kalman trust (§26.1) and the four-column separation (§26.2).
//!
//! Trust is a *ranking* signal: it never changes source verification, ACLs or
//! owner authorization, and `authz_decision` is a decision object rather than a
//! score. Repeated restatement of the same source never counts as an
//! independent observation.

use std::collections::BTreeSet;

use crate::domain::{MemoryError, Result, VerificationState};

/// Beta prior success pseudo-count (§29.2 `Beta=(2,2)`).
pub const BETA_ALPHA0: f64 = 2.0;
/// Beta prior failure pseudo-count (§29.2 `Beta=(2,2)`).
pub const BETA_BETA0: f64 = 2.0;
/// Kalman process noise (§29.2 `Kalman_Q=.05`).
pub const KALMAN_Q: f64 = 0.05;
/// Kalman measurement noise (§29.2 `Kalman_R=.1`).
pub const KALMAN_R: f64 = 0.1;
/// Weight of the Beta mean in the ranking signal (§26.1 `T_rank`).
pub const BETA_WEIGHT: f64 = 0.7;
/// Weight of the Kalman mean in the ranking signal (§26.1 `T_rank`).
pub const KALMAN_WEIGHT: f64 = 0.3;
/// Agent-depth discount rate (§29.2 `depth_tau=.2`).
pub const DEPTH_TAU: f64 = 0.2;
/// Fold-level discount rate (§29.2 `fold_kappa=.15`).
pub const FOLD_KAPPA: f64 = 0.15;

/// Beta posterior over whether a source is reliable.
#[derive(Debug, Clone, PartialEq)]
pub struct BetaState {
    /// Posterior success count.
    pub alpha: f64,
    /// Posterior failure count.
    pub beta: f64,
    /// Independent source groups already counted (§26.1: restatement is not a
    /// new observation).
    pub groups: BTreeSet<String>,
}

impl Default for BetaState {
    fn default() -> Self {
        Self {
            alpha: BETA_ALPHA0,
            beta: BETA_BETA0,
            groups: BTreeSet::new(),
        }
    }
}

impl BetaState {
    /// Posterior mean `E[T] = alpha/(alpha+beta)`.
    pub fn mean(&self) -> f64 {
        let total = self.alpha + self.beta;
        if total <= 0.0 {
            return 0.5;
        }
        (self.alpha / total).clamp(0.0, 1.0)
    }

    /// Number of independent observations behind the posterior.
    pub fn observations(&self) -> u32 {
        self.groups.len() as u32
    }

    /// Record an observation from a source group.
    ///
    /// Returns `Ok(false)` when the group was already counted, which is how a
    /// repeated restatement is prevented from inflating the posterior.
    pub fn record(&mut self, source_group: &str, positive: bool) -> Result<bool> {
        if source_group.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "source_group".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let key = format!("{source_group}:{}", if positive { "p" } else { "n" });
        if self.groups.contains(&key) {
            return Ok(false);
        }
        self.groups.insert(key);
        if positive {
            self.alpha += 1.0;
        } else {
            self.beta += 1.0;
        }
        Ok(true)
    }
}

/// Kalman smoother over a trust measurement stream.
#[derive(Debug, Clone, PartialEq)]
pub struct KalmanState {
    /// Current mean, `None` until the first measurement.
    pub mean: Option<f64>,
    /// Current estimate variance.
    pub variance: f64,
}

impl Default for KalmanState {
    fn default() -> Self {
        Self {
            mean: None,
            variance: 1.0,
        }
    }
}

impl KalmanState {
    /// Whether a measurement has been folded in yet.
    pub fn is_initialized(&self) -> bool {
        self.mean.is_some()
    }

    /// Fold one measurement in (§26.1 Kalman update).
    pub fn observe(&mut self, measurement: f64) -> Result<()> {
        if !measurement.is_finite() || !(0.0..=1.0).contains(&measurement) {
            return Err(MemoryError::InvalidArgument {
                field: "measurement".to_string(),
                reason: format!("must be in [0,1], got {measurement}"),
            });
        }
        match self.mean {
            None => {
                self.mean = Some(measurement);
                self.variance = KALMAN_R;
                Ok(())
            }
            Some(previous) => {
                let predicted_variance = self.variance + KALMAN_Q;
                let gain = predicted_variance / (predicted_variance + KALMAN_R);
                let updated = previous + gain * (measurement - previous);
                self.mean = Some(updated.clamp(0.0, 1.0));
                self.variance = (1.0 - gain) * predicted_variance;
                Ok(())
            }
        }
    }
}

/// Why a ranking value was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustRankReason {
    /// `0.7*BetaMean + 0.3*KalmanMean`.
    Weighted,
    /// Kalman is uninitialized, so the Beta mean is used and recorded (§26.1).
    BetaOnlyColdStart,
}

impl TrustRankReason {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustRankReason::Weighted => "weighted_beta_kalman",
            TrustRankReason::BetaOnlyColdStart => "beta_only_cold_start",
        }
    }
}

/// A bounded ranking value with its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustRank {
    /// Value in `[0,1]`.
    pub value: f64,
    /// Beta posterior mean.
    pub beta_mean: f64,
    /// Kalman mean, when initialized.
    pub kalman_mean: Option<f64>,
    /// How the value was produced.
    pub reason: TrustRankReason,
}

impl TrustRank {
    /// Whether the ranking fell back to Beta only.
    pub fn is_fallback(&self) -> bool {
        self.reason == TrustRankReason::BetaOnlyColdStart
    }
}

/// `T_rank = clamp(0.7*BetaMean + 0.3*KalmanMean, 0, 1)` (§26.1).
pub fn rank(beta: &BetaState, kalman: &KalmanState) -> Result<TrustRank> {
    let beta_mean = beta.mean();
    if !beta_mean.is_finite() {
        return Err(MemoryError::InvalidArgument {
            field: "beta_mean".to_string(),
            reason: "must be finite".to_string(),
        });
    }
    match kalman.mean {
        Some(mean) => {
            if !mean.is_finite() {
                return Err(MemoryError::InvalidArgument {
                    field: "kalman_mean".to_string(),
                    reason: "must be finite".to_string(),
                });
            }
            Ok(TrustRank {
                value: (BETA_WEIGHT * beta_mean + KALMAN_WEIGHT * mean).clamp(0.0, 1.0),
                beta_mean,
                kalman_mean: Some(mean),
                reason: TrustRankReason::Weighted,
            })
        }
        None => Ok(TrustRank {
            value: beta_mean.clamp(0.0, 1.0),
            beta_mean,
            kalman_mean: None,
            reason: TrustRankReason::BetaOnlyColdStart,
        }),
    }
}

/// Authorization outcome. Deliberately a decision object, not a score (§26.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzDecision {
    /// The caller may read the record.
    Allow,
    /// The caller may not read the record.
    Deny,
}

/// The four trust columns plus the separate authorization decision (§26.2).
#[derive(Debug, Clone, PartialEq)]
pub struct TrustColumns {
    /// `source_reliability` in `[0,1]`.
    pub source_reliability: f64,
    /// `claim_confidence` in `[0,1]`.
    pub claim_confidence: f64,
    /// `extractor_confidence` in `[0,1]`.
    pub extractor_confidence: f64,
    /// Verification state, never derived from the scores.
    pub verification_state: VerificationState,
    /// Authorization decision, never a score.
    pub authz_decision: AuthzDecision,
}

impl TrustColumns {
    /// Validate every score is bounded and finite.
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("source_reliability", self.source_reliability),
            ("claim_confidence", self.claim_confidence),
            ("extractor_confidence", self.extractor_confidence),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(MemoryError::InvalidArgument {
                    field: name.to_string(),
                    reason: format!("must be in [0,1], got {value}"),
                });
            }
        }
        Ok(())
    }
}

/// Agent-depth discount `exp(-0.2*depth)` for indirect claims (§26.1).
pub fn depth_discount(depth: u32) -> f64 {
    (-DEPTH_TAU * f64::from(depth)).exp()
}

/// Fold-level discount `exp(-0.15*fold_level)` for indirect claims (§26.1).
pub fn fold_discount(fold_level: u32) -> f64 {
    (-FOLD_KAPPA * f64::from(fold_level)).exp()
}

/// Ranking value for an indirect claim.
///
/// The discount applies to the *ranking* signal only: verification state, ACLs
/// and owner authorization are untouched (§26.1, AG-003).
pub fn indirect_rank(rank: &TrustRank, depth: u32, fold_level: u32) -> f64 {
    (rank.value * depth_discount(depth) * fold_discount(fold_level)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beta_starts_at_the_registered_prior_and_updates() {
        let mut beta = BetaState::default();
        assert_eq!(beta.alpha, BETA_ALPHA0);
        assert_eq!(beta.beta, BETA_BETA0);
        assert_eq!(beta.mean(), 0.5);
        assert!(beta.record("doc-a", true).unwrap());
        assert_eq!(beta.alpha, 3.0);
        assert!((beta.mean() - 3.0 / 5.0).abs() < 1e-12);
        assert!(beta.record("doc-a", false).unwrap());
        assert_eq!(beta.beta, 3.0);
        assert_eq!(beta.observations(), 2);
    }

    #[test]
    fn a_repeated_restatement_never_counts_twice() {
        let mut beta = BetaState::default();
        assert!(beta.record("doc-a", true).unwrap());
        for _ in 0..5 {
            assert!(!beta.record("doc-a", true).unwrap(), "restatement");
        }
        assert_eq!(beta.alpha, 3.0, "only one observation is counted");
        assert!(beta.record("doc-b", true).unwrap());
        assert_eq!(beta.alpha, 4.0);
    }

    #[test]
    fn beta_rejects_an_empty_source_group() {
        let mut beta = BetaState::default();
        assert_eq!(
            beta.record("  ", true).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn kalman_cold_start_then_smooths_towards_the_measurement() {
        let mut kalman = KalmanState::default();
        assert!(!kalman.is_initialized());
        kalman.observe(0.2).unwrap();
        assert_eq!(kalman.mean, Some(0.2));
        let variance_after_first = kalman.variance;
        kalman.observe(0.9).unwrap();
        let mean = kalman.mean.unwrap();
        assert!(mean > 0.2 && mean < 0.9, "mean={mean}");
        assert!(kalman.variance < variance_after_first);
        assert!(kalman.observe(1.5).is_err());
        assert!(kalman.observe(f64::NAN).is_err());
    }

    #[test]
    fn ranking_uses_beta_only_until_kalman_is_initialized() {
        let beta = BetaState::default();
        let cold = rank(&beta, &KalmanState::default()).unwrap();
        assert_eq!(cold.value, 0.5);
        assert!(cold.is_fallback());
        assert_eq!(cold.reason.as_str(), "beta_only_cold_start");

        let mut kalman = KalmanState::default();
        kalman.observe(0.9).unwrap();
        let warm = rank(&beta, &kalman).unwrap();
        assert!(!warm.is_fallback());
        assert_eq!(warm.reason.as_str(), "weighted_beta_kalman");
        assert!((warm.value - (0.7 * 0.5 + 0.3 * 0.9)).abs() < 1e-12);
        assert!((0.0..=1.0).contains(&warm.value));
    }

    #[test]
    fn column_validation_is_bounded_and_authorization_is_not_a_score() {
        let columns = TrustColumns {
            source_reliability: 0.9,
            claim_confidence: 0.7,
            extractor_confidence: 0.8,
            verification_state: VerificationState::Unverified,
            authz_decision: AuthzDecision::Allow,
        };
        assert!(columns.validate().is_ok());
        let bad = TrustColumns {
            claim_confidence: 1.2,
            ..columns.clone()
        };
        assert_eq!(bad.validate().unwrap_err().code(), "INVALID_ARGUMENT");
        // A low score never flips the decision, and a denial never lowers a score.
        let denied = TrustColumns {
            authz_decision: AuthzDecision::Deny,
            ..columns
        };
        assert_eq!(denied.source_reliability, 0.9);
    }

    #[test]
    fn discounts_only_reduce_the_ranking_signal() {
        assert_eq!(depth_discount(0), 1.0);
        assert!((depth_discount(1) - (-0.2f64).exp()).abs() < 1e-12);
        assert!(depth_discount(3) < depth_discount(2));
        assert_eq!(fold_discount(0), 1.0);
        assert!(fold_discount(2) < fold_discount(1));

        let rank = TrustRank {
            value: 0.8,
            beta_mean: 0.8,
            kalman_mean: None,
            reason: TrustRankReason::BetaOnlyColdStart,
        };
        let indirect = indirect_rank(&rank, 2, 3);
        assert!(indirect < rank.value);
        assert_eq!(
            rank.beta_mean, 0.8,
            "the raw columns are unchanged by discounting"
        );
    }
}
