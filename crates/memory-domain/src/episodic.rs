//! Episodic memory lifecycle, active TTL sweep and purge guards (§21.3, §23.1–§23.2).
//!
//! The sweep never purges on its own authority: it produces an action and the
//! guards are explicit, so a caller cannot accidentally delete held or
//! review-pending evidence by running a TTL pass.

use crate::domain::{EpisodeOutcome, EpisodeState, EpisodeType, MemoryError, Result, Trust};
use crate::fsrs::{self, FsrsState, RetentionGate};

/// One episodic memory record (§21.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    /// Memory id.
    pub id: String,
    /// Session that produced it.
    pub session_id: String,
    /// Task it belongs to.
    pub task_id: String,
    /// Writing agent instance.
    pub agent_id: String,
    /// Delegation depth of the writer.
    pub depth: u32,
    /// Creation instant (Unix seconds).
    pub created_at: i64,
    /// Episode type.
    pub episode_type: EpisodeType,
    /// Summary text.
    pub summary: String,
    /// Authorised reference to the full text.
    pub full_text_ref: Option<String>,
    /// Key facts.
    pub key_facts: Vec<String>,
    /// Outcome.
    pub outcome: EpisodeOutcome,
    /// Importance in `[0,1]`.
    pub importance_score: f64,
    /// Current fold depth.
    pub fold_level: u32,
    /// Parent episode when this one was folded from another.
    pub parent_episode_id: Option<String>,
    /// Sources of this episode.
    pub source_refs: Vec<String>,
    /// Trust columns.
    pub trust: Trust,
    /// Lifecycle state.
    pub state: EpisodeState,
    /// State the record was in before it was held.
    pub held_from: Option<EpisodeState>,
    /// Active window end.
    pub active_until: i64,
    /// Purge-eligible instant.
    pub purge_after: i64,
    /// FSRS state.
    pub fsrs: FsrsState,
    /// Instant of the last successful review.
    pub last_review: i64,
    /// Legal hold flag (§22.2 R8).
    pub legal_hold: bool,
    /// Explicitly pinned by the user (§22.2 R6).
    pub pinned: bool,
    /// A review is pending, so the record cannot be deleted.
    pub pending_review: bool,
    /// A job currently depends on the record.
    pub active_job: bool,
    /// References that must never be broken by a deletion.
    pub protected_refs: Vec<String>,
    /// BCP-47 language.
    pub language: String,
}

impl Episode {
    /// Validate every bounded field and the lifecycle invariants.
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "id".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if !self.importance_score.is_finite() || !(0.0..=1.0).contains(&self.importance_score) {
            return Err(MemoryError::InvalidArgument {
                field: "importance_score".to_string(),
                reason: format!("must be in [0,1], got {}", self.importance_score),
            });
        }
        self.trust.validate()?;
        self.fsrs
            .validate()
            .map_err(|error| MemoryError::InvalidArgument {
                field: "fsrs_state".to_string(),
                reason: error.to_string(),
            })?;
        Ok(())
    }

    /// Current retrievability at `now`; a clock skew is reported, never clamped.
    pub fn retrievability(&self, now: i64) -> Result<f64> {
        fsrs::retrievability_at(self.last_review, now, self.fsrs.stability_days).map_err(|error| {
            MemoryError::InvalidInstant {
                detail: error.to_string(),
            }
        })
    }

    /// Transition the lifecycle state, refusing illegal moves.
    pub fn transition(&mut self, next: EpisodeState) -> Result<()> {
        if next == self.state {
            return Ok(());
        }
        if next == EpisodeState::Held {
            if !self.state.is_live() {
                return Err(self.conflict("be held"));
            }
            self.held_from = Some(self.state);
            self.state = EpisodeState::Held;
            return Ok(());
        }
        if self.state == EpisodeState::Held {
            return Err(self.conflict("leave a hold without a release"));
        }
        if !allowed(self.state, next) {
            return Err(self.conflict(&format!("become {}", next.as_str())));
        }
        self.state = next;
        Ok(())
    }

    /// Release a hold and return to the state it was held from.
    ///
    /// The original horizon is re-evaluated afterwards, so a released record
    /// does not gain lifetime (§21.3).
    pub fn release_hold(&mut self) -> Result<()> {
        if self.state != EpisodeState::Held {
            return Err(self.conflict("be released from a hold"));
        }
        let previous = self
            .held_from
            .take()
            .ok_or_else(|| MemoryError::StateConflict {
                resource: self.id.clone(),
                state: self.state.as_str().to_string(),
                operation: "release_hold".to_string(),
            })?;
        self.state = previous;
        Ok(())
    }

    fn conflict(&self, operation: &str) -> MemoryError {
        MemoryError::StateConflict {
            resource: self.id.clone(),
            state: self.state.as_str().to_string(),
            operation: operation.to_string(),
        }
    }
}

fn allowed(from: EpisodeState, to: EpisodeState) -> bool {
    use EpisodeState::{Active, Archived, Compressed, Expired, Purged, Tombstoned};
    match from {
        Active => matches!(to, Compressed | Archived | Expired),
        Compressed => matches!(to, Archived | Expired),
        Archived => matches!(to, Expired),
        Expired => matches!(to, Tombstoned),
        Tombstoned => matches!(to, Purged),
        Purged => false,
        EpisodeState::Held => false,
    }
}

/// Guards that must all pass before a record may be purged (§23.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PurgeGuards {
    /// The retention horizon has passed.
    pub retention_expired: bool,
    /// No job depends on the record.
    pub no_active_job: bool,
    /// No legal hold or explicit protection.
    pub no_hold: bool,
    /// No pinned evidence.
    pub no_pinned_evidence: bool,
    /// No review is pending.
    pub no_pending_review: bool,
    /// No protected reference points at the record.
    pub no_protected_refs: bool,
    /// Regulation allows the deletion.
    pub regulation_allows: bool,
}

impl PurgeGuards {
    /// Build the guards from a record.
    pub fn of(episode: &Episode, retention_expired: bool) -> Self {
        Self {
            retention_expired,
            no_active_job: !episode.active_job,
            no_hold: !episode.legal_hold && !episode.pinned,
            no_pinned_evidence: !episode.pinned,
            no_pending_review: !episode.pending_review,
            no_protected_refs: episode.protected_refs.is_empty(),
            regulation_allows: true,
        }
    }

    /// Every reason the guard set blocks a purge, in a stable order.
    pub fn blocking_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if !self.retention_expired {
            reasons.push("retention_horizon_not_reached");
        }
        if !self.no_active_job {
            reasons.push("active_job");
        }
        if !self.no_hold {
            reasons.push("hold");
        }
        if !self.no_pinned_evidence {
            reasons.push("pinned_evidence");
        }
        if !self.no_pending_review {
            reasons.push("pending_review");
        }
        if !self.no_protected_refs {
            reasons.push("protected_refs");
        }
        if !self.regulation_allows {
            reasons.push("regulation");
        }
        reasons
    }

    /// Whether a purge may proceed.
    pub fn permits_purge(&self) -> bool {
        self.blocking_reasons().is_empty()
    }
}

/// The action a sweep proposes for one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepAction {
    /// Nothing to do.
    Keep,
    /// Compress the body, keeping the source.
    Compress,
    /// Leave the active window but keep the record.
    Archive,
    /// Stop general queries and push a tombstone.
    Expire,
    /// Physically delete; every guard passed.
    Purge,
}

impl SweepAction {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            SweepAction::Keep => "keep",
            SweepAction::Compress => "compress",
            SweepAction::Archive => "archive",
            SweepAction::Expire => "expire",
            SweepAction::Purge => "purge",
        }
    }
}

/// Why a sweep finished the way it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepReason {
    /// Inside the active TTL window.
    ActiveWindow,
    /// Retention gate asked for compression.
    RetentionCompress,
    /// Past the active TTL but not purge-eligible.
    ArchivedNotPurgeable,
    /// Past the purge horizon but a guard blocked the purge.
    PurgeBlocked(Vec<&'static str>),
    /// Past the purge horizon with every guard satisfied.
    PurgeAllowed,
    /// Held: nothing happens until the hold is released.
    Held,
    /// Already tombstoned or purged.
    AlreadyGone,
    /// A hard privacy delete overrides FSRS (§23.1).
    PrivacyDelete,
}

/// Full sweep decision for one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepDecision {
    /// Chosen action.
    pub action: SweepAction,
    /// Why.
    pub reason: SweepReason,
    /// Retrievability at the sweep instant, when it could be computed.
    pub retrievability: Option<u8>,
}

/// Decide what a TTL/FSRS sweep does with a record.
///
/// `privacy_delete` is the hard privacy policy of §23.1: it outranks FSRS and
/// retention, but never legal hold.
pub fn decide_sweep(
    episode: &Episode,
    now: i64,
    privacy_delete: bool,
    regulation_allows: bool,
) -> Result<SweepDecision> {
    if matches!(
        episode.state,
        EpisodeState::Tombstoned | EpisodeState::Purged
    ) {
        return Ok(SweepDecision {
            action: SweepAction::Keep,
            reason: SweepReason::AlreadyGone,
            retrievability: None,
        });
    }
    let retrievability = if episode.state.is_live() {
        Some(
            (episode.retrievability(now)? * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8,
        )
    } else {
        None
    };

    if privacy_delete {
        if episode.legal_hold {
            return Ok(SweepDecision {
                action: SweepAction::Expire,
                reason: SweepReason::PurgeBlocked(vec!["legal_hold"]),
                retrievability,
            });
        }
        return Ok(SweepDecision {
            action: SweepAction::Purge,
            reason: SweepReason::PrivacyDelete,
            retrievability,
        });
    }

    if episode.state == EpisodeState::Held {
        return Ok(SweepDecision {
            action: SweepAction::Keep,
            reason: SweepReason::Held,
            retrievability,
        });
    }

    if now < episode.active_until {
        let gate = fsrs::retention_gate(retrievability.unwrap_or(100) as f64 / 100.0)?;
        return Ok(SweepDecision {
            action: if gate == RetentionGate::CompressCandidate {
                SweepAction::Compress
            } else {
                SweepAction::Keep
            },
            reason: if gate == RetentionGate::CompressCandidate {
                SweepReason::RetentionCompress
            } else {
                SweepReason::ActiveWindow
            },
            retrievability,
        });
    }

    let retention_expired = now >= episode.purge_after;
    let mut guards = PurgeGuards::of(episode, retention_expired);
    guards.regulation_allows = regulation_allows;
    let blocking = guards.blocking_reasons();
    let action = if blocking.is_empty() {
        SweepAction::Purge
    } else if retention_expired {
        SweepAction::Expire
    } else {
        SweepAction::Archive
    };
    Ok(SweepDecision {
        action,
        reason: if blocking.is_empty() {
            SweepReason::PurgeAllowed
        } else if retention_expired {
            SweepReason::PurgeBlocked(blocking)
        } else {
            SweepReason::ArchivedNotPurgeable
        },
        retrievability,
    })
}

/// Apply a sweep decision to a record, keeping the lifecycle legal.
pub fn apply_sweep(episode: &mut Episode, decision: &SweepDecision) -> Result<()> {
    match decision.action {
        SweepAction::Keep => Ok(()),
        SweepAction::Compress => advance_to(episode, EpisodeState::Compressed),
        SweepAction::Archive => advance_to(episode, EpisodeState::Archived),
        SweepAction::Expire => advance_to(episode, EpisodeState::Expired),
        SweepAction::Purge => {
            advance_to(episode, EpisodeState::Expired)?;
            advance_to(episode, EpisodeState::Tombstoned)?;
            advance_to(episode, EpisodeState::Purged)
        }
    }
}

/// Walk the legal path until the record reaches `target`.
fn advance_to(episode: &mut Episode, target: EpisodeState) -> Result<()> {
    while episode.state != target {
        let next = match episode.state {
            EpisodeState::Active => EpisodeState::Compressed,
            EpisodeState::Compressed => EpisodeState::Archived,
            EpisodeState::Archived => EpisodeState::Expired,
            EpisodeState::Expired => EpisodeState::Tombstoned,
            EpisodeState::Tombstoned => EpisodeState::Purged,
            EpisodeState::Held | EpisodeState::Purged => {
                return Err(MemoryError::StateConflict {
                    resource: episode.id.clone(),
                    state: episode.state.as_str().to_string(),
                    operation: format!("become {}", target.as_str()),
                })
            }
        };
        episode.transition(next)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsrs::S0_DAYS;

    fn episode(state: EpisodeState) -> Episode {
        Episode {
            id: "mem:a:b:1".into(),
            session_id: "s1".into(),
            task_id: "t1".into(),
            agent_id: "a1".into(),
            depth: 0,
            created_at: 0,
            episode_type: EpisodeType::ConversationTurn,
            summary: "did a thing".into(),
            full_text_ref: None,
            key_facts: vec!["fact".into()],
            outcome: EpisodeOutcome::Success,
            importance_score: 0.5,
            fold_level: 0,
            parent_episode_id: None,
            source_refs: vec![],
            trust: Trust::default(),
            state,
            held_from: None,
            active_until: 72 * 3_600,
            purge_after: 7 * 86_400,
            fsrs: FsrsState::default(),
            last_review: 0,
            legal_hold: false,
            pinned: false,
            pending_review: false,
            active_job: false,
            protected_refs: vec![],
            language: "en".into(),
        }
    }

    #[test]
    fn lifecycle_allows_the_registered_path_only() {
        let mut live = episode(EpisodeState::Active);
        live.transition(EpisodeState::Compressed).unwrap();
        live.transition(EpisodeState::Archived).unwrap();
        live.transition(EpisodeState::Expired).unwrap();
        live.transition(EpisodeState::Tombstoned).unwrap();
        live.transition(EpisodeState::Purged).unwrap();
        assert_eq!(live.state, EpisodeState::Purged);
        assert!(live.transition(EpisodeState::Active).is_err());
    }

    #[test]
    fn lifecycle_refuses_skipping_intermediate_states() {
        let mut live = episode(EpisodeState::Active);
        let error = live.transition(EpisodeState::Purged).unwrap_err();
        assert_eq!(error.code(), "LINK_STATE_CONFLICT");
        let mut archived = episode(EpisodeState::Archived);
        assert!(archived.transition(EpisodeState::Compressed).is_err());
        let mut expired = episode(EpisodeState::Expired);
        assert!(expired.transition(EpisodeState::Active).is_err());
    }

    #[test]
    fn hold_records_and_restores_the_previous_state() {
        let mut live = episode(EpisodeState::Compressed);
        live.transition(EpisodeState::Held).unwrap();
        assert_eq!(live.held_from, Some(EpisodeState::Compressed));
        // A held record cannot move anywhere else.
        assert!(live.transition(EpisodeState::Archived).is_err());
        live.release_hold().unwrap();
        assert_eq!(live.state, EpisodeState::Compressed);
        assert!(live.release_hold().is_err());
        let mut dead = episode(EpisodeState::Expired);
        assert!(dead.transition(EpisodeState::Held).is_err());
    }

    #[test]
    fn purge_guards_list_every_blocking_reason() {
        let mut record = episode(EpisodeState::Expired);
        record.legal_hold = true;
        record.pinned = true;
        record.pending_review = true;
        record.active_job = true;
        record.protected_refs.push("trace:1".into());
        let guards = PurgeGuards::of(&record, false);
        assert!(!guards.permits_purge());
        assert_eq!(
            guards.blocking_reasons(),
            vec![
                "retention_horizon_not_reached",
                "active_job",
                "hold",
                "pinned_evidence",
                "pending_review",
                "protected_refs"
            ]
        );
    }

    #[test]
    fn sweep_keeps_compresses_and_archives_by_horizon() {
        let record = episode(EpisodeState::Active);
        let inside = decide_sweep(&record, 3_600, false, true).unwrap();
        assert_eq!(inside.action, SweepAction::Keep);
        assert_eq!(inside.reason, SweepReason::ActiveWindow);

        let past_active = decide_sweep(&record, 73 * 3_600, false, true).unwrap();
        assert_eq!(past_active.action, SweepAction::Archive);
        assert_eq!(past_active.reason, SweepReason::ArchivedNotPurgeable);
    }

    #[test]
    fn sweep_purges_only_when_every_guard_passes() {
        let record = episode(EpisodeState::Archived);
        let now = 8 * 86_400;
        let purged = decide_sweep(&record, now, false, true).unwrap();
        assert_eq!(purged.action, SweepAction::Purge);
        assert_eq!(purged.reason, SweepReason::PurgeAllowed);

        let blocked = decide_sweep(&record, now, false, false).unwrap();
        assert_eq!(blocked.action, SweepAction::Expire);
        assert_eq!(
            blocked.reason,
            SweepReason::PurgeBlocked(vec!["regulation"])
        );
    }

    #[test]
    fn held_records_are_never_swept_but_privacy_delete_outranks_retention() {
        let mut record = episode(EpisodeState::Active);
        record.transition(EpisodeState::Held).unwrap();
        let held = decide_sweep(&record, 9 * 86_400, false, true).unwrap();
        assert_eq!(held.action, SweepAction::Keep);
        assert_eq!(held.reason, SweepReason::Held);

        let privacy = decide_sweep(&episode(EpisodeState::Active), 1, true, false).unwrap();
        assert_eq!(privacy.action, SweepAction::Purge);
        assert_eq!(privacy.reason, SweepReason::PrivacyDelete);
    }

    #[test]
    fn privacy_delete_still_respects_legal_hold() {
        let mut record = episode(EpisodeState::Active);
        record.legal_hold = true;
        let decision = decide_sweep(&record, 1, true, true).unwrap();
        assert_eq!(decision.action, SweepAction::Expire);
        assert_eq!(
            decision.reason,
            SweepReason::PurgeBlocked(vec!["legal_hold"])
        );
    }

    #[test]
    fn already_gone_records_are_left_alone() {
        for state in [EpisodeState::Tombstoned, EpisodeState::Purged] {
            let decision = decide_sweep(&episode(state), 10 * 86_400, true, true).unwrap();
            assert_eq!(decision.action, SweepAction::Keep);
            assert_eq!(decision.reason, SweepReason::AlreadyGone);
        }
    }

    #[test]
    fn sweep_rejects_clock_skew_instead_of_clamping() {
        let record = episode(EpisodeState::Active);
        let error = decide_sweep(&record, -1, false, true).unwrap_err();
        assert_eq!(error.code(), "INVALID_ARGUMENT");
    }

    #[test]
    fn apply_sweep_keeps_the_lifecycle_legal() {
        let mut record = episode(EpisodeState::Active);
        apply_sweep(
            &mut record,
            &SweepDecision {
                action: SweepAction::Archive,
                reason: SweepReason::ArchivedNotPurgeable,
                retrievability: Some(50),
            },
        )
        .unwrap();
        assert_eq!(record.state, EpisodeState::Archived);
        apply_sweep(
            &mut record,
            &SweepDecision {
                action: SweepAction::Purge,
                reason: SweepReason::PurgeAllowed,
                retrievability: Some(10),
            },
        )
        .unwrap();
        assert_eq!(record.state, EpisodeState::Purged);
        assert_eq!(S0_DAYS, record.fsrs.stability_days);
    }

    #[test]
    fn episode_validation_checks_bounds_and_fsrs() {
        let record = episode(EpisodeState::Active);
        assert!(record.validate().is_ok());
        let mut bad = record.clone();
        bad.importance_score = 2.0;
        assert!(bad.validate().is_err());
        let mut bad = record;
        bad.fsrs.stability_days = 0.0;
        assert!(bad.validate().is_err());
    }
}
