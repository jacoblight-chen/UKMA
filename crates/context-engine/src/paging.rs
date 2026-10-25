//! Paging out/in a block (§22.4): integrity-checked, ACL-checked on the way
//! back in, and never silently lossy.

use sha2::{Digest, Sha256};

/// Why a paged block could not be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    /// The caller is not allowed to read this block.
    CrossScopeDenied {
        /// Block id.
        block_id: String,
    },
    /// Integrity check failed (corruption or tampering).
    IntegrityFailed {
        /// Expected content hash.
        expected: String,
    },
    /// The source revision is no longer visible.
    SourceUnavailable {
        /// Missing revision.
        revision: String,
    },
    /// The page record expired.
    Expired {
        /// Page id.
        page_id: String,
    },
    /// The block is protected and may not be paged out (§22.2 R1–R8).
    PolicyDenied {
        /// Block id.
        block_id: String,
    },
}

impl PageError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            PageError::CrossScopeDenied { .. } => "CROSS_SCOPE_DENIED",
            PageError::IntegrityFailed { .. } => "INTERNAL",
            PageError::SourceUnavailable { .. } => "SOURCE_UNAVAILABLE",
            PageError::Expired { .. } => "NOT_FOUND",
            PageError::PolicyDenied { .. } => "POLICY_DENIED",
        }
    }
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageError::CrossScopeDenied { block_id } => write!(f, "denied: {block_id}"),
            PageError::IntegrityFailed { expected } => {
                write!(f, "integrity failed, expected {expected}")
            }
            PageError::SourceUnavailable { revision } => {
                write!(f, "revision {revision} unavailable")
            }
            PageError::Expired { page_id } => write!(f, "page {page_id} expired"),
            PageError::PolicyDenied { block_id } => {
                write!(f, "protected block cannot be paged: {block_id}")
            }
        }
    }
}

impl std::error::Error for PageError {}

/// Durable record of a paged-out block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRecord {
    /// Page identity.
    pub page_id: String,
    /// Block that was paged out.
    pub block_id: String,
    /// Memory revision the page belongs to.
    pub memory_revision: u64,
    /// `sha256:<hex>` of the payload.
    pub checksum: String,
    /// Index generation at page-out time.
    pub index_generation: String,
    /// Expiry instant (RFC3339) if the policy sets one.
    pub expires_at: Option<String>,
}

/// A successfully restored block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedPage {
    /// The page that was opened.
    pub record: PageRecord,
    /// Restored payload.
    pub payload: Vec<u8>,
}

/// Content hash of a payload.
pub fn checksum(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Page a block out.
///
/// Refuses protected blocks instead of quietly dropping them (§22.4, `CT-004`).
pub fn page_out(
    page_id: &str,
    block_id: &str,
    memory_revision: u64,
    index_generation: &str,
    payload: &[u8],
    protected: bool,
    expires_at: Option<String>,
) -> Result<PageRecord, PageError> {
    if protected {
        return Err(PageError::PolicyDenied {
            block_id: block_id.to_string(),
        });
    }
    Ok(PageRecord {
        page_id: page_id.to_string(),
        block_id: block_id.to_string(),
        memory_revision,
        checksum: checksum(payload),
        index_generation: index_generation.to_string(),
        expires_at,
    })
}

/// Page a block back in.
///
/// Every guard in §22.4 is applied: expiry, ACL, source visibility, and the
/// integrity hash. Any failure is returned, never papered over.
pub fn page_in(
    record: &PageRecord,
    payload: &[u8],
    authorized: bool,
    source_revision_visible: bool,
    expired: bool,
) -> Result<OpenedPage, PageError> {
    if expired {
        return Err(PageError::Expired {
            page_id: record.page_id.clone(),
        });
    }
    if !authorized {
        return Err(PageError::CrossScopeDenied {
            block_id: record.block_id.clone(),
        });
    }
    if !source_revision_visible {
        return Err(PageError::SourceUnavailable {
            revision: record.memory_revision.to_string(),
        });
    }
    let actual = checksum(payload);
    if actual != record.checksum {
        return Err(PageError::IntegrityFailed {
            expected: record.checksum.clone(),
        });
    }
    Ok(OpenedPage {
        record: record.clone(),
        payload: payload.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_restores_identical_payload() {
        let payload = b"task state snapshot";
        let record = page_out("p1", "b1", 3, "gen-1", payload, false, None).unwrap();
        assert!(record.checksum.starts_with("sha256:"));
        let opened = page_in(&record, payload, true, true, false).unwrap();
        assert_eq!(opened.payload, payload);
        assert_eq!(opened.record.memory_revision, 3);
    }

    #[test]
    fn protected_blocks_cannot_be_paged_out() {
        let error = page_out("p1", "b1", 1, "gen-1", b"x", true, None).unwrap_err();
        assert_eq!(error.code(), "POLICY_DENIED");
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let record = page_out("p1", "b1", 1, "gen-1", b"original", false, None).unwrap();
        let error = page_in(&record, b"tampered", true, true, false).unwrap_err();
        assert_eq!(error.code(), "INTERNAL");
        assert!(error.to_string().contains("integrity failed"));
    }

    #[test]
    fn every_guard_is_enforced() {
        let record = page_out("p1", "b1", 1, "gen-1", b"data", false, None).unwrap();
        assert_eq!(
            page_in(&record, b"data", false, true, false)
                .unwrap_err()
                .code(),
            "CROSS_SCOPE_DENIED"
        );
        assert_eq!(
            page_in(&record, b"data", true, false, false)
                .unwrap_err()
                .code(),
            "SOURCE_UNAVAILABLE"
        );
        assert_eq!(
            page_in(&record, b"data", true, true, true)
                .unwrap_err()
                .code(),
            "NOT_FOUND"
        );
    }
}
