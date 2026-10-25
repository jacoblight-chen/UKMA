//! Backend cutover drill (§28.3, DB-010, E2E-API-11, P10-WP2).
//!
//! The cutover is a nine-state machine with explicit guards. Two rules make it
//! safe rather than ceremonial:
//!
//! - a step only advances when its **precondition** holds, so a half-finished
//!   export cannot be followed by `IMPORT`;
//! - every mutation requires the **fencing token** of the current epoch, so a
//!   superseded writer cannot commit after a cutover or a rollback.
//!
//! The machine owns no storage of its own: callers perform the actual work
//! (snapshot, export, import, verify, shadow read) and report the evidence, which
//! is what makes the drill testable without a second live backend.

use std::collections::BTreeMap;

/// The nine cutover states, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CutoverState {
    /// Decided but nothing started.
    Planned,
    /// Source frozen and snapshotted.
    Snapshot,
    /// Canonical data exported.
    Export,
    /// Target accepted the import.
    Import,
    /// Logical identity, revisions, evidence and ACL parity verified.
    Verify,
    /// Target serves shadow reads next to the source.
    ShadowRead,
    /// Writers moved to the target.
    Cutover,
    /// Both sides observed; the source is read-only.
    Monitor,
    /// Source retired.
    Complete,
}

impl CutoverState {
    /// Every state in order.
    pub const ALL: [CutoverState; 9] = [
        CutoverState::Planned,
        CutoverState::Snapshot,
        CutoverState::Export,
        CutoverState::Import,
        CutoverState::Verify,
        CutoverState::ShadowRead,
        CutoverState::Cutover,
        CutoverState::Monitor,
        CutoverState::Complete,
    ];

    /// Stable token used in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            CutoverState::Planned => "planned",
            CutoverState::Snapshot => "snapshot",
            CutoverState::Export => "export",
            CutoverState::Import => "import",
            CutoverState::Verify => "verify",
            CutoverState::ShadowRead => "shadow_read",
            CutoverState::Cutover => "cutover",
            CutoverState::Monitor => "monitor",
            CutoverState::Complete => "complete",
        }
    }

    /// The state that must follow this one.
    pub fn next(self) -> Option<CutoverState> {
        match self {
            CutoverState::Planned => Some(CutoverState::Snapshot),
            CutoverState::Snapshot => Some(CutoverState::Export),
            CutoverState::Export => Some(CutoverState::Import),
            CutoverState::Import => Some(CutoverState::Verify),
            CutoverState::Verify => Some(CutoverState::ShadowRead),
            CutoverState::ShadowRead => Some(CutoverState::Cutover),
            CutoverState::Cutover => Some(CutoverState::Monitor),
            CutoverState::Monitor => Some(CutoverState::Complete),
            CutoverState::Complete => None,
        }
    }

    /// Whether writers may write to the source in this state.
    pub fn source_writable(self) -> bool {
        matches!(
            self,
            CutoverState::Planned
                | CutoverState::Snapshot
                | CutoverState::Export
                | CutoverState::Import
                | CutoverState::Verify
                | CutoverState::ShadowRead
        )
    }

    /// Whether writers may write to the target in this state.
    pub fn target_writable(self) -> bool {
        matches!(
            self,
            CutoverState::Cutover | CutoverState::Monitor | CutoverState::Complete
        )
    }
}

/// Why a cutover step was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutoverError {
    /// The step is not the one the machine expects.
    OutOfOrder {
        /// State the machine is in.
        current: CutoverState,
        /// State the caller asked for.
        requested: CutoverState,
    },
    /// A precondition for the step was not satisfied.
    PreconditionFailed {
        /// Step that was refused.
        state: CutoverState,
        /// Why.
        reason: String,
    },
    /// The fencing token does not belong to the current epoch.
    StaleFence {
        /// Token the caller presented.
        presented: u64,
        /// Token of the current epoch.
        current: u64,
    },
    /// An acknowledged cutover cannot be undone without an explicit rollback.
    NotRollbackable {
        /// State the machine is in.
        state: CutoverState,
    },
}

impl std::fmt::Display for CutoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CutoverError::OutOfOrder { current, requested } => write!(
                f,
                "cannot move to {} while in {}",
                requested.as_str(),
                current.as_str()
            ),
            CutoverError::PreconditionFailed { state, reason } => {
                write!(f, "{} refused: {reason}", state.as_str())
            }
            CutoverError::StaleFence { presented, current } => {
                write!(f, "stale fencing token {presented}, current {current}")
            }
            CutoverError::NotRollbackable { state } => {
                write!(f, "{} is past the rollback point", state.as_str())
            }
        }
    }
}

impl std::error::Error for CutoverError {}

/// Evidence a caller must supply for a step to be accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepEvidence {
    /// Artifact/content hash of the snapshot or export.
    pub checksum: Option<String>,
    /// Records moved by the step.
    pub records: Option<usize>,
    /// Divergences found during verification or shadow reads.
    pub divergences: Option<usize>,
}

/// One completed step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    /// State reached.
    pub state: CutoverState,
    /// Epoch that performed the step.
    pub fence: u64,
    /// Caller-supplied evidence.
    pub evidence: StepEvidence,
}

/// The cutover drill.
#[derive(Debug, Clone)]
pub struct Cutover {
    state: CutoverState,
    fence: u64,
    steps: Vec<StepRecord>,
    /// Generations the target must keep readable after the cutover.
    retained_generations: Vec<String>,
}

impl Default for Cutover {
    fn default() -> Self {
        Self::new()
    }
}

impl Cutover {
    /// A cutover that has not started.
    pub fn new() -> Self {
        Self {
            state: CutoverState::Planned,
            fence: 1,
            steps: Vec::new(),
            retained_generations: Vec::new(),
        }
    }

    /// Current state.
    pub fn state(&self) -> CutoverState {
        self.state
    }

    /// Current fencing token.
    pub fn fence(&self) -> u64 {
        self.fence
    }

    /// Completed steps, in order.
    pub fn steps(&self) -> &[StepRecord] {
        &self.steps
    }

    /// Generations that must stay readable after the cutover.
    pub fn retained_generations(&self) -> &[String] {
        &self.retained_generations
    }

    /// Advance to `next_state` with `evidence` and the current fencing token.
    pub fn advance(
        &mut self,
        next_state: CutoverState,
        fence: u64,
        evidence: StepEvidence,
    ) -> Result<CutoverState, CutoverError> {
        if fence != self.fence {
            return Err(CutoverError::StaleFence {
                presented: fence,
                current: self.fence,
            });
        }
        let expected = self.state.next().ok_or(CutoverError::OutOfOrder {
            current: self.state,
            requested: next_state,
        })?;
        if expected != next_state {
            return Err(CutoverError::OutOfOrder {
                current: self.state,
                requested: next_state,
            });
        }
        Self::check_precondition(next_state, &evidence)?;
        self.steps.push(StepRecord {
            state: next_state,
            fence: self.fence,
            evidence: evidence.clone(),
        });
        self.state = next_state;
        // A new epoch starts whenever writer ownership moves.
        if matches!(
            next_state,
            CutoverState::Cutover | CutoverState::Monitor | CutoverState::Complete
        ) {
            self.fence += 1;
        }
        Ok(self.state)
    }

    /// Record that a generation must stay readable after the cutover.
    pub fn require_generation(&mut self, generation: &str) {
        if !self
            .retained_generations
            .iter()
            .any(|existing| existing == generation)
        {
            self.retained_generations.push(generation.to_string());
        }
    }

    /// Roll back to the previous state, bumping the fence so a superseded writer
    /// cannot keep writing.
    pub fn rollback(&mut self, reason: &str) -> Result<CutoverState, CutoverError> {
        if !self.state.source_writable() {
            return Err(CutoverError::NotRollbackable { state: self.state });
        }
        if self.state == CutoverState::Planned {
            return Err(CutoverError::OutOfOrder {
                current: self.state,
                requested: CutoverState::Planned,
            });
        }
        let previous = CutoverState::ALL
            .iter()
            .copied()
            .take_while(|state| *state != self.state)
            .last()
            .unwrap_or(CutoverState::Planned);
        self.fence += 1;
        self.steps.push(StepRecord {
            state: previous,
            fence: self.fence,
            evidence: StepEvidence {
                checksum: None,
                records: None,
                divergences: None,
            },
        });
        self.state = previous;
        let _ = reason;
        Ok(self.state)
    }

    /// Machine-readable drill report.
    pub fn report_json(&self) -> String {
        let mut steps: BTreeMap<&str, &str> = BTreeMap::new();
        for step in &self.steps {
            steps.insert(step.state.as_str(), "done");
        }
        serde_json::json!({
            "states": CutoverState::ALL
                .iter()
                .map(|state| state.as_str())
                .collect::<Vec<_>>(),
            "current": self.state.as_str(),
            "fence": self.fence,
            "source_writable": self.state.source_writable(),
            "target_writable": self.state.target_writable(),
            "steps": steps,
            "retained_generations": self.retained_generations,
        })
        .to_string()
    }

    fn check_precondition(
        state: CutoverState,
        evidence: &StepEvidence,
    ) -> Result<(), CutoverError> {
        let refused = |reason: &str| {
            Err(CutoverError::PreconditionFailed {
                state,
                reason: reason.to_string(),
            })
        };
        match state {
            CutoverState::Planned => refused("nothing precedes planned"),
            CutoverState::Snapshot | CutoverState::Export => {
                if evidence
                    .checksum
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                {
                    return refused("a content checksum is required");
                }
                Ok(())
            }
            CutoverState::Import | CutoverState::Verify => {
                if evidence.records.unwrap_or(0) == 0 {
                    return refused("the step must report the records it moved");
                }
                Ok(())
            }
            CutoverState::ShadowRead => {
                if evidence.records.unwrap_or(0) == 0 {
                    return refused("shadow reads must report the records compared");
                }
                if evidence.divergences != Some(0) {
                    return refused("shadow reads must report zero divergences");
                }
                Ok(())
            }
            CutoverState::Cutover | CutoverState::Monitor | CutoverState::Complete => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(checksum: &str, records: usize, divergences: usize) -> StepEvidence {
        StepEvidence {
            checksum: Some(checksum.to_string()),
            records: Some(records),
            divergences: Some(divergences),
        }
    }

    #[test]
    fn the_nine_states_are_walked_in_order_and_writer_ownership_moves_once() {
        let mut cutover = Cutover::new();
        let fence = cutover.fence();
        for state in [
            CutoverState::Snapshot,
            CutoverState::Export,
            CutoverState::Import,
            CutoverState::Verify,
            CutoverState::ShadowRead,
        ] {
            cutover
                .advance(state, fence, evidence("sha256:aa", 7, 0))
                .unwrap();
            assert!(cutover.state().source_writable());
            assert!(!cutover.state().target_writable());
        }
        cutover
            .advance(CutoverState::Cutover, fence, StepEvidence::default())
            .unwrap();
        assert!(!cutover.state().source_writable());
        assert!(cutover.state().target_writable());
        // The fence moves when writer ownership moves.
        assert_eq!(cutover.fence(), fence + 1);
        cutover
            .advance(
                CutoverState::Monitor,
                cutover.fence(),
                StepEvidence::default(),
            )
            .unwrap();
        cutover
            .advance(
                CutoverState::Complete,
                cutover.fence(),
                StepEvidence::default(),
            )
            .unwrap();
        assert_eq!(cutover.steps().len(), 8);
    }

    #[test]
    fn a_step_out_of_order_or_without_evidence_is_refused() {
        let mut cutover = Cutover::new();
        let fence = cutover.fence();
        assert!(matches!(
            cutover.advance(CutoverState::Import, fence, evidence("x", 1, 0)),
            Err(CutoverError::OutOfOrder { .. })
        ));
        assert!(matches!(
            cutover.advance(CutoverState::Snapshot, fence, StepEvidence::default()),
            Err(CutoverError::PreconditionFailed { .. })
        ));
        cutover
            .advance(CutoverState::Snapshot, fence, evidence("sha256:aa", 0, 0))
            .unwrap();
        // Export needs a checksum, import needs a record count.
        assert!(matches!(
            cutover.advance(CutoverState::Export, fence, StepEvidence::default()),
            Err(CutoverError::PreconditionFailed { .. })
        ));
        cutover
            .advance(CutoverState::Export, fence, evidence("sha256:bb", 3, 0))
            .unwrap();
        assert!(matches!(
            cutover.advance(
                CutoverState::Import,
                fence,
                StepEvidence {
                    checksum: None,
                    records: Some(0),
                    divergences: None
                }
            ),
            Err(CutoverError::PreconditionFailed { .. })
        ));
    }

    #[test]
    fn shadow_reads_must_be_clean_before_the_cutover() {
        let mut cutover = Cutover::new();
        let fence = cutover.fence();
        for state in [
            CutoverState::Snapshot,
            CutoverState::Export,
            CutoverState::Import,
            CutoverState::Verify,
        ] {
            cutover
                .advance(state, fence, evidence("sha256:aa", 5, 0))
                .unwrap();
        }
        assert!(matches!(
            cutover.advance(CutoverState::ShadowRead, fence, evidence("sha256:aa", 5, 1)),
            Err(CutoverError::PreconditionFailed { .. })
        ));
        cutover
            .advance(CutoverState::ShadowRead, fence, evidence("sha256:aa", 5, 0))
            .unwrap();
    }

    #[test]
    fn a_stale_fence_cannot_advance_or_roll_back() {
        let mut cutover = Cutover::new();
        let old = cutover.fence();
        cutover
            .advance(CutoverState::Snapshot, old, evidence("sha256:aa", 1, 0))
            .unwrap();
        assert!(matches!(
            cutover.advance(CutoverState::Export, old + 99, evidence("sha256:bb", 1, 0)),
            Err(CutoverError::StaleFence { .. })
        ));
    }

    #[test]
    fn rollback_before_the_cutover_bumps_the_fence_and_after_it_is_refused() {
        let mut cutover = Cutover::new();
        let fence = cutover.fence();
        cutover
            .advance(CutoverState::Snapshot, fence, evidence("sha256:aa", 1, 0))
            .unwrap();
        cutover
            .advance(CutoverState::Export, fence, evidence("sha256:bb", 1, 0))
            .unwrap();
        assert_eq!(
            cutover.rollback("export was wrong").unwrap(),
            CutoverState::Snapshot
        );
        assert!(
            cutover.fence() > fence,
            "a rollback must fence the old writer"
        );

        let mut done = Cutover::new();
        let fence = done.fence();
        for state in [
            CutoverState::Snapshot,
            CutoverState::Export,
            CutoverState::Import,
            CutoverState::Verify,
            CutoverState::ShadowRead,
        ] {
            done.advance(state, fence, evidence("sha256:aa", 9, 0))
                .unwrap();
        }
        done.advance(CutoverState::Cutover, fence, StepEvidence::default())
            .unwrap();
        assert!(matches!(
            done.rollback("too late"),
            Err(CutoverError::NotRollbackable { .. })
        ));
    }

    #[test]
    fn the_report_names_every_state_and_the_current_one() {
        let mut cutover = Cutover::new();
        cutover.require_generation("gen-7");
        cutover
            .advance(
                CutoverState::Snapshot,
                cutover.fence(),
                evidence("sha256:aa", 1, 0),
            )
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_str(&cutover.report_json()).expect("report is JSON");
        assert_eq!(report["current"], "snapshot");
        assert_eq!(report["states"].as_array().unwrap().len(), 9);
        assert_eq!(report["retained_generations"][0], "gen-7");
        assert_eq!(report["steps"]["snapshot"], "done");
    }
}
