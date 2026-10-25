//! Eviction policies (§22.3): a deterministic policy is REQUIRED; the RL policy
//! is REQUIRED-CONFIGURABLE and ships in shadow mode, never bypassing the safety
//! filter.

use std::collections::BTreeSet;

/// One eviction candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Block identity.
    pub id: String,
    /// Tokens the block occupies.
    pub tokens: u32,
    /// Relevance to the current task, in `[0,1]`.
    pub relevance: f64,
    /// Age in days.
    pub age_days: f64,
    /// Importance in `[0,1]`.
    pub importance: f64,
    /// Referenced earlier in the session.
    pub referenced: bool,
    /// Explicitly pinned by the user (never evicted, §22.2 R6).
    pub pinned: bool,
    /// Carries a preserve rule (never evicted, §22.2 R1–R8).
    pub protected: bool,
    /// Belongs to the in-flight task (never evicted, §22.2 R4).
    pub task_active: bool,
}

impl Candidate {
    /// Whether this candidate is untouchable by any policy.
    pub fn is_hard_protected(&self) -> bool {
        self.pinned || self.protected || self.task_active
    }
}

/// Outcome of an eviction ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedEvictions {
    /// Blocks selected for eviction, in eviction order.
    pub selected: Vec<String>,
    /// Blocks that may never be evicted but were offered.
    pub protected: Vec<String>,
    /// `true` when the policy could not free enough budget without touching
    /// protected content; callers must then fail safely instead of truncating.
    pub overflow_safe_failure: bool,
    /// Tokens freed by `selected`.
    pub freed_tokens: u32,
    /// Audit detail.
    pub reason: String,
}

/// Eviction strategy interface (§22.3).
pub trait EvictionPolicy {
    /// Rank candidates so that evicting them frees `required_tokens`.
    fn rank(&self, candidates: &[Candidate], required_tokens: u32) -> RankedEvictions;

    /// Stable policy identifier recorded in audit records.
    fn policy_version(&self) -> &'static str;
}

/// Deterministic policy: hard filter first, then a total order that never
/// depends on insertion order.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeterministicPolicy;

fn total_order_key(candidate: &Candidate) -> (bool, u64, u64, u64, u32, String) {
    // Lower tuple sorts first => evict first. Evict the least valuable content:
    // unreferenced, old, low relevance, low importance, large.
    (
        candidate.referenced,
        (candidate.relevance * 1000.0) as u64,
        (candidate.importance * 1000.0) as u64,
        (1_000_000.0 - candidate.age_days * 1000.0).max(0.0) as u64,
        u32::MAX - candidate.tokens,
        candidate.id.clone(),
    )
}

impl EvictionPolicy for DeterministicPolicy {
    fn rank(&self, candidates: &[Candidate], required_tokens: u32) -> RankedEvictions {
        let mut protected: Vec<String> = Vec::new();
        let mut evictable: Vec<&Candidate> = Vec::new();
        for candidate in candidates {
            if candidate.is_hard_protected() {
                protected.push(candidate.id.clone());
            } else {
                evictable.push(candidate);
            }
        }
        evictable.sort_by_key(|candidate| total_order_key(candidate));

        let mut selected = Vec::new();
        let mut freed = 0u32;
        for candidate in evictable {
            if freed >= required_tokens {
                break;
            }
            freed += candidate.tokens;
            selected.push(candidate.id.clone());
        }
        let overflow_safe_failure = freed < required_tokens;
        RankedEvictions {
            selected,
            protected,
            overflow_safe_failure,
            freed_tokens: freed,
            reason: if overflow_safe_failure {
                "insufficient evictable tokens; refusing to truncate protected content (§22.3)"
                    .to_string()
            } else {
                "deterministic ordering by referenced/relevance/importance/age/size".to_string()
            },
        }
    }

    fn policy_version(&self) -> &'static str {
        "deterministic-v1"
    }
}

/// RL eviction policy in **shadow mode** (§22.3 REQUIRED-CONFIGURABLE).
///
/// It never removes anything itself: it records what it would have proposed and
/// always returns the deterministic selection. Promotion to active requires the
/// A/B safety gate, which is not implemented here.
#[derive(Debug, Clone)]
pub struct ShadowRlPolicy {
    deterministic: DeterministicPolicy,
    shadow_enabled: bool,
}

impl Default for ShadowRlPolicy {
    fn default() -> Self {
        Self {
            deterministic: DeterministicPolicy,
            shadow_enabled: true,
        }
    }
}

/// One shadow decision record.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowRecord {
    /// Hash of the candidate set the proposal was made on.
    pub candidate_set_hash: String,
    /// Proposed block ids.
    pub proposal: Vec<String>,
    /// Always `true` for this implementation.
    pub was_shadow: bool,
    /// Human-readable decision reason.
    pub decision_reason: String,
}

impl ShadowRlPolicy {
    /// New policy; `shadow_enabled = false` disables the model entirely.
    pub fn new(shadow_enabled: bool) -> Self {
        Self {
            deterministic: DeterministicPolicy,
            shadow_enabled,
        }
    }

    /// Produce the deterministic ranking plus (optionally) a shadow proposal.
    pub fn rank_with_shadow(
        &self,
        candidates: &[Candidate],
        required_tokens: u32,
        model_proposal: Option<Vec<String>>,
    ) -> (RankedEvictions, Option<ShadowRecord>) {
        let ranked = self.deterministic.rank(candidates, required_tokens);
        if !self.shadow_enabled {
            return (ranked, None);
        }
        let mut ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        let candidate_set_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            for id in &ids {
                hasher.update(id.as_bytes());
                hasher.update([0u8]);
            }
            let digest = hasher.finalize();
            let mut out = String::with_capacity(16);
            for byte in &digest[..8] {
                out.push_str(&format!("{byte:02x}"));
            }
            out
        };
        // Safety filter applies to the proposal too: protected ids are dropped.
        let protected: BTreeSet<&String> = candidates
            .iter()
            .filter(|candidate| candidate.is_hard_protected())
            .map(|candidate| &candidate.id)
            .collect();
        let raw = model_proposal.unwrap_or_else(|| ranked.selected.clone());
        let filtered: Vec<String> = raw
            .into_iter()
            .filter(|id| !protected.contains(id))
            .collect();
        let record = ShadowRecord {
            candidate_set_hash,
            proposal: filtered,
            was_shadow: true,
            decision_reason: "shadow mode: deterministic policy is authoritative (§22.3)"
                .to_string(),
        };
        (ranked, Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, tokens: u32, relevance: f64) -> Candidate {
        Candidate {
            id: id.to_string(),
            tokens,
            relevance,
            age_days: 1.0,
            importance: 0.5,
            referenced: false,
            pinned: false,
            protected: false,
            task_active: false,
        }
    }

    #[test]
    fn protected_content_is_never_selected() {
        let mut pinned = candidate("pinned", 500, 0.0);
        pinned.pinned = true;
        let mut task = candidate("task", 500, 0.0);
        task.task_active = true;
        let mut protected = candidate("rule", 500, 0.0);
        protected.protected = true;
        let candidates = vec![pinned, task, protected, candidate("low", 100, 0.1)];

        let ranked = DeterministicPolicy.rank(&candidates, 100);
        assert_eq!(ranked.selected, vec!["low".to_string()]);
        assert_eq!(ranked.protected.len(), 3);
        assert!(!ranked.overflow_safe_failure);
    }

    #[test]
    fn insufficient_evictable_tokens_fails_safely() {
        let mut pinned = candidate("pinned", 900, 0.0);
        pinned.pinned = true;
        let candidates = vec![pinned, candidate("small", 10, 0.1)];
        let ranked = DeterministicPolicy.rank(&candidates, 500);
        assert!(ranked.overflow_safe_failure);
        assert!(ranked.reason.contains("refusing to truncate"));
    }

    #[test]
    fn ranking_is_deterministic_and_prefers_low_value_first() {
        let candidates = vec![
            candidate("high", 100, 0.9),
            candidate("mid", 100, 0.5),
            candidate("low", 100, 0.1),
        ];
        let first = DeterministicPolicy.rank(&candidates, 200);
        let second = DeterministicPolicy.rank(&candidates, 200);
        assert_eq!(first, second);
        assert_eq!(first.selected, vec!["low".to_string(), "mid".to_string()]);
    }

    #[test]
    fn shadow_policy_never_overrides_deterministic_selection() {
        let candidates = vec![candidate("a", 100, 0.9), candidate("b", 100, 0.1)];
        let policy = ShadowRlPolicy::default();
        // The model proposes evicting the *valuable* block.
        let (ranked, shadow) =
            policy.rank_with_shadow(&candidates, 100, Some(vec!["a".to_string()]));
        assert_eq!(ranked.selected, vec!["b".to_string()], "deterministic wins");
        let record = shadow.expect("shadow record");
        assert!(record.was_shadow);
        assert_eq!(
            record.proposal,
            vec!["a".to_string()],
            "proposal is recorded only"
        );
    }

    #[test]
    fn shadow_proposal_cannot_include_protected_blocks() {
        let mut pinned = candidate("pinned", 100, 0.0);
        pinned.pinned = true;
        let candidates = vec![pinned, candidate("b", 100, 0.1)];
        let policy = ShadowRlPolicy::default();
        let (_, shadow) = policy.rank_with_shadow(
            &candidates,
            100,
            Some(vec!["pinned".to_string(), "b".to_string()]),
        );
        let record = shadow.expect("shadow record");
        assert_eq!(record.proposal, vec!["b".to_string()]);
    }

    #[test]
    fn disabling_shadow_removes_model_influence_entirely() {
        let policy = ShadowRlPolicy::new(false);
        let candidates = vec![candidate("a", 10, 0.5)];
        let (ranked, shadow) = policy.rank_with_shadow(&candidates, 10, Some(vec![]));
        assert!(shadow.is_none());
        assert_eq!(ranked.selected, vec!["a".to_string()]);
    }
}
