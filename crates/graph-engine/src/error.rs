//! UKMA graph engine error type, mapped to registered v1.2.3 `ErrorCode` values.
//! No new wire codes are invented (gap report §Gap 3).

use std::fmt;

use ukma_contracts::ErrorDetail;

/// Graph engine failures, each mapped to a registered v1.2.3 error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    /// Entity absent in the requested scope. Cross-scope reads surface as this
    /// variant deliberately: existence is never leaked (spec: FORBIDDEN does
    /// not return whether a resource exists).
    NotFound {
        /// Entity class ("node" | "edge").
        entity: &'static str,
        /// Canonical id that was requested.
        id: String,
    },
    /// Optimistic concurrency mismatch (stale `expected_revision`).
    Conflict {
        /// Entity class ("node" | "edge").
        entity: &'static str,
        /// Canonical id of the entity.
        id: String,
        /// Revision the caller expected.
        expected_revision: u64,
        /// Revision actually observed.
        actual_revision: u64,
    },
    /// Same idempotency key replayed with a different payload fingerprint.
    IdempotencyConflict {
        /// Canonical id concerned.
        id: String,
    },
    /// Malformed argument (empty scope field, malformed canonical id, ...).
    InvalidArgument {
        /// Argument name.
        field: &'static str,
        /// Why the argument is invalid.
        reason: String,
    },
    /// The active store cannot provide the requested capability (probed, not
    /// assumed — ADR-022).
    CapabilityUnavailable {
        /// Capability name (e.g. "durable_flush").
        capability: &'static str,
    },
    /// A write was attempted across a scope boundary.
    CrossScopeDenied {
        /// Tenant of the attempted write.
        tenant_id: String,
        /// Project of the attempted write.
        project_id: String,
    },
    /// A traversal bound was reached (bounded termination, PROP-03).
    ResourceLimit {
        /// The bound value.
        limit: u64,
    },
    /// Unexpected internal failure.
    Internal(String),
}

impl GraphError {
    /// Registered v1.2.3 `ErrorCode` for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            GraphError::NotFound { .. } => "NOT_FOUND",
            GraphError::Conflict { .. } => "ETAG_MISMATCH",
            GraphError::IdempotencyConflict { .. } => "IDEMPOTENCY_CONFLICT",
            GraphError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            GraphError::CapabilityUnavailable { .. } => "CAPABILITY_UNAVAILABLE",
            GraphError::CrossScopeDenied { .. } => "CROSS_SCOPE_DENIED",
            GraphError::ResourceLimit { .. } => "RESOURCE_LIMIT",
            GraphError::Internal(_) => "INTERNAL",
        }
    }

    /// Whether the caller may retry the same request.
    pub fn retryable(&self) -> bool {
        matches!(self, GraphError::Internal(_))
    }

    /// Wire form per the v1.2.3 `ErrorDetail` contract.
    pub fn error_detail(&self) -> ErrorDetail {
        ErrorDetail {
            code: self.code().to_string(),
            safe_message: self.safe_message(),
            retryable: self.retryable(),
            retry_after_ms: None,
        }
    }

    fn safe_message(&self) -> String {
        match self {
            GraphError::NotFound { entity, id } => {
                format!("{entity} {id} not found in scope")
            }
            GraphError::Conflict {
                entity,
                id,
                expected_revision,
                actual_revision,
            } => format!(
                "revision mismatch for {entity} {id}: expected {expected_revision}, actual {actual_revision}"
            ),
            GraphError::IdempotencyConflict { id } => {
                format!("idempotency key replay with different payload for {id}")
            }
            GraphError::InvalidArgument { field, reason } => {
                format!("{field}: {reason}")
            }
            GraphError::CapabilityUnavailable { capability } => {
                format!("capability {capability} unavailable on active store")
            }
            GraphError::CrossScopeDenied {
                tenant_id,
                project_id,
            } => format!("write denied across scope {tenant_id}/{project_id}"),
            GraphError::ResourceLimit { limit } => format!("traversal bound {limit} reached"),
            GraphError::Internal(msg) => msg.clone(),
        }
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.safe_message())
    }
}

impl std::error::Error for GraphError {}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, GraphError>;
