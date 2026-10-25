//! Semantic facts: proposals, independent verification and conflict sets
//! (§21.4, §25.4, §27.1, MAPI-10/11/12).
//!
//! A fact is only promoted when at least three *independent* source groups and
//! the confidence gate agree, and no contradiction is unresolved. Two documents
//! citing each other, or one agent repeating a sentence, count as one group.

use std::collections::BTreeSet;

use crate::domain::{Evidence, MemoryError, Result, VerificationState};

/// Independent source groups required for promotion (§27.1).
pub const PROMOTION_MIN_GROUPS: usize = 3;
/// Confidence required for promotion (§27.1 `confidence>=0.65`).
pub const PROMOTION_MIN_CONFIDENCE: f64 = 0.65;
/// Similarity required to treat two claims as the same concept (§25.4).
pub const THETA_ALIGN: f64 = 0.85;
/// Similarity that only recalls candidates, never proves equivalence (§25.4).
pub const THETA_CROSS: f64 = 0.75;

/// One authorised translation of a claim (§25.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Translation {
    /// BCP-47 target language.
    pub language: String,
    /// Translated utterance.
    pub text: String,
    /// Whether the translation was independently verified.
    pub verified: bool,
}

/// One semantic claim (§21.4).
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticFact {
    /// Fact id.
    pub id: String,
    /// Canonical subject concept id.
    pub subject: String,
    /// Predicate.
    pub predicate: String,
    /// Object of the claim.
    pub object: String,
    /// Optional qualifier carrying negation, unit or time.
    pub qualifier: Option<String>,
    /// BCP-47 language of the original utterance.
    pub language: String,
    /// Translations kept beside the original, never replacing it.
    pub translations: Vec<Translation>,
    /// Claim confidence in `[0,1]`.
    pub confidence: f64,
    /// Evidence supporting the claim.
    pub evidence: Vec<Evidence>,
    /// Verification state.
    pub verification: VerificationState,
    /// Facts that contradict this one.
    pub contradicted_by: BTreeSet<String>,
    /// Revision number.
    pub version: u64,
    /// Creation instant.
    pub created_at: i64,
    /// Instant of the last verification.
    pub last_verified_at: Option<i64>,
    /// Whether a legal hold covers the claim.
    pub legal_hold: bool,
}

impl SemanticFact {
    /// Validate the bounded fields and the required evidence (§34.2 MAPI-10).
    pub fn validate(&self) -> Result<()> {
        if self.subject.trim().is_empty() || self.predicate.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "subject".to_string(),
                reason: "subject and predicate must be non-empty".to_string(),
            });
        }
        if self.language.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "language".to_string(),
                reason: "must be a BCP-47 tag".to_string(),
            });
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(MemoryError::InvalidArgument {
                field: "confidence".to_string(),
                reason: format!("must be in [0,1], got {}", self.confidence),
            });
        }
        if self.evidence.is_empty() {
            return Err(MemoryError::EvidenceRequired {
                operation: "memory.propose_fact".to_string(),
            });
        }
        for item in &self.evidence {
            item.validate()?;
        }
        Ok(())
    }

    /// Distinct source groups behind the claim (§27.1).
    pub fn independent_groups(&self) -> BTreeSet<&str> {
        self.evidence
            .iter()
            .map(|item| item.source_group.as_str())
            .collect()
    }

    /// Whether the claim is currently contradicted.
    pub fn is_contradicted(&self) -> bool {
        !self.contradicted_by.is_empty()
    }
}

/// How two claims relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRelation {
    /// Same proposition expressed in another language or wording.
    Equivalent,
    /// Same subject and predicate with an incompatible object or qualifier.
    Contradicts,
    /// Not comparable.
    Unrelated,
}

impl ClaimRelation {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimRelation::Equivalent => "EQUIVALENT",
            ClaimRelation::Contradicts => "CONTRADICTS",
            ClaimRelation::Unrelated => "UNRELATED",
        }
    }
}

fn normalised(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Whether two qualifiers are compatible (both absent counts as compatible).
fn compatible_qualifiers(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => normalised(a) == normalised(b),
        (None, Some(b)) | (Some(b), None) => normalised(b).is_empty(),
    }
}

/// Classify the relation between two claims (§25.4).
///
/// `similarity` is a calibrated similarity in `[0,1]`; a value below
/// [`THETA_ALIGN`] never proves equivalence, and any negation/unit/version
/// mismatch becomes a contradiction instead of an equivalence.
pub fn classify(left: &SemanticFact, right: &SemanticFact, similarity: f64) -> ClaimRelation {
    if !similarity.is_finite() {
        return ClaimRelation::Unrelated;
    }
    if normalised(&left.subject) != normalised(&right.subject)
        || normalised(&left.predicate) != normalised(&right.predicate)
    {
        return ClaimRelation::Unrelated;
    }
    let same_object = normalised(&left.object) == normalised(&right.object);
    let compatible = compatible_qualifiers(left.qualifier.as_deref(), right.qualifier.as_deref());
    if same_object && compatible && similarity >= THETA_ALIGN {
        ClaimRelation::Equivalent
    } else if !same_object || !compatible {
        ClaimRelation::Contradicts
    } else {
        ClaimRelation::Unrelated
    }
}

/// Whether a cross-lingual pair is only a recall candidate (§25.4).
pub fn is_recall_candidate(similarity: f64) -> bool {
    similarity.is_finite() && (THETA_CROSS..THETA_ALIGN).contains(&similarity)
}

/// Outcome of the promotion gate (§27.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionGate {
    /// Whether promotion is allowed.
    pub allowed: bool,
    /// Groups counted.
    pub groups: usize,
    /// Why promotion was refused, empty when allowed.
    pub reasons: Vec<&'static str>,
    /// Whether the manual approval exception was used.
    pub manual_override: bool,
}

/// Auditable manual approval of an under-sourced candidate (§27.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualApproval {
    /// Approval reference recorded in the audit trail.
    pub approval_ref: String,
    /// Why the reviewer approved it.
    pub reason: String,
    /// Whether the source was actually verified.
    pub source_verified: bool,
    /// Whether collection was legal.
    pub legal_collection: bool,
    /// Whether the evidence is still valid.
    pub not_expired: bool,
}

/// Evaluate the promotion gate.
///
/// A manual approval may waive the independent-group count, but it can never
/// waive safety, ACL, hold, missing evidence or an unresolved contradiction.
pub fn promotion_gate(fact: &SemanticFact, manual: Option<&ManualApproval>) -> PromotionGate {
    let groups = fact.independent_groups().len();
    let mut reasons = Vec::new();
    if fact.evidence.is_empty() {
        reasons.push("missing_evidence");
    }
    if fact.legal_hold {
        reasons.push("legal_hold");
    }
    if fact.is_contradicted() {
        reasons.push("unresolved_contradiction");
    }
    if fact.verification == VerificationState::Rejected {
        reasons.push("rejected");
    }
    let confidence_ok = fact.confidence >= PROMOTION_MIN_CONFIDENCE;
    if !confidence_ok {
        reasons.push("confidence_below_threshold");
    }
    let groups_ok = groups >= PROMOTION_MIN_GROUPS;
    let mut manual_override = false;
    if !groups_ok {
        match manual {
            Some(approval) => {
                if approval.approval_ref.trim().is_empty() || approval.reason.trim().is_empty() {
                    reasons.push("manual_approval_incomplete");
                } else if !approval.source_verified {
                    reasons.push("manual_approval_unverified_source");
                } else if !approval.legal_collection {
                    reasons.push("manual_approval_illegal_collection");
                } else if !approval.not_expired {
                    reasons.push("manual_approval_expired_evidence");
                } else if reasons.is_empty() {
                    manual_override = true;
                }
            }
            None => reasons.push("insufficient_independent_groups"),
        }
    }
    PromotionGate {
        allowed: reasons.is_empty(),
        groups,
        reasons,
        manual_override,
    }
}

/// Decision of a fact review (§34.2 MAPI-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    /// Accept the claim.
    Verify,
    /// Reject the claim.
    Reject,
}

/// Result of `verify_fact`.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationOutcome {
    /// New verification state.
    pub state: VerificationState,
    /// Evidence ids that were accepted as independent.
    pub accepted_evidence: Vec<String>,
    /// Whether the state changed.
    pub changed: bool,
}

/// Apply a review decision to a fact (§34.2 MAPI-11).
///
/// The reviewer must present evidence ids that exist on the fact and are not the
/// single source that produced it, and a conflict with an existing review is
/// reported rather than overwritten.
pub fn verify(
    fact: &mut SemanticFact,
    evidence_ids: &[String],
    decision: ReviewDecision,
    now: i64,
) -> Result<VerificationOutcome> {
    if matches!(
        fact.verification,
        VerificationState::Verified | VerificationState::Rejected
    ) {
        return Err(MemoryError::StateConflict {
            resource: fact.id.clone(),
            state: fact.verification.as_str().to_string(),
            operation: "verify_fact".to_string(),
        });
    }
    if evidence_ids.is_empty() {
        return Err(MemoryError::EvidenceRequired {
            operation: "memory.verify_fact".to_string(),
        });
    }
    let available: BTreeSet<&str> = fact
        .evidence
        .iter()
        .map(|item| item.evidence_id.as_str())
        .collect();
    let mut accepted = Vec::new();
    for id in evidence_ids {
        if !available.contains(id.as_str()) {
            return Err(MemoryError::NotFound {
                resource: format!("evidence {id}"),
            });
        }
        accepted.push(id.clone());
    }
    if fact.is_contradicted() && decision == ReviewDecision::Verify {
        return Err(MemoryError::PolicyDenied {
            reason: "a contradicted claim cannot be verified before the conflict is resolved"
                .to_string(),
        });
    }
    fact.verification = match decision {
        ReviewDecision::Verify => VerificationState::Verified,
        ReviewDecision::Reject => VerificationState::Rejected,
    };
    fact.last_verified_at = Some(now);
    fact.version += 1;
    Ok(VerificationOutcome {
        state: fact.verification,
        accepted_evidence: accepted,
        changed: true,
    })
}

/// Find the facts that conflict with `target` (§34.2 MAPI-12).
pub fn find_conflicts<'a>(
    target: &SemanticFact,
    candidates: impl IntoIterator<Item = &'a SemanticFact>,
    max_results: usize,
) -> Result<Vec<&'a SemanticFact>> {
    if max_results == 0 {
        return Err(MemoryError::InvalidArgument {
            field: "max_results".to_string(),
            reason: "must be greater than zero".to_string(),
        });
    }
    let mut conflicts: Vec<&SemanticFact> = candidates
        .into_iter()
        .filter(|candidate| candidate.id != target.id)
        .filter(|candidate| classify(target, candidate, THETA_ALIGN) == ClaimRelation::Contradicts)
        .collect();
    conflicts.sort_by(|left, right| left.id.cmp(&right.id));
    conflicts.truncate(max_results);
    Ok(conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(id: &str, group: &str) -> Evidence {
        Evidence {
            evidence_id: id.to_string(),
            source_ref: format!("artifact:sha256:{id}"),
            confidence: 0.9,
            source_group: group.to_string(),
        }
    }

    fn fact() -> SemanticFact {
        SemanticFact {
            id: "fact:1".to_string(),
            subject: "concept:order".to_string(),
            predicate: "status".to_string(),
            object: "shipped".to_string(),
            qualifier: None,
            language: "en".to_string(),
            translations: Vec::new(),
            confidence: 0.7,
            evidence: vec![evidence("e1", "doc-a"), evidence("e2", "doc-b")],
            verification: VerificationState::Unverified,
            contradicted_by: BTreeSet::new(),
            version: 1,
            created_at: 0,
            last_verified_at: None,
            legal_hold: false,
        }
    }

    #[test]
    fn proposal_requires_evidence_and_bounded_confidence() {
        let mut candidate = fact();
        assert!(candidate.validate().is_ok());
        candidate.evidence.clear();
        assert_eq!(
            candidate.validate().unwrap_err().code(),
            "EVIDENCE_REQUIRED"
        );
        let mut bad = fact();
        bad.confidence = 1.4;
        assert_eq!(bad.validate().unwrap_err().code(), "INVALID_ARGUMENT");
        let mut blank = fact();
        blank.subject = " ".into();
        assert!(blank.validate().is_err());
    }

    #[test]
    fn independent_groups_are_deduplicated() {
        let mut candidate = fact();
        candidate.evidence.push(evidence("e3", "doc-a"));
        assert_eq!(candidate.independent_groups().len(), 2);
    }

    #[test]
    fn promotion_needs_three_groups_and_the_confidence_gate() {
        let mut candidate = fact();
        let gate = promotion_gate(&candidate, None);
        assert!(!gate.allowed);
        assert_eq!(gate.groups, 2);
        assert!(gate.reasons.contains(&"insufficient_independent_groups"));

        candidate.evidence.push(evidence("e3", "doc-c"));
        let gate = promotion_gate(&candidate, None);
        assert!(gate.allowed, "{gate:?}");
        assert!(!gate.manual_override);

        let mut low = candidate.clone();
        low.confidence = 0.5;
        let gate = promotion_gate(&low, None);
        assert!(!gate.allowed);
        assert!(gate.reasons.contains(&"confidence_below_threshold"));
    }

    #[test]
    fn a_manual_approval_waives_only_the_group_count() {
        let candidate = fact();
        let approval = ManualApproval {
            approval_ref: "approval:1".into(),
            reason: "primary regulator document".into(),
            source_verified: true,
            legal_collection: true,
            not_expired: true,
        };
        let gate = promotion_gate(&candidate, Some(&approval));
        assert!(gate.allowed);
        assert!(gate.manual_override);

        let mut held = candidate.clone();
        held.legal_hold = true;
        let gate = promotion_gate(&held, Some(&approval));
        assert!(!gate.allowed);
        assert!(gate.reasons.contains(&"legal_hold"));

        let mut contradicted = candidate.clone();
        contradicted.contradicted_by.insert("fact:2".into());
        let gate = promotion_gate(&contradicted, Some(&approval));
        assert!(!gate.allowed);
        assert!(gate.reasons.contains(&"unresolved_contradiction"));

        let weak = ManualApproval {
            source_verified: false,
            ..approval
        };
        let gate = promotion_gate(&candidate, Some(&weak));
        assert!(!gate.allowed);
        assert!(gate.reasons.contains(&"manual_approval_unverified_source"));
    }

    #[test]
    fn equivalence_requires_the_alignment_threshold() {
        let left = fact();
        let mut right = fact();
        right.id = "fact:2".into();
        right.language = "zh-Hant".into();
        assert_eq!(classify(&left, &right, 0.9), ClaimRelation::Equivalent);
        assert_eq!(classify(&left, &right, 0.8), ClaimRelation::Unrelated);
        assert!(is_recall_candidate(0.8));
        assert!(!is_recall_candidate(0.9));
        assert!(!is_recall_candidate(0.5));
    }

    #[test]
    fn a_negated_or_numeric_difference_is_a_contradiction() {
        let left = fact();
        let mut negated = fact();
        negated.id = "fact:2".into();
        negated.qualifier = Some("not".into());
        assert_eq!(classify(&left, &negated, 0.95), ClaimRelation::Contradicts);

        let mut numeric = fact();
        numeric.id = "fact:3".into();
        numeric.object = "delivered".into();
        assert_eq!(classify(&left, &numeric, 0.95), ClaimRelation::Contradicts);

        let mut other = fact();
        other.id = "fact:4".into();
        other.subject = "concept:invoice".into();
        assert_eq!(classify(&left, &other, 0.99), ClaimRelation::Unrelated);
        assert_eq!(classify(&left, &other, f64::NAN), ClaimRelation::Unrelated);
    }

    #[test]
    fn verification_needs_known_evidence_and_cannot_be_repeated() {
        let mut candidate = fact();
        let outcome = verify(
            &mut candidate,
            &["e1".to_string()],
            ReviewDecision::Verify,
            42,
        )
        .unwrap();
        assert_eq!(outcome.state, VerificationState::Verified);
        assert_eq!(candidate.last_verified_at, Some(42));
        assert_eq!(candidate.version, 2);

        assert_eq!(
            verify(
                &mut candidate,
                &["e1".to_string()],
                ReviewDecision::Reject,
                43
            )
            .unwrap_err()
            .code(),
            "LINK_STATE_CONFLICT"
        );

        let mut fresh = fact();
        assert_eq!(
            verify(&mut fresh, &[], ReviewDecision::Verify, 1)
                .unwrap_err()
                .code(),
            "EVIDENCE_REQUIRED"
        );
        assert_eq!(
            verify(&mut fresh, &["e9".to_string()], ReviewDecision::Verify, 1)
                .unwrap_err()
                .code(),
            "NOT_FOUND"
        );
    }

    #[test]
    fn a_contradicted_claim_cannot_be_verified() {
        let mut candidate = fact();
        candidate.contradicted_by.insert("fact:2".into());
        assert_eq!(
            verify(
                &mut candidate,
                &["e1".to_string()],
                ReviewDecision::Verify,
                1
            )
            .unwrap_err()
            .code(),
            "POLICY_DENIED"
        );
        let rejected = verify(
            &mut candidate,
            &["e1".to_string()],
            ReviewDecision::Reject,
            1,
        )
        .unwrap();
        assert_eq!(rejected.state, VerificationState::Rejected);
    }

    #[test]
    fn conflicts_are_listed_deterministically_and_bounded() {
        let target = fact();
        let mut first = fact();
        first.id = "fact:a".into();
        first.object = "cancelled".into();
        let mut second = fact();
        second.id = "fact:b".into();
        second.qualifier = Some("not".into());
        let mut unrelated = fact();
        unrelated.id = "fact:c".into();
        unrelated.predicate = "owner".into();
        let candidates = [second.clone(), first.clone(), unrelated];

        let conflicts = find_conflicts(&target, candidates.iter(), 10).unwrap();
        assert_eq!(
            conflicts
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fact:a", "fact:b"]
        );
        assert_eq!(
            find_conflicts(&target, candidates.iter(), 1).unwrap().len(),
            1
        );
        assert_eq!(
            find_conflicts(&target, candidates.iter(), 0)
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
    }
}
