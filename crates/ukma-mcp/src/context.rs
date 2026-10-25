//! Trusted operation context (§31.4 "client 自報僅能作為期望 scope 驗證").
//!
//! The gateway authenticates the principal and injects the tenant/project/agent/
//! session itself: whatever the client sent is kept only as `expected_scopes` for
//! comparison, never as authority.

use ukma_contracts::{Consistency, RequestContext, ScopeRef};

use crate::error::{GatewayError, NOT_FOUND};

/// Authenticated context injected by the gateway.
#[derive(Debug, Clone)]
pub struct OpContext {
    /// Request id (echoed in the response envelope).
    pub request_id: String,
    /// Trace id.
    pub trace_id: String,
    /// Authenticated principal.
    pub principal_id: String,
    /// Trusted scope filled by the gateway.
    pub scope: ScopeRef,
    /// Scopes the principal actually holds.
    pub granted_scopes: Vec<String>,
    /// Scopes the client asked for; informational only.
    pub expected_scopes: Vec<String>,
    /// Deadline as an RFC 3339 instant.
    pub deadline_at: Option<String>,
    /// Idempotency key for mutating calls.
    pub idempotency_key: Option<String>,
    /// Optimistic concurrency tag.
    pub expected_etag: Option<String>,
    /// Requested consistency mode.
    pub consistency: Consistency,
    /// Required index generation for strict reads.
    pub required_generation: Option<String>,
    /// Delegation token of a sub-agent call.
    pub delegation_token: Option<String>,
}

impl OpContext {
    /// Build a context from an authenticated request envelope context.
    pub fn from_request(request: &RequestContext, granted_scopes: Vec<String>) -> Self {
        let expected_scopes = request.scopes.clone();
        Self {
            request_id: request.request_id.clone(),
            trace_id: request.trace_id.clone(),
            principal_id: request.principal_id.clone(),
            scope: request.scope.clone(),
            granted_scopes,
            expected_scopes,
            deadline_at: request.deadline_at.clone(),
            idempotency_key: request.idempotency_key.clone(),
            expected_etag: request.expected_etag.clone(),
            consistency: request.consistency.clone(),
            required_generation: request.required_generation.clone(),
            delegation_token: request.delegation_token.clone(),
        }
    }

    /// Whether the principal holds at least one of `scopes`.
    pub fn holds_any(&self, scopes: &[&str]) -> bool {
        scopes
            .iter()
            .any(|scope| self.granted_scopes.iter().any(|held| held == scope))
    }

    /// Deny-by-default scope check.
    pub fn require(&self, scopes: &[&str]) -> Result<(), GatewayError> {
        if scopes.is_empty() {
            return Ok(());
        }
        if self.holds_any(scopes) {
            return Ok(());
        }
        Err(GatewayError::forbidden(
            scopes.first().copied().unwrap_or("unknown"),
        ))
    }

    /// Whether the client asked for a scope the principal does not hold.
    ///
    /// A mismatch is not an authorization failure (the grant decides), but it is
    /// reported as a warning so a caller can see that its expectation was not met.
    pub fn expectation_mismatch(&self) -> Option<String> {
        let missing: Vec<&str> = self
            .expected_scopes
            .iter()
            .filter(|scope| !self.granted_scopes.contains(scope))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            None
        } else {
            Some(format!(
                "requested scopes not granted: {}",
                missing.join(", ")
            ))
        }
    }

    /// Project into the domain-level request context.
    pub fn to_request_context(&self) -> RequestContext {
        RequestContext {
            request_id: self.request_id.clone(),
            trace_id: self.trace_id.clone(),
            scope: self.scope.clone(),
            principal_id: self.principal_id.clone(),
            scopes: self.granted_scopes.clone(),
            deadline_at: self.deadline_at.clone(),
            consistency: self.consistency.clone(),
            required_generation: self.required_generation.clone(),
            snapshot_id: None,
            idempotency_key: self.idempotency_key.clone(),
            expected_etag: self.expected_etag.clone(),
            delegation_token: self.delegation_token.clone(),
        }
    }
}

/// Visibility decision for a resource (§40.7: never reveal hidden existence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// The caller may see the resource.
    Visible,
    /// The caller lacks a scope but the resource is in its own scope.
    Forbidden,
    /// The resource lives in another scope: report it as absent.
    Hidden,
}

/// Decide how an invisible object must be reported.
///
/// A resource in the caller's own tenant/project without the required scope is
/// `FORBIDDEN`; a resource in another tenant/project is `NOT_FOUND`, which is what
/// keeps the gateway from leaking existence across tenants.
pub fn visibility(has_scope: bool, same_tenant_project: bool) -> Visibility {
    if !same_tenant_project {
        Visibility::Hidden
    } else if has_scope {
        Visibility::Visible
    } else {
        Visibility::Forbidden
    }
}

/// Error for an invisible object.
pub fn visibility_error(visibility: Visibility, resource: &str) -> Option<GatewayError> {
    match visibility {
        Visibility::Visible => None,
        Visibility::Forbidden => Some(GatewayError::forbidden("resource access")),
        Visibility::Hidden => Some(GatewayError::not_found(resource)),
    }
}

/// Whether the same tenant/project pair is addressed.
pub fn same_scope(left: &ScopeRef, right: &ScopeRef) -> bool {
    left.tenant_id == right.tenant_id && left.project_id == right.project_id
}

/// Code used for a hidden resource.
pub const HIDDEN_CODE: &str = NOT_FOUND;

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RequestContext {
        RequestContext {
            request_id: "req-1".into(),
            trace_id: "trace-1".into(),
            scope: ScopeRef {
                tenant_id: "tenant-a".into(),
                project_id: "project-b".into(),
                repo_id: None,
                workspace_id: None,
                agent_id: Some("agent-main".into()),
                session_id: Some("s1".into()),
            },
            principal_id: "principal".into(),
            scopes: vec!["code:read".into(), "memory:write".into()],
            deadline_at: None,
            consistency: Consistency::Eventual,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: Some("idem-1".into()),
            expected_etag: None,
            delegation_token: None,
        }
    }

    #[test]
    fn the_trusted_scope_comes_from_the_gateway_not_the_client() {
        let request = context();
        let context = OpContext::from_request(&request, vec!["code:read".into()]);
        assert_eq!(context.granted_scopes, vec!["code:read".to_string()]);
        assert_eq!(
            context.expected_scopes,
            vec!["code:read".to_string(), "memory:write".to_string()]
        );
        assert!(context
            .expectation_mismatch()
            .unwrap()
            .contains("memory:write"));
        assert_eq!(context.scope.tenant_id, "tenant-a");
    }

    #[test]
    fn scope_checks_are_deny_by_default() {
        let context = OpContext::from_request(&context(), vec!["code:read".into()]);
        assert!(context.require(&["code:read"]).is_ok());
        assert!(context.require(&["code:read", "code:write"]).is_ok());
        assert!(context.require(&[]).is_ok());
        let error = context.require(&["code:write"]).unwrap_err();
        assert_eq!(error.code, "FORBIDDEN");
        assert!(error.safe_message.contains("code:write"));
    }

    #[test]
    fn projection_keeps_the_granted_scopes_and_ids() {
        let context = OpContext::from_request(&context(), vec!["code:read".into()]);
        let projected = context.to_request_context();
        assert_eq!(projected.scopes, vec!["code:read".to_string()]);
        assert_eq!(projected.idempotency_key.as_deref(), Some("idem-1"));
        assert_eq!(projected.principal_id, "principal");
    }

    #[test]
    fn a_hidden_resource_is_reported_as_absent() {
        assert_eq!(visibility(true, true), Visibility::Visible);
        assert_eq!(visibility(false, true), Visibility::Forbidden);
        assert_eq!(visibility(false, false), Visibility::Hidden);
        assert_eq!(visibility(true, false), Visibility::Hidden);

        assert!(visibility_error(Visibility::Visible, "symbol").is_none());
        assert_eq!(
            visibility_error(Visibility::Forbidden, "symbol")
                .unwrap()
                .code,
            "FORBIDDEN"
        );
        let hidden = visibility_error(Visibility::Hidden, "symbol").unwrap();
        assert_eq!(hidden.code, HIDDEN_CODE);
        assert!(hidden.safe_message.contains("not found"));
        assert!(!hidden.safe_message.contains("forbidden"));
    }

    #[test]
    fn same_scope_compares_tenant_and_project() {
        let mut other = context().scope;
        assert!(same_scope(&context().scope, &other));
        other.project_id = "project-z".into();
        assert!(!same_scope(&context().scope, &other));
    }
}
