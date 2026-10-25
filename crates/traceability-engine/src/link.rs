//! The `TraceLink` state machine (§35.2) and the three-dimensional validity
//! assessment (§40.3).
//!
//! States and validity are independent: a link can be `stale` in the workflow
//! while its history stays `valid`, and an accepted link whose sources moved on
//! becomes `requires_revalidation`, never `invalid`.

use crate::domain::{
    CurrentApplicability, Evidence, HistoricalValidity, IndexFreshness, Result, TraceError,
    TraceLink, TraceState, ValidityVector,
};

/// Legal transitions of §35.2.
pub fn can_transition(from: TraceState, to: TraceState) -> bool {
    use TraceState::{Accepted, Proposed, Rejected, Revoked, Stale, Validated};
    match from {
        Proposed => matches!(to, Validated | Rejected | Revoked),
        Validated => matches!(to, Accepted | Rejected | Stale | Revoked),
        Accepted => matches!(to, Stale | Revoked),
        Stale => matches!(to, Validated | Revoked),
        Rejected | Revoked => false,
    }
}

/// Apply a transition, refusing anything the state machine does not allow.
pub fn transition(link: &mut TraceLink, to: TraceState, instant: &str) -> Result<()> {
    if !can_transition(link.state, to) {
        return Err(TraceError::LinkStateConflict {
            link: link.reference.id.clone(),
            state: link.state.as_str().to_string(),
            transition: format!("become {}", to.as_str()),
        });
    }
    link.state = to;
    if to == TraceState::Validated {
        link.verified_at = Some(instant.to_string());
    }
    Ok(())
}

/// Append evidence.
///
/// Evidence is immutable once accepted: adding more requires a new link revision
/// and a fresh review (§35.2).
pub fn append_evidence(link: &mut TraceLink, evidence: &[Evidence]) -> Result<()> {
    if link.state == TraceState::Accepted {
        return Err(TraceError::LinkStateConflict {
            link: link.reference.id.clone(),
            state: link.state.as_str().to_string(),
            transition: "append evidence".to_string(),
        });
    }
    for item in evidence {
        if !item.confidence.is_finite() || !(0.0..=1.0).contains(&item.confidence) {
            return Err(TraceError::InvalidArgument {
                field: "evidence.confidence".to_string(),
                reason: format!("must be in [0,1], got {}", item.confidence),
            });
        }
        if item.evidence_id.trim().is_empty() {
            return Err(TraceError::InvalidArgument {
                field: "evidence_id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if !link
            .evidence
            .iter()
            .any(|existing| existing.evidence_id == item.evidence_id)
        {
            link.evidence.push(item.clone());
        }
    }
    Ok(())
}

/// Observed head revisions of the two endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointHeads {
    /// Observed source head, `None` when unknown.
    pub source: Option<String>,
    /// Observed target head, `None` when unknown.
    pub target: Option<String>,
}

impl EndpointHeads {
    /// Heads that match the asserted revisions.
    pub fn matching(link: &TraceLink) -> Self {
        Self {
            source: Some(link.source_revision.clone()),
            target: Some(link.target_revision.clone()),
        }
    }

    /// Heads that have moved on from the asserted revisions.
    pub fn moved(link: &TraceLink) -> Self {
        Self {
            source: Some(format!("{}-next", link.source_revision)),
            target: Some(link.target_revision.clone()),
        }
    }

    /// Heads that are not observable.
    pub fn unknown() -> Self {
        Self {
            source: None,
            target: None,
        }
    }
}

/// Historical validity from the recorded verification.
///
/// A revoke withdraws the link going forward; it never rewrites history, so a
/// verified link that was later revoked keeps `valid` (§40.3).
pub fn historical_validity(link: &TraceLink) -> HistoricalValidity {
    match link.state {
        TraceState::Rejected => HistoricalValidity::Invalid,
        TraceState::Revoked => {
            if link.verified_at.is_some() {
                HistoricalValidity::Valid
            } else {
                HistoricalValidity::Unverified
            }
        }
        TraceState::Validated | TraceState::Accepted | TraceState::Stale
            if link.verified_at.is_some() =>
        {
            HistoricalValidity::Valid
        }
        _ => HistoricalValidity::Unverified,
    }
}

/// Current applicability from the observed heads.
pub fn current_applicability(link: &TraceLink, heads: &EndpointHeads) -> CurrentApplicability {
    if matches!(link.state, TraceState::Rejected | TraceState::Revoked) {
        return CurrentApplicability::NotApplicable;
    }
    match (&heads.source, &heads.target) {
        (None, _) | (_, None) => CurrentApplicability::Unknown,
        (Some(source), Some(target)) => {
            if source == &link.source_revision && target == &link.target_revision {
                CurrentApplicability::Applicable
            } else {
                CurrentApplicability::RequiresRevalidation
            }
        }
    }
}

/// Assess the three validity dimensions of a link (§40.3).
pub fn assess(
    link: &TraceLink,
    heads: &EndpointHeads,
    index: IndexFreshness,
    assessed_at: &str,
) -> ValidityVector {
    ValidityVector {
        historical_validity: historical_validity(link),
        current_applicability: current_applicability(link, heads),
        index_freshness: index,
        asserted_source_revision: link.source_revision.clone(),
        observed_head_revision: heads.source.clone(),
        assessed_at: assessed_at.to_string(),
        evidence_refs: link
            .evidence
            .iter()
            .map(|item| item.source_ref.clone())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Ref, TraceKind, SCHEMA_VERSION};

    fn link(state: TraceState) -> TraceLink {
        let source = Ref::new("code:a", "code", "rev-1");
        let target = Ref::new("req:1", "requirement", "rev-1");
        TraceLink {
            reference: Ref::new("link:1", "trace_link", "\"1\""),
            source: source.clone(),
            target: target.clone(),
            kind: TraceKind::Implements,
            state,
            evidence: vec![Evidence {
                evidence_id: "e1".into(),
                source_ref: "artifact:sha256:aa".into(),
                confidence: 0.9,
            }],
            rationale: Some("implements the requirement".into()),
            created_at: format!("{SCHEMA_VERSION}@0"),
            verified_at: if matches!(
                state,
                TraceState::Validated
                    | TraceState::Accepted
                    | TraceState::Stale
                    | TraceState::Revoked
            ) {
                Some("1".to_string())
            } else {
                None
            },
            source_revision: "rev-1".into(),
            target_revision: "rev-1".into(),
            etag: "\"1\"".into(),
            validity: ValidityVector {
                historical_validity: HistoricalValidity::Unverified,
                current_applicability: CurrentApplicability::Unknown,
                index_freshness: IndexFreshness::Unknown,
                asserted_source_revision: "rev-1".into(),
                observed_head_revision: None,
                assessed_at: "0".into(),
                evidence_refs: Vec::new(),
            },
        }
    }

    #[test]
    fn the_transition_matrix_is_the_registered_one() {
        use TraceState::*;
        for (from, to) in [
            (Proposed, Validated),
            (Proposed, Rejected),
            (Proposed, Revoked),
            (Validated, Accepted),
            (Validated, Rejected),
            (Validated, Stale),
            (Validated, Revoked),
            (Accepted, Stale),
            (Accepted, Revoked),
            (Stale, Validated),
            (Stale, Revoked),
        ] {
            assert!(can_transition(from, to), "{from:?} -> {to:?}");
        }
        for (from, to) in [
            (Proposed, Accepted),
            (Proposed, Stale),
            (Accepted, Validated),
            (Accepted, Rejected),
            (Stale, Accepted),
            (Rejected, Validated),
            (Revoked, Validated),
            (Rejected, Revoked),
        ] {
            assert!(!can_transition(from, to), "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn transitions_move_the_state_and_stamp_verification() {
        let mut candidate = link(TraceState::Proposed);
        transition(&mut candidate, TraceState::Validated, "100").unwrap();
        assert_eq!(candidate.state, TraceState::Validated);
        assert_eq!(candidate.verified_at.as_deref(), Some("100"));
        transition(&mut candidate, TraceState::Accepted, "200").unwrap();
        assert_eq!(candidate.state, TraceState::Accepted);
        assert_eq!(
            transition(&mut candidate, TraceState::Rejected, "300")
                .unwrap_err()
                .code(),
            "LINK_STATE_CONFLICT"
        );
    }

    #[test]
    fn rejected_and_revoked_are_terminal() {
        for terminal in [TraceState::Rejected, TraceState::Revoked] {
            let mut dead = link(terminal);
            for target in [
                TraceState::Validated,
                TraceState::Accepted,
                TraceState::Stale,
            ] {
                assert_eq!(
                    transition(&mut dead, target, "1").unwrap_err().code(),
                    "LINK_STATE_CONFLICT"
                );
            }
        }
    }

    #[test]
    fn evidence_is_immutable_after_acceptance() {
        let mut accepted = link(TraceState::Accepted);
        assert_eq!(
            append_evidence(
                &mut accepted,
                &[Evidence {
                    evidence_id: "e2".into(),
                    source_ref: "artifact:sha256:bb".into(),
                    confidence: 0.8,
                }]
            )
            .unwrap_err()
            .code(),
            "LINK_STATE_CONFLICT"
        );

        let mut proposed = link(TraceState::Proposed);
        append_evidence(
            &mut proposed,
            &[Evidence {
                evidence_id: "e2".into(),
                source_ref: "artifact:sha256:bb".into(),
                confidence: 0.8,
            }],
        )
        .unwrap();
        assert_eq!(proposed.evidence.len(), 2);
        // Duplicate ids are not stored twice.
        append_evidence(
            &mut proposed,
            &[Evidence {
                evidence_id: "e2".into(),
                source_ref: "artifact:sha256:bb".into(),
                confidence: 0.8,
            }],
        )
        .unwrap();
        assert_eq!(proposed.evidence.len(), 2);

        let mut bad = link(TraceState::Proposed);
        assert!(append_evidence(
            &mut bad,
            &[Evidence {
                evidence_id: "e3".into(),
                source_ref: "artifact:sha256:cc".into(),
                confidence: 2.0,
            }]
        )
        .is_err());
    }

    #[test]
    fn historical_validity_is_not_changed_by_a_new_head() {
        let accepted = link(TraceState::Accepted);
        let before = assess(
            &accepted,
            &EndpointHeads::matching(&accepted),
            IndexFreshness::Current,
            "10",
        );
        assert_eq!(before.historical_validity, HistoricalValidity::Valid);
        assert_eq!(
            before.current_applicability,
            CurrentApplicability::Applicable
        );

        let after = assess(
            &accepted,
            &EndpointHeads::moved(&accepted),
            IndexFreshness::Current,
            "20",
        );
        assert_eq!(
            after.historical_validity,
            HistoricalValidity::Valid,
            "history stays true"
        );
        assert_eq!(
            after.current_applicability,
            CurrentApplicability::RequiresRevalidation
        );
        assert_eq!(after.observed_head_revision.as_deref(), Some("rev-1-next"));
        assert_eq!(after.asserted_source_revision, "rev-1");
    }

    #[test]
    fn applicability_covers_unknown_and_terminal_links() {
        let proposed = link(TraceState::Proposed);
        assert_eq!(
            current_applicability(&proposed, &EndpointHeads::unknown()),
            CurrentApplicability::Unknown
        );
        assert_eq!(
            historical_validity(&proposed),
            HistoricalValidity::Unverified
        );

        let rejected = link(TraceState::Rejected);
        assert_eq!(historical_validity(&rejected), HistoricalValidity::Invalid);
        assert_eq!(
            current_applicability(&rejected, &EndpointHeads::matching(&rejected)),
            CurrentApplicability::NotApplicable
        );

        let revoked = link(TraceState::Revoked);
        assert_eq!(
            current_applicability(&revoked, &EndpointHeads::matching(&revoked)),
            CurrentApplicability::NotApplicable
        );
        assert_eq!(
            historical_validity(&revoked),
            HistoricalValidity::Valid,
            "revoking a verified link does not rewrite history"
        );
        let mut unverified_revoke = link(TraceState::Revoked);
        unverified_revoke.verified_at = None;
        assert_eq!(
            historical_validity(&unverified_revoke),
            HistoricalValidity::Unverified
        );
    }
}
