//! Three-level folding (§22.2) with the non-negotiable preservation rules.
//!
//! Safety rules run **before** any model suggestion: a protected block is never
//! folded, no matter what the scorer says (§22.2, `FO-003`, `PROP-05`).

/// High threshold: `score > 0.7` means PRESERVE.
pub const FOLD_HIGH: f64 = 0.7;
/// Low threshold: `score <= 0.3` means FOLD.
pub const FOLD_LOW: f64 = 0.3;
/// Trust decay per fold level (§22.2 `T_fold = T_orig * exp(-0.15 * level)`).
pub const FOLD_TRUST_KAPPA: f64 = 0.15;
/// Number of MLP input features (§22.2).
pub const FEATURE_COUNT: usize = 22;

/// Folding level for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FoldLevel {
    /// Keep the original content (or a lossless reference).
    Preserve,
    /// Summarise, keeping key facts and references.
    Summarize,
    /// Replace the process with a `FoldedResult` plus evidence pointers.
    Fold,
}

/// Deterministic level from an importance score in `[0,1]` (§22.2 boundaries).
pub fn classify(score: f64) -> FoldLevel {
    if score > FOLD_HIGH {
        FoldLevel::Preserve
    } else if score > FOLD_LOW {
        FoldLevel::Summarize
    } else {
        FoldLevel::Fold
    }
}

/// Trust discount of a folded claim; it only signals indirectness and MUST NOT
/// change verification state or authorization (§22.2, §26.1).
pub fn trust_after_folding(t_original: f64, fold_level: u32) -> f64 {
    let decayed = t_original * (-FOLD_TRUST_KAPPA * f64::from(fold_level)).exp();
    decayed.clamp(0.0, 1.0)
}

/// The eight contents that must always be preserved (§22.2 R1–R8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreserveRule {
    /// R1 explicit user instruction.
    UserInstruction,
    /// R2 errors and diagnostic evidence.
    Diagnostic,
    /// R3 security / permission / system rules.
    Security,
    /// R4 in-flight task state.
    TaskState,
    /// R5 critical cross-lingual bridge.
    CrossLingualBridge,
    /// R6 explicit user pin.
    UserPin,
    /// R7 authoritative test / code / spec evidence.
    AuthorityEvidence,
    /// R8 legal hold.
    LegalHold,
}

impl PreserveRule {
    /// All rules, in specification order.
    pub const ALL: [PreserveRule; 8] = [
        PreserveRule::UserInstruction,
        PreserveRule::Diagnostic,
        PreserveRule::Security,
        PreserveRule::TaskState,
        PreserveRule::CrossLingualBridge,
        PreserveRule::UserPin,
        PreserveRule::AuthorityEvidence,
        PreserveRule::LegalHold,
    ];

    /// Canonical identifier used in audit records.
    pub fn as_str(self) -> &'static str {
        match self {
            PreserveRule::UserInstruction => "R1_user_instruction",
            PreserveRule::Diagnostic => "R2_diagnostic",
            PreserveRule::Security => "R3_security",
            PreserveRule::TaskState => "R4_task_state",
            PreserveRule::CrossLingualBridge => "R5_cross_lingual_bridge",
            PreserveRule::UserPin => "R6_user_pin",
            PreserveRule::AuthorityEvidence => "R7_authority_evidence",
            PreserveRule::LegalHold => "R8_legal_hold",
        }
    }
}

/// The 22 MLP input features in the fixed order of §22.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    // semantic (0–4)
    /// Message length.
    MessageLength,
    /// Token count.
    TokenCount,
    /// TF-IDF rarity.
    TfidfRarity,
    /// Semantic density.
    SemanticDensity,
    /// Query cosine.
    QueryCosine,
    // structural (5–10)
    /// Role type.
    RoleType,
    /// Position in session.
    SessionPosition,
    /// Contains code.
    ContainsCode,
    /// Tool call count.
    ToolCallCount,
    /// File path count.
    FilePathCount,
    /// URL count.
    UrlCount,
    // temporal (11–15)
    /// Age of the message.
    TimestampAge,
    /// Elapsed since session start.
    ElapsedSinceSessionStart,
    /// Gap to previous message.
    InterMessageGap,
    /// Expected session length.
    ExpectedSessionLength,
    /// Remaining turns.
    RemainingTurns,
    // interaction (16–21)
    /// Previously referenced.
    PreviouslyReferenced,
    /// Reference count.
    ReferenceCount,
    /// Sentiment polarity.
    SentimentPolarity,
    /// Has question.
    HasQuestion,
    /// Has instruction keyword.
    HasInstructionKeyword,
    /// Error indicator.
    ErrorIndicator,
}

impl Feature {
    /// All features in canonical order (§22.2).
    pub const ALL: [Feature; FEATURE_COUNT] = [
        Feature::MessageLength,
        Feature::TokenCount,
        Feature::TfidfRarity,
        Feature::SemanticDensity,
        Feature::QueryCosine,
        Feature::RoleType,
        Feature::SessionPosition,
        Feature::ContainsCode,
        Feature::ToolCallCount,
        Feature::FilePathCount,
        Feature::UrlCount,
        Feature::TimestampAge,
        Feature::ElapsedSinceSessionStart,
        Feature::InterMessageGap,
        Feature::ExpectedSessionLength,
        Feature::RemainingTurns,
        Feature::PreviouslyReferenced,
        Feature::ReferenceCount,
        Feature::SentimentPolarity,
        Feature::HasQuestion,
        Feature::HasInstructionKeyword,
        Feature::ErrorIndicator,
    ];

    /// Index of this feature in the canonical vector.
    pub fn index(self) -> usize {
        Feature::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .expect("feature is in ALL")
    }
}

/// Feature vector with an explicit missing mask (§22.2: a missing value MUST NOT
/// be silently coerced to `0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeatureVector {
    values: [f64; FEATURE_COUNT],
    present: [bool; FEATURE_COUNT],
}

impl Default for FeatureVector {
    fn default() -> Self {
        Self {
            values: [0.0; FEATURE_COUNT],
            present: [false; FEATURE_COUNT],
        }
    }
}

impl FeatureVector {
    /// Empty vector: every feature missing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one feature (marks it present).
    pub fn with(mut self, feature: Feature, value: f64) -> Self {
        let index = feature.index();
        self.values[index] = value;
        self.present[index] = true;
        self
    }

    /// Value of a feature, if present.
    pub fn get(&self, feature: Feature) -> Option<f64> {
        let index = feature.index();
        self.present[index].then_some(self.values[index])
    }

    /// Missing mask, in canonical order.
    pub fn mask(&self) -> [bool; FEATURE_COUNT] {
        self.present
    }

    /// Number of present features.
    pub fn present_count(&self) -> usize {
        self.present.iter().filter(|present| **present).count()
    }

    /// Whether every feature is present (required for model inference).
    pub fn is_complete(&self) -> bool {
        self.present_count() == FEATURE_COUNT
    }
}

/// Why folding fell back to the deterministic policy (§22.2, `FO-002`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// No model weights available.
    ModelAbsent,
    /// Feature vector incomplete.
    FeaturesIncomplete,
    /// Model produced NaN/infinite output.
    NonFiniteScore,
    /// Model profile/version incompatible.
    Incompatible,
    /// Operator disabled the model.
    Disabled,
}

/// Decision for one block, with its reason recorded for audit.
#[derive(Debug, Clone, PartialEq)]
pub struct FoldDecision {
    /// Chosen level.
    pub level: FoldLevel,
    /// Whether a safety rule forced PRESERVE.
    pub forced_preserve: bool,
    /// Rule that forced preservation, when applicable.
    pub preserve_rule: Option<PreserveRule>,
    /// Fallback reason when the model was not used.
    pub fallback: Option<FallbackReason>,
}

/// Decide how to fold one block.
///
/// Order (§22.2): safety rules first, then the model suggestion, then the
/// deterministic classification. Safety always wins.
pub fn decide(importance: f64, rules: &[PreserveRule], model_score: Option<f64>) -> FoldDecision {
    if let Some(rule) = rules.iter().min() {
        return FoldDecision {
            level: FoldLevel::Preserve,
            forced_preserve: true,
            preserve_rule: Some(*rule),
            fallback: None,
        };
    }
    match model_score {
        Some(score) if score.is_finite() => FoldDecision {
            level: classify(score),
            forced_preserve: false,
            preserve_rule: None,
            fallback: None,
        },
        Some(_) => FoldDecision {
            level: classify(importance),
            forced_preserve: false,
            preserve_rule: None,
            fallback: Some(FallbackReason::NonFiniteScore),
        },
        None => FoldDecision {
            level: classify(importance),
            forced_preserve: false,
            preserve_rule: None,
            fallback: Some(FallbackReason::ModelAbsent),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_boundaries_are_exact() {
        assert_eq!(classify(0.700_000_1), FoldLevel::Preserve);
        assert_eq!(classify(0.7), FoldLevel::Summarize, "0.7 is not > 0.7");
        assert_eq!(classify(0.300_000_1), FoldLevel::Summarize);
        assert_eq!(classify(0.3), FoldLevel::Fold, "0.3 is not > 0.3");
        assert_eq!(classify(0.0), FoldLevel::Fold);
        assert_eq!(classify(1.0), FoldLevel::Preserve);
    }

    #[test]
    fn all_eight_preserve_rules_force_preserve() {
        for rule in PreserveRule::ALL {
            let decision = decide(0.01, &[rule], Some(0.01));
            assert_eq!(decision.level, FoldLevel::Preserve, "{rule:?} must win");
            assert!(decision.forced_preserve);
            assert_eq!(decision.preserve_rule, Some(rule));
        }
    }

    #[test]
    fn missing_model_falls_back_deterministically() {
        let decision = decide(0.9, &[], None);
        assert_eq!(decision.level, FoldLevel::Preserve);
        assert_eq!(decision.fallback, Some(FallbackReason::ModelAbsent));
    }

    #[test]
    fn nan_model_score_is_rejected_not_trusted() {
        let decision = decide(0.1, &[], Some(f64::NAN));
        assert_eq!(decision.level, FoldLevel::Fold);
        assert_eq!(decision.fallback, Some(FallbackReason::NonFiniteScore));
    }

    #[test]
    fn trust_decay_matches_formula_and_stays_bounded() {
        assert!((trust_after_folding(1.0, 0) - 1.0).abs() < 1e-12);
        assert!((trust_after_folding(1.0, 1) - (-0.15f64).exp()).abs() < 1e-12);
        assert!((trust_after_folding(1.0, 2) - (-0.3f64).exp()).abs() < 1e-12);
        assert!(trust_after_folding(5.0, 3) <= 1.0);
    }

    #[test]
    fn feature_vector_tracks_missing_mask() {
        let vector = FeatureVector::new()
            .with(Feature::TokenCount, 128.0)
            .with(Feature::HasQuestion, 1.0);
        assert_eq!(vector.present_count(), 2);
        assert_eq!(vector.get(Feature::TokenCount), Some(128.0));
        assert_eq!(vector.get(Feature::MessageLength), None);
        assert!(!vector.is_complete());
        let mask = vector.mask();
        assert!(mask[Feature::TokenCount.index()]);
        assert!(!mask[Feature::MessageLength.index()]);
    }

    #[test]
    fn feature_order_matches_spec_groups() {
        // 0–4 semantic, 5–10 structural, 11–15 temporal, 16–21 interaction.
        assert_eq!(Feature::MessageLength.index(), 0);
        assert_eq!(Feature::QueryCosine.index(), 4);
        assert_eq!(Feature::RoleType.index(), 5);
        assert_eq!(Feature::UrlCount.index(), 10);
        assert_eq!(Feature::TimestampAge.index(), 11);
        assert_eq!(Feature::RemainingTurns.index(), 15);
        assert_eq!(Feature::PreviouslyReferenced.index(), 16);
        assert_eq!(Feature::ErrorIndicator.index(), 21);
        assert_eq!(Feature::ALL.len(), FEATURE_COUNT);
    }

    #[test]
    fn complete_feature_vector_is_recognised() {
        let mut vector = FeatureVector::new();
        for (index, feature) in Feature::ALL.iter().enumerate() {
            vector = vector.with(*feature, index as f64);
        }
        assert!(vector.is_complete());
        assert_eq!(vector.present_count(), FEATURE_COUNT);
    }
}
