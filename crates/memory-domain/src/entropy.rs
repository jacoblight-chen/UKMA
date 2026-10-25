//! Entropy-triggered summarisation (§23.3, LC-002).
//!
//! Compression only happens for mid-retention, redundant content, and it is
//! rolled back when the summary grows or loses a key fact or a reference.

use crate::domain::{MemoryError, Result};
use crate::fsrs::{is_compression_candidate, EntropySignal};

/// What the scan decided for one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionDecision {
    /// Summarise the record.
    Summarize,
    /// Empty content: nothing to measure (§23.3 `SKIPPED_EMPTY`).
    SkipEmpty,
    /// Not a candidate.
    NotCandidate {
        /// Why.
        reason: &'static str,
    },
}

impl CompressionDecision {
    /// Stable token.
    pub fn as_str(&self) -> &'static str {
        match self {
            CompressionDecision::Summarize => "summarize",
            CompressionDecision::SkipEmpty => "skipped_empty",
            CompressionDecision::NotCandidate { .. } => "not_candidate",
        }
    }
}

/// Decide whether a record is a summarisation candidate.
pub fn decide(
    retrievability: f64,
    signal: EntropySignal,
    original_tokens: u32,
) -> Result<CompressionDecision> {
    if !retrievability.is_finite() || !(0.0..=1.0).contains(&retrievability) {
        return Err(MemoryError::InvalidArgument {
            field: "retrievability".to_string(),
            reason: format!("must be in [0,1], got {retrievability}"),
        });
    }
    if original_tokens == 0 {
        return Ok(CompressionDecision::SkipEmpty);
    }
    if matches!(signal, EntropySignal::SkippedEmpty) {
        return Ok(CompressionDecision::SkipEmpty);
    }
    if !is_compression_candidate(retrievability, signal) {
        return Ok(CompressionDecision::NotCandidate {
            reason: "retrievability or redundancy is outside the compression band",
        });
    }
    Ok(CompressionDecision::Summarize)
}

/// Verdict of a compression attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressionVerdict {
    /// The summary is smaller and kept everything that matters.
    Accepted {
        /// Tokens saved.
        saved_tokens: u32,
    },
    /// The attempt must be rolled back, with the reason recorded.
    RolledBack {
        /// Why.
        reason: &'static str,
    },
}

impl CompressionVerdict {
    /// Whether the compression was accepted.
    pub fn is_accepted(&self) -> bool {
        matches!(self, CompressionVerdict::Accepted { .. })
    }
}

/// Validate a compression result (§23.3: roll back on growth or content loss).
pub fn verify(
    original_tokens: u32,
    compressed_tokens: u32,
    key_content_retained: bool,
    references_retained: bool,
) -> CompressionVerdict {
    if !key_content_retained {
        return CompressionVerdict::RolledBack {
            reason: "key content was lost",
        };
    }
    if !references_retained {
        return CompressionVerdict::RolledBack {
            reason: "source references were lost",
        };
    }
    if compressed_tokens >= original_tokens {
        return CompressionVerdict::RolledBack {
            reason: "compression did not reduce the token count",
        };
    }
    CompressionVerdict::Accepted {
        saved_tokens: original_tokens - compressed_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsrs::entropy_signal;

    #[test]
    fn empty_content_is_skipped_not_compressed() {
        assert_eq!(
            decide(0.5, EntropySignal::SkippedEmpty, 0).unwrap(),
            CompressionDecision::SkipEmpty
        );
        assert_eq!(
            decide(0.5, entropy_signal(b""), 10).unwrap(),
            CompressionDecision::SkipEmpty
        );
        assert_eq!(CompressionDecision::SkipEmpty.as_str(), "skipped_empty");
    }

    #[test]
    fn only_mid_retention_redundant_content_is_summarised() {
        let redundant = entropy_signal(&[b'x'; 4_096]);
        assert_eq!(
            decide(0.5, redundant, 500).unwrap(),
            CompressionDecision::Summarize
        );
        assert_eq!(
            decide(0.95, redundant, 500).unwrap().as_str(),
            "not_candidate"
        );
        assert_eq!(
            decide(0.1, redundant, 500).unwrap().as_str(),
            "not_candidate"
        );
        assert!(decide(1.5, redundant, 500).is_err());
        assert!(decide(f64::NAN, redundant, 500).is_err());
    }

    #[test]
    fn a_summary_that_grows_or_loses_content_is_rolled_back() {
        assert_eq!(
            verify(500, 100, true, true),
            CompressionVerdict::Accepted { saved_tokens: 400 }
        );
        assert!(verify(500, 100, true, true).is_accepted());
        assert_eq!(
            verify(500, 600, true, true),
            CompressionVerdict::RolledBack {
                reason: "compression did not reduce the token count"
            }
        );
        assert_eq!(
            verify(500, 100, false, true),
            CompressionVerdict::RolledBack {
                reason: "key content was lost"
            }
        );
        assert_eq!(
            verify(500, 100, true, false),
            CompressionVerdict::RolledBack {
                reason: "source references were lost"
            }
        );
        assert!(!verify(500, 500, true, true).is_accepted());
    }
}
