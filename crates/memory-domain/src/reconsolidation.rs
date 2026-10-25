//! Reconsolidation windows and immutable memory revisions (§23.4, §21.5).
//!
//! UKMA never edits a memory in place: every update appends a `MemoryRevision`
//! and moves a logical head pointer, so audit and rollback stay possible.

use crate::domain::{
    Evidence, MemoryError, MemoryPatch, ReconsolidationWindow, Result, WindowState,
};

/// Default labile window in hours (§23.4, §29.2 `window=4h`).
pub const DEFAULT_WINDOW_HOURS: f64 = 4.0;
/// Smallest configurable window (§23.4 `1–12h`).
pub const MIN_WINDOW_HOURS: f64 = 1.0;
/// Largest configurable window (§23.4 `1–12h`).
pub const MAX_WINDOW_HOURS: f64 = 12.0;
/// Temporary strength decay after a reconsolidation (§23.4 `strength_decay=0.8`).
pub const STRENGTH_DECAY: f64 = 0.8;

/// One immutable revision of a memory record.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRevision {
    /// Monotonic revision number, starting at 1.
    pub revision: u64,
    /// Content tag of this revision.
    pub etag: String,
    /// Summary at this revision.
    pub summary: String,
    /// Body at this revision.
    pub content: String,
    /// Key facts at this revision.
    pub key_facts: Vec<String>,
    /// Evidence attached at this revision.
    pub evidence: Vec<Evidence>,
    /// Instant the revision was created.
    pub created_at: i64,
    /// Principal that created it.
    pub author: String,
    /// Whether this revision is the current head.
    pub head: bool,
}

/// A record addressable by revision with a logical head pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRecord {
    /// Memory id.
    pub id: String,
    revisions: Vec<MemoryRevision>,
}

impl MemoryRecord {
    /// A record with its first revision.
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
        content: impl Into<String>,
        evidence: Vec<Evidence>,
        created_at: i64,
        author: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let summary = summary.into();
        let content = content.into();
        let author = author.into();
        let etag = revision_etag(&id, 1, &summary, &content);
        Self {
            id,
            revisions: vec![MemoryRevision {
                revision: 1,
                etag,
                summary,
                content,
                key_facts: Vec::new(),
                evidence,
                created_at,
                author,
                head: true,
            }],
        }
    }

    /// The current head revision.
    pub fn head(&self) -> &MemoryRevision {
        // `revisions` is never empty: the constructor seeds revision 1 and no
        // operation removes revisions.
        self.revisions
            .iter()
            .find(|revision| revision.head)
            .unwrap_or_else(|| &self.revisions[self.revisions.len() - 1])
    }

    /// A specific historical revision.
    pub fn revision(&self, number: u64) -> Option<&MemoryRevision> {
        self.revisions
            .iter()
            .find(|revision| revision.revision == number)
    }

    /// Every revision, oldest first.
    pub fn history(&self) -> &[MemoryRevision] {
        &self.revisions
    }

    /// Append a revision derived from the given patch.
    ///
    /// The previous head is kept unchanged, which is what makes the update
    /// auditable and rollback-able (§23.4).
    pub fn append(
        &mut self,
        patch: &MemoryPatch,
        evidence: Vec<Evidence>,
        created_at: i64,
        author: &str,
    ) -> Result<&MemoryRevision> {
        if patch.is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "patch".to_string(),
                reason: "must change at least one field".to_string(),
            });
        }
        patch.validate()?;
        let previous = self.head().clone();
        let summary = patch
            .summary
            .clone()
            .unwrap_or_else(|| previous.summary.clone());
        let content = patch
            .content
            .clone()
            .unwrap_or_else(|| previous.content.clone());
        let key_facts = patch
            .key_facts
            .clone()
            .unwrap_or_else(|| previous.key_facts.clone());
        let mut merged_evidence = previous.evidence.clone();
        for item in evidence {
            item.validate()?;
            if !merged_evidence
                .iter()
                .any(|existing| existing.evidence_id == item.evidence_id)
            {
                merged_evidence.push(item);
            }
        }
        let next = previous.revision + 1;
        let etag = revision_etag(&self.id, next, &summary, &content);
        for revision in self.revisions.iter_mut() {
            revision.head = false;
        }
        self.revisions.push(MemoryRevision {
            revision: next,
            etag,
            summary,
            content,
            key_facts,
            evidence: merged_evidence,
            created_at,
            author: author.to_string(),
            head: true,
        });
        Ok(self.head())
    }

    /// Move the head pointer back to an existing revision (§23.4 rollback).
    pub fn rollback_to(&mut self, number: u64) -> Result<&MemoryRevision> {
        if self.revision(number).is_none() {
            return Err(MemoryError::NotFound {
                resource: format!("revision {number}"),
            });
        }
        for revision in self.revisions.iter_mut() {
            revision.head = revision.revision == number;
        }
        Ok(self.head())
    }
}

fn revision_etag(id: &str, revision: u64, summary: &str, content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update([0u8]);
    hasher.update(revision.to_le_bytes());
    hasher.update([0u8]);
    hasher.update(summary.as_bytes());
    hasher.update([0u8]);
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    format!("\"{out}\"")
}

/// An open reconsolidation window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Window id.
    pub window_id: String,
    /// Memory the window applies to.
    pub memory_id: String,
    /// Instant the window opened.
    pub opened_at: i64,
    /// Instant the window stops accepting writes.
    pub labile_until: i64,
    /// Head revision the window was opened against.
    pub basis_revision: u64,
    /// Window state.
    pub state: WindowState,
    /// Task the retrievals happened in.
    pub retrieved_in_task: String,
    /// Principal allowed to write inside the window.
    pub authorized_editor: String,
}

impl Window {
    /// Open a window against the current head.
    ///
    /// The length defaults to 4h and is clamped to `1..=12h` (§23.4).
    pub fn open(
        window_id: impl Into<String>,
        memory_id: impl Into<String>,
        head: &MemoryRevision,
        opened_at: i64,
        requested_hours: Option<f64>,
        retrieved_in_task: impl Into<String>,
        authorized_editor: impl Into<String>,
    ) -> Result<Self> {
        let hours = requested_hours.unwrap_or(DEFAULT_WINDOW_HOURS);
        if !hours.is_finite() {
            return Err(MemoryError::InvalidArgument {
                field: "window_hours".to_string(),
                reason: "must be finite".to_string(),
            });
        }
        let clamped = hours.clamp(MIN_WINDOW_HOURS, MAX_WINDOW_HOURS);
        Ok(Self {
            window_id: window_id.into(),
            memory_id: memory_id.into(),
            opened_at,
            labile_until: crate::time::add_hours(opened_at, clamped),
            basis_revision: head.revision,
            state: WindowState::Open,
            retrieved_in_task: retrieved_in_task.into(),
            authorized_editor: authorized_editor.into(),
        })
    }

    /// The wire form of this window.
    pub fn view(&self, kind: crate::domain::MemoryKind) -> ReconsolidationWindow {
        ReconsolidationWindow {
            fact: crate::domain::Ref::new(
                self.memory_id.clone(),
                kind,
                self.basis_revision.to_string(),
            ),
            window_id: self.window_id.clone(),
            expires_at: crate::time::format_rfc3339(self.labile_until),
            state: self.state,
        }
    }

    /// Whether the window still accepts a write at `now`.
    pub fn is_open_at(&self, now: i64) -> bool {
        self.state == WindowState::Open && now <= self.labile_until
    }

    /// Close the window by time if it ran out.
    pub fn expire_if_due(&mut self, now: i64) -> WindowState {
        if self.state == WindowState::Open && now > self.labile_until {
            self.state = WindowState::Expired;
        }
        self.state
    }

    /// Validate a commit against the window, the CAS tag and the evidence.
    pub fn check_commit(
        &mut self,
        now: i64,
        expected_etag: &str,
        head: &MemoryRevision,
        evidence: &[Evidence],
    ) -> Result<()> {
        if !evidence.is_empty() {
            for item in evidence {
                item.validate()?;
            }
        }
        if !self.is_open_at(now) {
            self.expire_if_due(now);
            return Err(MemoryError::PolicyDenied {
                reason: format!(
                    "window {} is {} at {}; a new revision proposal is required (§23.4)",
                    self.window_id,
                    self.state.as_str(),
                    crate::time::format_rfc3339(now)
                ),
            });
        }
        if expected_etag != head.etag {
            return Err(MemoryError::EtagMismatch {
                expected: expected_etag.to_string(),
                actual: head.etag.clone(),
            });
        }
        if head.revision != self.basis_revision {
            return Err(MemoryError::EtagMismatch {
                expected: format!("revision {}", self.basis_revision),
                actual: format!("revision {}", head.revision),
            });
        }
        if evidence.is_empty() {
            return Err(MemoryError::EvidenceRequired {
                operation: "memory.commit_reconsolidation".to_string(),
            });
        }
        Ok(())
    }

    /// Mark the window committed.
    pub fn commit(&mut self) {
        self.state = WindowState::Committed;
    }

    /// Close the window without a patch.
    pub fn cancel(&mut self) -> Result<()> {
        if self.state != WindowState::Open {
            return Err(MemoryError::StateConflict {
                resource: self.window_id.clone(),
                state: self.state.as_str().to_string(),
                operation: "cancel".to_string(),
            });
        }
        self.state = WindowState::Cancelled;
        Ok(())
    }
}

/// Temporary strength after a reconsolidation (§23.4).
///
/// It only affects retrieval priority; verification state, source revision and
/// permissions are untouched.
pub fn decayed_strength(strength: f64) -> f64 {
    (strength * STRENGTH_DECAY).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MemoryKind;

    fn evidence(id: &str) -> Evidence {
        Evidence {
            evidence_id: id.to_string(),
            source_ref: "artifact:sha256:aa".to_string(),
            confidence: 0.9,
            source_group: "doc-a".to_string(),
        }
    }

    fn record() -> MemoryRecord {
        MemoryRecord::new(
            "mem:1",
            "v1 summary",
            "v1 body",
            vec![evidence("e1")],
            0,
            "agent",
        )
    }

    #[test]
    fn revisions_are_immutable_and_the_head_moves_forward() {
        let mut record = record();
        let first_etag = record.head().etag.clone();
        let patch = MemoryPatch {
            summary: Some("v2 summary".into()),
            ..MemoryPatch::default()
        };
        let appended = record
            .append(&patch, vec![evidence("e2")], 10, "editor")
            .unwrap()
            .clone();
        assert_eq!(appended.revision, 2);
        assert!(appended.head);
        assert_ne!(appended.etag, first_etag);
        assert_eq!(record.history().len(), 2);
        assert_eq!(record.revision(1).unwrap().summary, "v1 summary");
        assert_eq!(record.revision(1).unwrap().etag, first_etag);
        assert_eq!(appended.evidence.len(), 2);
        assert_eq!(appended.content, "v1 body", "untouched fields carry over");
    }

    #[test]
    fn duplicate_evidence_is_not_appended_twice() {
        let mut record = record();
        let patch = MemoryPatch {
            content: Some("v2 body".into()),
            ..MemoryPatch::default()
        };
        let appended = record
            .append(&patch, vec![evidence("e1")], 5, "editor")
            .unwrap()
            .clone();
        assert_eq!(appended.evidence.len(), 1);
    }

    #[test]
    fn empty_patches_and_bad_evidence_are_rejected() {
        let mut record = record();
        assert_eq!(
            record
                .append(&MemoryPatch::default(), vec![], 1, "editor")
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
        let mut bad = evidence("e2");
        bad.confidence = 2.0;
        assert!(record
            .append(
                &MemoryPatch {
                    summary: Some("s".into()),
                    ..MemoryPatch::default()
                },
                vec![bad],
                1,
                "editor"
            )
            .is_err());
    }

    #[test]
    fn rollback_moves_the_head_without_losing_history() {
        let mut record = record();
        record
            .append(
                &MemoryPatch {
                    summary: Some("v2".into()),
                    ..MemoryPatch::default()
                },
                vec![],
                3,
                "editor",
            )
            .unwrap();
        let rolled = record.rollback_to(1).unwrap().clone();
        assert_eq!(rolled.revision, 1);
        assert!(rolled.head);
        assert_eq!(record.history().len(), 2);
        assert_eq!(record.rollback_to(9).unwrap_err().code(), "NOT_FOUND");
    }

    #[test]
    fn windows_default_to_four_hours_and_clamp_to_the_registered_range() {
        let record = record();
        let default = Window::open("w1", "mem:1", record.head(), 0, None, "t1", "editor").unwrap();
        assert_eq!(default.labile_until, 4 * 3_600);
        let too_long =
            Window::open("w2", "mem:1", record.head(), 0, Some(48.0), "t1", "editor").unwrap();
        assert_eq!(too_long.labile_until, MAX_WINDOW_HOURS as i64 * 3_600);
        let too_short =
            Window::open("w3", "mem:1", record.head(), 0, Some(0.1), "t1", "editor").unwrap();
        assert_eq!(too_short.labile_until, MIN_WINDOW_HOURS as i64 * 3_600);
        assert!(Window::open("w4", "mem:1", record.head(), 0, Some(f64::NAN), "t1", "e").is_err());
    }

    #[test]
    fn window_view_uses_the_wire_shape() {
        let record = record();
        let window = Window::open("w1", "mem:1", record.head(), 0, None, "t1", "editor").unwrap();
        let view = window.view(MemoryKind::Semantic);
        assert_eq!(view.window_id, "w1");
        assert_eq!(view.state, WindowState::Open);
        assert_eq!(view.expires_at, "1970-01-01T04:00:00Z");
        assert_eq!(view.fact.id, "mem:1");
    }

    #[test]
    fn commit_requires_an_open_window_matching_etag_and_evidence() {
        let record = record();
        let mut window =
            Window::open("w1", "mem:1", record.head(), 0, None, "t1", "editor").unwrap();
        let head = record.head().clone();

        assert_eq!(
            window
                .check_commit(10, &head.etag, &head, &[])
                .unwrap_err()
                .code(),
            "EVIDENCE_REQUIRED"
        );
        assert_eq!(
            window
                .check_commit(10, "\"stale\"", &head, &[evidence("e2")])
                .unwrap_err()
                .code(),
            "ETAG_MISMATCH"
        );
        window
            .check_commit(10, &head.etag, &head, &[evidence("e2")])
            .unwrap();
        window.commit();
        assert_eq!(window.state, WindowState::Committed);
        assert_eq!(
            window
                .check_commit(10, &head.etag, &head, &[evidence("e3")])
                .unwrap_err()
                .code(),
            "POLICY_DENIED"
        );
    }

    #[test]
    fn a_moved_head_forces_a_new_proposal() {
        let mut record = record();
        let mut window =
            Window::open("w1", "mem:1", record.head(), 0, None, "t1", "editor").unwrap();
        let basis_etag = record.head().etag.clone();
        record
            .append(
                &MemoryPatch {
                    content: Some("someone else's edit".into()),
                    ..MemoryPatch::default()
                },
                vec![],
                5,
                "other",
            )
            .unwrap();
        let head = record.head().clone();
        let error = window
            .check_commit(10, &basis_etag, &head, &[evidence("e2")])
            .unwrap_err();
        assert_eq!(error.code(), "ETAG_MISMATCH");
    }

    #[test]
    fn an_expired_window_reports_policy_denied_and_stays_expired() {
        let record = record();
        let mut window =
            Window::open("w1", "mem:1", record.head(), 0, Some(1.0), "t1", "editor").unwrap();
        let head = record.head().clone();
        let error = window
            .check_commit(2 * 3_600, &head.etag, &head, &[evidence("e2")])
            .unwrap_err();
        assert_eq!(error.code(), "POLICY_DENIED");
        assert_eq!(window.state, WindowState::Expired);
        assert_eq!(window.expire_if_due(10 * 3_600), WindowState::Expired);
    }

    #[test]
    fn cancel_only_works_once_and_only_while_open() {
        let record = record();
        let mut window =
            Window::open("w1", "mem:1", record.head(), 0, None, "t1", "editor").unwrap();
        window.cancel().unwrap();
        assert_eq!(window.state, WindowState::Cancelled);
        assert_eq!(window.cancel().unwrap_err().code(), "LINK_STATE_CONFLICT");
    }

    #[test]
    fn strength_decay_only_scales_the_ranking_signal() {
        assert!((decayed_strength(1.0) - 0.8).abs() < 1e-12);
        assert_eq!(decayed_strength(2.0), 1.0);
        assert_eq!(decayed_strength(-1.0), 0.0);
    }
}
