//! Sub-agent fractal isolation (§26.4, §34.1 `AgentScopeGrant`).
//!
//! Isolation is keyed by `agent_instance_id + task_id + tenant_id`, never by a
//! depth number alone, and a parent can only attenuate rights when it delegates.

use crate::domain::{AgentScopeGrant, AgentStatus, MemoryError, Result};
use crate::time::parse_rfc3339;

/// Maximum delegation depth (§26.4, §29.2 `max_depth=3`).
pub const MAX_DEPTH: u32 = 3;
/// Maximum concurrent sub-agents (§26.4, §29.2 `max_concurrent=10`).
pub const MAX_CONCURRENT: usize = 10;
/// Default sub-agent timeout in seconds (§26.4 `subagent_timeout=300s`).
pub const SUBAGENT_TIMEOUT_SECS: i64 = 300;
/// Follow-up window after a child task completes (§23.2, §29.2 `quick_followup=1h`).
pub const QUICK_FOLLOWUP_HOURS: i64 = 1;
/// Archive horizon after a child task completes (§23.2, §29.2 `archive_after=24h`).
pub const ARCHIVE_AFTER_HOURS: i64 = 24;
/// Purge-eligible horizon after a child task completes (§29.2 `purge_eligible_after=7days`).
pub const PURGE_ELIGIBLE_AFTER_DAYS: i64 = 7;

/// What a parent currently holds, used to attenuate a delegation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentAuthority {
    /// Tenant of the parent (isolation unit component).
    pub tenant_id: String,
    /// Parent agent instance id.
    pub agent_instance_id: String,
    /// Depth the parent runs at.
    pub depth: u32,
    /// Scopes the parent holds.
    pub scopes: Vec<String>,
    /// Tools the parent may call.
    pub tools: Vec<String>,
    /// Context tokens the parent may hand out.
    pub max_context_tokens: u32,
    /// Parent grant expiry as an instant.
    pub expires_at: i64,
    /// Tokens still unallocated from the parent budget.
    pub remaining_tokens: u32,
}

impl ParentAuthority {
    /// Whether the parent holds a scope.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }

    /// Whether the parent may call a tool.
    pub fn has_tool(&self, tool: &str) -> bool {
        self.tools.iter().any(|held| held == tool)
    }
}

/// Isolation key of a sub-agent scope (§26.4).
pub fn scope_key(tenant_id: &str, agent_instance_id: &str, task_id: &str) -> String {
    format!("scope:{tenant_id}:{agent_instance_id}:{task_id}")
}

/// Validate a delegation grant against the parent's authority.
///
/// Every rule here is an attenuation rule: the child can only get a subset of
/// what the parent holds, a shallower-or-equal expiry, and a smaller budget.
pub fn validate_grant(parent: &ParentAuthority, grant: &AgentScopeGrant) -> Result<i64> {
    if grant.child_agent_id.trim().is_empty() {
        return Err(MemoryError::InvalidArgument {
            field: "grant.child_agent_id".to_string(),
            reason: "must be non-empty".to_string(),
        });
    }
    if grant.task_id.trim().is_empty() {
        return Err(MemoryError::InvalidArgument {
            field: "grant.task_id".to_string(),
            reason: "the isolation unit requires a task id (§26.4)".to_string(),
        });
    }
    if grant.child_agent_id == parent.agent_instance_id {
        return Err(MemoryError::InvalidArgument {
            field: "grant.child_agent_id".to_string(),
            reason: "must differ from the parent agent instance".to_string(),
        });
    }
    let expected_depth = parent.depth + 1;
    if grant.depth != expected_depth {
        return Err(MemoryError::PolicyDenied {
            reason: format!(
                "child depth must be {} (parent depth {} + 1), got {}",
                expected_depth, parent.depth, grant.depth
            ),
        });
    }
    if grant.depth > MAX_DEPTH {
        return Err(MemoryError::PolicyDenied {
            reason: format!(
                "depth {} exceeds max_depth {MAX_DEPTH} (§26.4)",
                grant.depth
            ),
        });
    }
    for scope in &grant.allowed_scopes {
        if !parent.has_scope(scope) {
            return Err(MemoryError::PolicyDenied {
                reason: format!(
                    "parent does not hold scope `{scope}`; delegation may only attenuate"
                ),
            });
        }
    }
    for tool in &grant.allowed_tools {
        if !parent.has_tool(tool) {
            return Err(MemoryError::PolicyDenied {
                reason: format!("parent cannot delegate tool `{tool}`"),
            });
        }
    }
    if grant.max_context_tokens > parent.max_context_tokens {
        return Err(MemoryError::ContextBudgetExceeded {
            requested: grant.max_context_tokens,
            remaining: parent.max_context_tokens,
        });
    }
    if grant.max_context_tokens > parent.remaining_tokens {
        return Err(MemoryError::ContextBudgetExceeded {
            requested: grant.max_context_tokens,
            remaining: parent.remaining_tokens,
        });
    }
    let expires_at = parse_rfc3339(&grant.expires_at)?;
    if expires_at > parent.expires_at {
        return Err(MemoryError::PolicyDenied {
            reason: "child grant must not outlive the parent grant".to_string(),
        });
    }
    Ok(expires_at)
}

/// Check that another sub-agent may start (§26.4 `max_concurrent=10`).
pub fn check_concurrency(running: usize) -> Result<()> {
    if running >= MAX_CONCURRENT {
        return Err(MemoryError::ResourceExhausted {
            detail: format!("{running} sub-agents already running, limit is {MAX_CONCURRENT}"),
        });
    }
    Ok(())
}

/// Deadline of a sub-agent started at `started_at`.
pub fn deadline(started_at: i64) -> i64 {
    started_at + SUBAGENT_TIMEOUT_SECS
}

/// Whether a sub-agent exceeded `subagent_timeout` at `now`.
pub fn is_timed_out(started_at: i64, now: i64) -> bool {
    now >= deadline(started_at)
}

/// Retention horizons of a completed child task (§23.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildRetention {
    /// End of the follow-up window.
    pub followup_until: i64,
    /// Instant the child becomes an archive candidate.
    pub archive_after: i64,
    /// Instant the child becomes purge-eligible.
    pub purge_eligible_after: i64,
}

impl ChildRetention {
    /// Horizons relative to a completion instant.
    pub fn after_completion(completed_at: i64) -> Self {
        Self {
            followup_until: completed_at + QUICK_FOLLOWUP_HOURS * 3_600,
            archive_after: completed_at + ARCHIVE_AFTER_HOURS * 3_600,
            purge_eligible_after: completed_at + PURGE_ELIGIBLE_AFTER_DAYS * 86_400,
        }
    }
}

/// Legal sub-agent lifecycle transitions (§26.4).
pub fn transition_status(from: AgentStatus, to: AgentStatus) -> Result<AgentStatus> {
    use AgentStatus::{
        Cancelled, CleanupEligible, Completed, Failed, Folding, Partial, Running, Sealed, Starting,
        TimedOut, Waiting,
    };
    let legal = match from {
        Starting => matches!(to, Running | Failed | Cancelled | TimedOut),
        Running => matches!(
            to,
            Waiting | Completed | Partial | Failed | Cancelled | TimedOut
        ),
        Waiting => matches!(
            to,
            Running | Completed | Partial | Failed | Cancelled | TimedOut
        ),
        Completed | Partial | Failed | Cancelled | TimedOut => matches!(to, Folding),
        Folding => matches!(to, Sealed),
        Sealed => matches!(to, CleanupEligible),
        CleanupEligible => false,
    };
    if !legal {
        return Err(MemoryError::StateConflict {
            resource: "agent_scope".to_string(),
            state: from.as_str().to_string(),
            operation: format!("become {}", to.as_str()),
        });
    }
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> ParentAuthority {
        ParentAuthority {
            tenant_id: "tenant-a".into(),
            agent_instance_id: "agent-main".into(),
            depth: 0,
            scopes: vec!["memory:read".into(), "memory:write".into()],
            tools: vec!["read_file".into()],
            max_context_tokens: 32_768,
            expires_at: 2_000_000_000,
            remaining_tokens: 16_384,
        }
    }

    fn grant() -> AgentScopeGrant {
        AgentScopeGrant {
            parent_agent_id: "agent-main".into(),
            child_agent_id: "agent-child".into(),
            session_id: "s1".into(),
            task_id: "t1".into(),
            depth: 1,
            allowed_scopes: vec!["memory:read".into()],
            allowed_tools: vec!["read_file".into()],
            expires_at: "2033-05-18T03:33:20Z".into(),
            max_context_tokens: 8_192,
        }
    }

    #[test]
    fn isolation_key_is_the_registered_unit() {
        assert_eq!(
            scope_key("tenant-a", "agent-child", "task-9"),
            "scope:tenant-a:agent-child:task-9"
        );
    }

    #[test]
    fn a_valid_grant_is_accepted_and_returns_its_expiry() {
        let expiry = validate_grant(&parent(), &grant()).unwrap();
        assert_eq!(expiry, 2_000_000_000);
    }

    #[test]
    fn grants_cannot_escalate_scopes_tools_budget_or_lifetime() {
        let mut wider_scope = grant();
        wider_scope.allowed_scopes.push("memory:forget".into());
        assert_eq!(
            validate_grant(&parent(), &wider_scope).unwrap_err().code(),
            "POLICY_DENIED"
        );

        let mut wider_tool = grant();
        wider_tool.allowed_tools.push("write_file".into());
        assert_eq!(
            validate_grant(&parent(), &wider_tool).unwrap_err().code(),
            "POLICY_DENIED"
        );

        let mut bigger_budget = grant();
        bigger_budget.max_context_tokens = 20_000;
        assert_eq!(
            validate_grant(&parent(), &bigger_budget)
                .unwrap_err()
                .code(),
            "CONTEXT_BUDGET_EXCEEDED"
        );

        let mut longer = grant();
        longer.expires_at = "2033-05-18T03:33:21Z".into();
        assert_eq!(
            validate_grant(&parent(), &longer).unwrap_err().code(),
            "POLICY_DENIED"
        );
    }

    #[test]
    fn grants_cannot_ask_for_a_budget_the_parent_already_spent() {
        let mut parent = parent();
        parent.remaining_tokens = 1_000;
        let error = validate_grant(&parent, &grant()).unwrap_err();
        match error {
            MemoryError::ContextBudgetExceeded {
                requested,
                remaining,
            } => {
                assert_eq!(requested, 8_192);
                assert_eq!(remaining, 1_000);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn grants_are_refused_beyond_max_depth_or_with_the_wrong_depth() {
        let mut deep_parent = parent();
        deep_parent.depth = MAX_DEPTH;
        let mut deep = grant();
        deep.depth = MAX_DEPTH + 1;
        assert_eq!(
            validate_grant(&deep_parent, &deep).unwrap_err().code(),
            "POLICY_DENIED"
        );

        let mut skipped = grant();
        skipped.depth = 2;
        assert_eq!(
            validate_grant(&parent(), &skipped).unwrap_err().code(),
            "POLICY_DENIED"
        );
    }

    #[test]
    fn grants_require_a_task_id_and_a_distinct_child() {
        let mut no_task = grant();
        no_task.task_id = " ".into();
        assert!(validate_grant(&parent(), &no_task).is_err());

        let mut same = grant();
        same.child_agent_id = "agent-main".into();
        assert!(validate_grant(&parent(), &same).is_err());

        let mut no_expiry = grant();
        no_expiry.expires_at = "not-a-date".into();
        assert_eq!(
            validate_grant(&parent(), &no_expiry).unwrap_err().code(),
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn concurrency_limit_and_timeout_are_enforced() {
        assert!(check_concurrency(0).is_ok());
        assert!(check_concurrency(MAX_CONCURRENT - 1).is_ok());
        assert_eq!(
            check_concurrency(MAX_CONCURRENT).unwrap_err().code(),
            "RESOURCE_EXHAUSTED"
        );
        assert_eq!(deadline(1_000), 1_000 + SUBAGENT_TIMEOUT_SECS);
        assert!(!is_timed_out(1_000, 1_299));
        assert!(is_timed_out(1_000, 1_300));
    }

    #[test]
    fn child_retention_horizons_follow_the_profile() {
        let horizons = ChildRetention::after_completion(0);
        assert_eq!(horizons.followup_until, 3_600);
        assert_eq!(horizons.archive_after, 86_400);
        assert_eq!(horizons.purge_eligible_after, 7 * 86_400);
    }

    #[test]
    fn lifecycle_only_allows_the_registered_transitions() {
        use AgentStatus::*;
        transition_status(Starting, Running).unwrap();
        transition_status(Running, Waiting).unwrap();
        transition_status(Waiting, Running).unwrap();
        transition_status(Running, Completed).unwrap();
        transition_status(Completed, Folding).unwrap();
        transition_status(Folding, Sealed).unwrap();
        transition_status(Sealed, CleanupEligible).unwrap();

        assert_eq!(
            transition_status(Running, Sealed).unwrap_err().code(),
            "LINK_STATE_CONFLICT"
        );
        assert!(transition_status(CleanupEligible, Folding).is_err());
        assert!(transition_status(Waiting, Folding).is_err());
        for terminal in [Completed, Partial, Failed, Cancelled, TimedOut] {
            assert!(transition_status(terminal, CleanupEligible).is_err());
        }
    }
}
