//! Deletion: tombstones, per-projection revocation and the `DeletionReceipt`
//! (§23.1, §34.1, MAPI-23, PROP-05).
//!
//! A deletion never touches a source artifact, code/document knowledge or an
//! accepted `TraceLink`. Those authorities may only become `stale`, which the
//! returned outcome reports separately so a caller cannot claim a full purge.

use crate::domain::{
    CacheInvalidation, DeletionReceipt, MemoryError, ProjectionState, ProjectionStatus, Ref, Result,
};
use crate::time::format_rfc3339;

/// Projections a memory record is projected into and that must be revoked.
pub const PROJECTIONS: [&str; 4] = [
    "lexical_index",
    "vector_index",
    "graph_projection",
    "translation_cache",
];

/// Authorities a deletion must never revoke (§34.2 `forget` note).
pub const NEVER_REVOKED: [&str; 3] = [
    "source_artifact",
    "code_document_knowledge",
    "accepted_trace_link",
];

/// Guards that decide whether a deletion is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeletionGuards {
    /// A legal hold protects the record (§22.2 R8).
    pub legal_hold: bool,
    /// Retention has not expired, and retention blocks this deletion.
    pub retention_blocks: bool,
    /// A job still depends on the record.
    pub active_job: bool,
    /// An accepted trace link points at the record.
    pub accepted_trace_links: Vec<String>,
    /// Regulation forbids the deletion.
    pub regulation_blocks: bool,
}

impl DeletionGuards {
    /// Reasons the deletion is refused, in a stable order.
    pub fn blocking_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.legal_hold {
            reasons.push("legal_hold".to_string());
        }
        if self.retention_blocks {
            reasons.push("retention".to_string());
        }
        if self.active_job {
            reasons.push("active_job".to_string());
        }
        if self.regulation_blocks {
            reasons.push("regulation".to_string());
        }
        reasons
    }

    /// Accepted trace links that survive the deletion and become stale.
    pub fn stale_trace_links(&self) -> &[String] {
        &self.accepted_trace_links
    }
}

/// Check that a deletion may proceed.
pub fn check_deletion_allowed(guards: &DeletionGuards) -> Result<()> {
    let reasons = guards.blocking_reasons();
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(MemoryError::DeletionBlocked {
            reason: reasons.join(","),
        })
    }
}

/// The tombstone revision tag for a record.
pub fn tombstone_revision(memory_id: &str, revision: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(memory_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(revision.to_le_bytes());
    hasher.update(b"tombstone");
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    format!("tombstone:{out}")
}

/// Outcome of a deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionOutcome {
    /// Receipt to return to the caller.
    pub receipt: DeletionReceipt,
    /// Accepted trace links that were marked stale instead of deleted.
    pub stale_trace_links: Vec<String>,
}

/// Build the receipt of a completed deletion.
///
/// `tombstone` is the revision tag produced by [`tombstone_revision`];
/// `backup_expiry_at` is the instant the last backup copy expires, where `None`
/// means there is no backup copy.
pub fn build_receipt(
    memory: Ref,
    tombstone: String,
    guards: &DeletionGuards,
    backup_expiry_at: Option<i64>,
) -> Result<DeletionOutcome> {
    check_deletion_allowed(guards)?;
    let projections = PROJECTIONS
        .iter()
        .map(|name| {
            debug_assert!(
                !NEVER_REVOKED.contains(name),
                "{name} must never be revoked by a deletion"
            );
            ProjectionStatus {
                name: (*name).to_string(),
                state: ProjectionState::Cleared,
            }
        })
        .collect();
    Ok(DeletionOutcome {
        receipt: DeletionReceipt {
            memory,
            tombstone_revision: tombstone,
            projections,
            cache_invalidation: CacheInvalidation::Complete,
            backup_expiry_at: backup_expiry_at.map(format_rfc3339),
            legal_hold: guards.legal_hold,
        },
        stale_trace_links: guards.stale_trace_links().to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MemoryKind;

    fn reference() -> Ref {
        Ref::new("mem:a:b:1", MemoryKind::Episodic, "3")
    }

    #[test]
    fn a_clean_deletion_revokes_every_projection_and_the_cache() {
        let guards = DeletionGuards::default();
        let outcome = build_receipt(
            reference(),
            tombstone_revision("mem:a:b:1", 4),
            &guards,
            Some(1_700_000_000),
        )
        .unwrap();
        let receipt = outcome.receipt;
        assert_eq!(receipt.projections.len(), PROJECTIONS.len());
        for projection in &receipt.projections {
            assert_eq!(projection.state, ProjectionState::Cleared);
            assert!(
                !NEVER_REVOKED.contains(&projection.name.as_str()),
                "{} must not be revoked",
                projection.name
            );
        }
        assert_eq!(receipt.cache_invalidation, CacheInvalidation::Complete);
        assert_eq!(
            receipt.backup_expiry_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert!(!receipt.legal_hold);
        assert!(outcome.stale_trace_links.is_empty());
        assert!(receipt.tombstone_revision.starts_with("tombstone:"));
    }

    #[test]
    fn legal_hold_retention_and_active_jobs_block_the_deletion() {
        let guards = DeletionGuards {
            legal_hold: true,
            retention_blocks: true,
            active_job: true,
            accepted_trace_links: vec!["trace:1".into()],
            regulation_blocks: true,
        };
        assert_eq!(
            guards.blocking_reasons(),
            vec!["legal_hold", "retention", "active_job", "regulation"]
        );
        let error = build_receipt(
            reference(),
            tombstone_revision("mem:a:b:1", 1),
            &guards,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code(), "DELETION_BLOCKED");
        assert!(error.to_string().contains("legal_hold"));
    }

    #[test]
    fn accepted_trace_links_survive_and_are_reported_as_stale() {
        let guards = DeletionGuards {
            accepted_trace_links: vec!["trace:9".into()],
            ..DeletionGuards::default()
        };
        let outcome = build_receipt(
            reference(),
            tombstone_revision("mem:a:b:1", 2),
            &guards,
            None,
        )
        .unwrap();
        assert_eq!(outcome.stale_trace_links, vec!["trace:9".to_string()]);
        assert!(outcome
            .receipt
            .projections
            .iter()
            .all(|projection| !NEVER_REVOKED.contains(&projection.name.as_str())));
    }

    #[test]
    fn tombstone_revisions_are_stable_and_depend_on_the_revision() {
        let first = tombstone_revision("mem:1", 1);
        assert_eq!(first, tombstone_revision("mem:1", 1));
        assert_ne!(first, tombstone_revision("mem:1", 2));
        assert_ne!(first, tombstone_revision("mem:2", 1));
        assert!(first.starts_with("tombstone:"));
    }
}
