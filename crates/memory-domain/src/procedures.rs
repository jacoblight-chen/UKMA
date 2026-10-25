//! Procedure lifecycle `LEARN → CONSOLIDATE → APPLY → REFINE` plus
//! `DISABLE`/`REVOKE` (§27.2, MAPI-20/21/22, PM-001).
//!
//! A procedure never grants a tool: `APPLY` still needs a host capability grant,
//! and the success rate is a bounded Beta posterior so one lucky run cannot claim
//! a perfect skill.

use std::collections::BTreeSet;

use crate::domain::{Evidence, MemoryError, ProcedureState, Result};

/// Runs required before consolidation (§29.2 `consolidate_calls>=3`).
pub const CONSOLIDATE_MIN_CALLS: u32 = 3;
/// Success rate required before consolidation (§29.2 `min_success_rate=.65`).
pub const CONSOLIDATE_MIN_SUCCESS: f64 = 0.65;
/// Runs that force a refinement review (§29.2 `refine_calls>=5`).
pub const REFINE_MIN_CALLS: u32 = 5;
/// Success rate that forces a refinement review (§29.2 `refine_at_success_rate<=.5`).
pub const REFINE_MAX_SUCCESS: f64 = 0.5;

/// What `consolidate` decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationDecision {
    /// Moved to `CONSOLIDATED`; it may now be activated.
    Approved,
    /// Not enough evidence yet; it stays a candidate.
    ReviewRequired,
}

impl ConsolidationDecision {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            ConsolidationDecision::Approved => "approved",
            ConsolidationDecision::ReviewRequired => "review_required",
        }
    }
}

/// Why a procedure action was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureAction {
    /// Promote a candidate after success.
    Learn,
    /// Approve or hold a candidate.
    Consolidate,
    /// Run an approved procedure.
    Apply,
    /// Send a procedure back for refinement.
    Refine,
    /// Disable a procedure.
    Disable,
}

/// A learned procedure (§21.4, §34.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Procedure {
    /// Procedure id.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// BCP-47 language.
    pub language: String,
    /// Ordered steps.
    pub steps: Vec<String>,
    /// Preconditions that must hold before a run.
    pub preconditions: Vec<String>,
    /// Postconditions the run must satisfy.
    pub postconditions: Vec<String>,
    /// Success criteria.
    pub success_criteria: Vec<String>,
    /// Tools the host would have to grant (never granted by memory).
    pub tool_scope: Vec<String>,
    /// Evidence supporting the procedure.
    pub evidence: Vec<Evidence>,
    /// Episodes it was learned from.
    pub source_episodes: Vec<String>,
    /// Observed runs.
    pub calls: u32,
    /// Successful runs.
    pub successes: u32,
    /// Failed runs.
    pub failures: u32,
    /// Lifecycle state.
    pub state: ProcedureState,
    /// Revision number.
    pub version: u64,
    /// Instant of the last review.
    pub last_verified_at: Option<i64>,
}

impl Procedure {
    /// A freshly learned candidate (§27.2 `LEARN`).
    pub fn learn(
        id: impl Into<String>,
        name: impl Into<String>,
        language: impl Into<String>,
        evidence: Vec<Evidence>,
        source_episodes: Vec<String>,
    ) -> Result<Self> {
        if evidence.is_empty() {
            return Err(MemoryError::EvidenceRequired {
                operation: "memory.register_procedure_candidate".to_string(),
            });
        }
        for item in &evidence {
            item.validate()?;
        }
        if source_episodes.is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "source_episodes".to_string(),
                reason: "a procedure must name the episodes it was learned from".to_string(),
            });
        }
        Ok(Self {
            id: id.into(),
            name: name.into(),
            language: language.into(),
            steps: Vec::new(),
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            success_criteria: Vec::new(),
            tool_scope: Vec::new(),
            evidence,
            source_episodes,
            calls: 0,
            successes: 0,
            failures: 0,
            state: ProcedureState::Candidate,
            version: 1,
            last_verified_at: None,
        })
    }

    /// Independent source groups behind the procedure.
    pub fn independent_groups(&self) -> BTreeSet<&str> {
        self.evidence
            .iter()
            .map(|item| item.source_group.as_str())
            .collect()
    }

    /// Raw success rate; `0.0` when it has never run.
    pub fn raw_success_rate(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            f64::from(self.successes) / f64::from(self.calls)
        }
    }

    /// Bounded Beta posterior mean of the success rate (§27.2).
    ///
    /// One success gives `2/3`, not `1.0`; the sample size stays visible through
    /// [`Procedure::calls`].
    pub fn success_rate(&self) -> f64 {
        let alpha = 1.0 + f64::from(self.successes);
        let beta = 1.0 + f64::from(self.failures);
        alpha / (alpha + beta)
    }

    /// Whether the procedure may run.
    ///
    /// This is a *state* check only: the host capability grant is separate and is
    /// never implied by an active procedure (§21.4).
    pub fn can_apply(&self) -> bool {
        self.state == ProcedureState::Active
    }

    /// Whether the run counters now require a refinement review (§27.2).
    pub fn needs_refinement(&self) -> bool {
        self.calls >= REFINE_MIN_CALLS
            || (self.calls > 0 && self.raw_success_rate() <= REFINE_MAX_SUCCESS)
    }

    /// Whether the run counters satisfy the consolidation gate (§27.2).
    ///
    /// This is a pure check, so a refused activation does not advance the
    /// procedure revision.
    pub fn consolidation_ready(&self) -> bool {
        self.calls >= CONSOLIDATE_MIN_CALLS && self.raw_success_rate() >= CONSOLIDATE_MIN_SUCCESS
    }

    /// Record one run outcome.
    ///
    /// Trials may be recorded while the procedure is still a candidate: the
    /// `LEARN → CONSOLIDATE` decision needs run statistics, and only `APPLY` of a
    /// consolidated procedure is a production run (§27.2).
    pub fn record_outcome(&mut self, success: bool) -> Result<()> {
        if self.state == ProcedureState::Disabled {
            return Err(MemoryError::StateConflict {
                resource: self.id.clone(),
                state: procedure_state_token(self.state).to_string(),
                operation: "record_procedure_outcome".to_string(),
            });
        }
        self.calls += 1;
        if success {
            self.successes += 1;
        } else {
            self.failures += 1;
        }
        self.version += 1;
        Ok(())
    }

    /// `CONSOLIDATE`: approve a candidate when the gates hold, otherwise hold it
    /// for review.
    pub fn consolidate(&mut self, now: i64) -> Result<ConsolidationDecision> {
        if self.state != ProcedureState::Candidate {
            return Err(MemoryError::StateConflict {
                resource: self.id.clone(),
                state: procedure_state_token(self.state).to_string(),
                operation: "consolidate".to_string(),
            });
        }
        let approved = self.calls >= CONSOLIDATE_MIN_CALLS
            && self.raw_success_rate() >= CONSOLIDATE_MIN_SUCCESS;
        if approved {
            self.state = ProcedureState::Consolidated;
            self.last_verified_at = Some(now);
            self.version += 1;
            Ok(ConsolidationDecision::Approved)
        } else {
            self.version += 1;
            Ok(ConsolidationDecision::ReviewRequired)
        }
    }

    /// `APPLY`: a review decision activates, refines or disables the procedure.
    pub fn review(&mut self, decision: ProcedureReview, now: i64) -> Result<()> {
        let next = match decision {
            ProcedureReview::Activate => {
                if !matches!(
                    self.state,
                    ProcedureState::Consolidated | ProcedureState::Refining
                ) {
                    return Err(MemoryError::StateConflict {
                        resource: self.id.clone(),
                        state: procedure_state_token(self.state).to_string(),
                        operation: "activate".to_string(),
                    });
                }
                ProcedureState::Active
            }
            ProcedureReview::Refine => {
                if self.state == ProcedureState::Disabled {
                    return Err(MemoryError::StateConflict {
                        resource: self.id.clone(),
                        state: procedure_state_token(self.state).to_string(),
                        operation: "refine".to_string(),
                    });
                }
                ProcedureState::Refining
            }
            ProcedureReview::Disable => ProcedureState::Disabled,
        };
        self.state = next;
        self.last_verified_at = Some(now);
        self.version += 1;
        Ok(())
    }
}

/// A review decision for a procedure (§34.2 `memory.review_procedure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureReview {
    /// Activate it.
    Activate,
    /// Send it back for refinement.
    Refine,
    /// Disable it.
    Disable,
}

/// Wire token of a procedure state.
pub fn procedure_state_token(state: ProcedureState) -> &'static str {
    match state {
        ProcedureState::Learn => "learn",
        ProcedureState::Candidate => "candidate",
        ProcedureState::Consolidated => "consolidated",
        ProcedureState::Active => "active",
        ProcedureState::Refining => "refine",
        ProcedureState::Disabled => "disabled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(group: &str) -> Evidence {
        Evidence {
            evidence_id: format!("e-{group}"),
            source_ref: "artifact:sha256:aa".to_string(),
            confidence: 0.9,
            source_group: group.to_string(),
        }
    }

    fn candidate() -> Procedure {
        let mut procedure = Procedure::learn(
            "proc:1",
            "retry the upload",
            "en",
            vec![evidence("doc-a")],
            vec!["ep:1".to_string()],
        )
        .unwrap();
        procedure.steps = vec!["open".into(), "retry".into()];
        procedure.tool_scope = vec!["upload".into()];
        procedure
    }

    #[test]
    fn learning_requires_evidence_and_a_source_episode() {
        assert_eq!(
            Procedure::learn("p", "n", "en", Vec::new(), vec!["ep:1".into()])
                .unwrap_err()
                .code(),
            "EVIDENCE_REQUIRED"
        );
        assert_eq!(
            Procedure::learn("p", "n", "en", vec![evidence("doc-a")], Vec::new())
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
        let procedure = candidate();
        assert_eq!(procedure.state, ProcedureState::Candidate);
        assert_eq!(procedure.version, 1);
    }

    #[test]
    fn success_rate_is_a_bounded_beta_posterior() {
        let mut procedure = candidate();
        assert_eq!(procedure.raw_success_rate(), 0.0);
        assert!((procedure.success_rate() - 0.5).abs() < 1e-12);
        procedure.record_outcome(true).unwrap();
        assert_eq!(procedure.calls, 1, "a candidate may run trials");

        let mut approved = candidate();
        for _ in 0..3 {
            approved.record_outcome(true).unwrap();
        }
        assert_eq!(approved.raw_success_rate(), 1.0);
        assert!((approved.success_rate() - 4.0 / 5.0).abs() < 1e-12);
        assert!(approved.success_rate() < 1.0, "one lucky run is not 100%");
        assert_eq!(approved.calls, 3);
    }

    #[test]
    fn consolidation_needs_three_runs_and_the_success_gate() {
        let mut early = candidate();
        assert_eq!(
            early.consolidate(10).unwrap(),
            ConsolidationDecision::ReviewRequired
        );
        assert_eq!(early.state, ProcedureState::Candidate);

        let mut good = candidate();
        for _ in 0..3 {
            good.record_outcome(true).unwrap();
        }
        assert_eq!(
            good.consolidate(10).unwrap(),
            ConsolidationDecision::Approved
        );
        assert_eq!(good.state, ProcedureState::Consolidated);
        assert_eq!(good.last_verified_at, Some(10));

        let mut poor = candidate();
        poor.state = ProcedureState::Active;
        for index in 0..3 {
            poor.record_outcome(index == 0).unwrap();
        }
        poor.state = ProcedureState::Candidate;
        assert_eq!(
            poor.consolidate(10).unwrap(),
            ConsolidationDecision::ReviewRequired
        );
        assert_eq!(
            poor.consolidate(10).unwrap(),
            ConsolidationDecision::ReviewRequired,
            "a failing procedure stays a candidate for another review"
        );
        assert_eq!(
            good.consolidate(10).unwrap_err().code(),
            "LINK_STATE_CONFLICT",
            "a consolidated procedure is not consolidated twice"
        );
    }

    #[test]
    fn activation_requires_consolidation_and_refinement_is_triggered_by_evidence() {
        let mut procedure = candidate();
        assert_eq!(
            procedure
                .review(ProcedureReview::Activate, 1)
                .unwrap_err()
                .code(),
            "LINK_STATE_CONFLICT"
        );

        for _ in 0..3 {
            procedure.record_outcome(true).unwrap();
        }
        procedure.consolidate(1).unwrap();
        procedure.review(ProcedureReview::Activate, 2).unwrap();
        assert_eq!(procedure.state, ProcedureState::Active);
        assert!(procedure.can_apply());

        for _ in 0..2 {
            procedure.record_outcome(false).unwrap();
        }
        assert!(
            procedure.needs_refinement(),
            "calls or success rate triggers"
        );
        procedure.review(ProcedureReview::Refine, 3).unwrap();
        assert_eq!(procedure.state, ProcedureState::Refining);
        assert!(!procedure.can_apply());

        procedure.review(ProcedureReview::Disable, 4).unwrap();
        assert_eq!(procedure.state, ProcedureState::Disabled);
        assert_eq!(
            procedure
                .review(ProcedureReview::Refine, 5)
                .unwrap_err()
                .code(),
            "LINK_STATE_CONFLICT"
        );
        assert_eq!(
            procedure.record_outcome(true).unwrap_err().code(),
            "LINK_STATE_CONFLICT"
        );
    }

    #[test]
    fn an_active_procedure_still_grants_no_tool() {
        let mut procedure = candidate();
        procedure.state = ProcedureState::Active;
        assert!(procedure.can_apply());
        assert_eq!(procedure.tool_scope, vec!["upload".to_string()]);
        assert_eq!(
            procedure_state_token(procedure.state),
            "active",
            "the state token carries no permission"
        );
    }
}
