//! Storage port errors, mapped to the closed v1.2.3 `ErrorCode` set (ADR-035).

use std::fmt;

/// Failure raised by a storage port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// Record absent in the requested scope (cross-scope reads also land here:
    /// existence is never leaked, §40.7).
    NotFound {
        /// Logical collection or table name.
        collection: &'static str,
        /// Record key.
        key: String,
    },
    /// Compare-and-swap failure.
    Conflict {
        /// Logical collection or table name.
        collection: &'static str,
        /// Record key.
        key: String,
        /// Revision the caller expected.
        expected_revision: u64,
        /// Revision actually observed.
        actual_revision: u64,
    },
    /// The adapter cannot provide the requested capability (ADR-022/038).
    CapabilityUnavailable {
        /// Capability name.
        capability: &'static str,
    },
    /// Malformed argument or scope.
    InvalidArgument {
        /// Offending field.
        field: &'static str,
        /// Why it is invalid.
        reason: String,
    },
    /// A declared bound was exceeded; never silently truncate (§31.4).
    ResourceLimit {
        /// Bound value.
        limit: u64,
    },
    /// Embedding profile mismatch (§25.2, DB-004).
    ModelProfileMismatch {
        /// Human-readable mismatch detail.
        detail: String,
    },
    /// Underlying I/O or serialization failure.
    Internal(String),
}

impl StorageError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            StorageError::NotFound { .. } => "NOT_FOUND",
            StorageError::Conflict { .. } => "ETAG_MISMATCH",
            StorageError::CapabilityUnavailable { .. } => "CAPABILITY_UNAVAILABLE",
            StorageError::InvalidArgument { .. } => "INVALID_ARGUMENT",
            StorageError::ResourceLimit { .. } => "RESOURCE_LIMIT",
            StorageError::ModelProfileMismatch { .. } => "MODEL_PROFILE_MISMATCH",
            StorageError::Internal(_) => "INTERNAL",
        }
    }

    /// Whether the caller may retry the same request.
    pub fn retryable(&self) -> bool {
        matches!(self, StorageError::Internal(_))
    }

    /// Safe, non-sensitive message.
    pub fn safe_message(&self) -> String {
        match self {
            StorageError::NotFound { collection, key } => {
                format!("{collection}/{key} not found in scope")
            }
            StorageError::Conflict {
                collection,
                key,
                expected_revision,
                actual_revision,
            } => format!(
                "{collection}/{key} revision mismatch: expected {expected_revision}, actual {actual_revision}"
            ),
            StorageError::CapabilityUnavailable { capability } => {
                format!("capability `{capability}` unavailable on the active adapter")
            }
            StorageError::InvalidArgument { field, reason } => format!("{field}: {reason}"),
            StorageError::ResourceLimit { limit } => format!("bound {limit} exceeded"),
            StorageError::ModelProfileMismatch { detail } => format!("model profile: {detail}"),
            StorageError::Internal(message) => message.clone(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.safe_message())
    }
}

impl std::error::Error for StorageError {}

/// Port result alias.
pub type PortResult<T> = Result<T, StorageError>;

/// Convert an I/O error into a port error with path context.
pub fn io_error(path: &std::path::Path, error: std::io::Error) -> StorageError {
    StorageError::Internal(format!("io error on {}: {error}", path.display()))
}
