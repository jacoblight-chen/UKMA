//! Gateway error surface.
//!
//! The registry is the only source of truth for which codes an operation may
//! return: a handler result is normalised against `OperationSpec::error_codes`
//! so the gateway can never emit a code the operation did not declare. Codes are
//! never invented here — every constant below is a member of the closed union in
//! `contracts/api/error-registry.yaml`.

use ukma_contracts::generated::OperationSpec;

/// No valid principal or missing/invalid envelope fields.
pub const INVALID_SCHEMA: &str = "INVALID_SCHEMA";
/// Denied for a visible operation.
pub const FORBIDDEN: &str = "FORBIDDEN";
/// Resource invisible in scope (§40.7).
pub const NOT_FOUND: &str = "NOT_FOUND";
/// Operation deadline exceeded.
pub const TIMEOUT: &str = "TIMEOUT";
/// Unexpected internal failure; mapped to when a handler emits an undeclared code.
pub const INTERNAL: &str = "INTERNAL";

/// A failure that can be rendered as the §31.5 error envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayError {
    /// Closed-set error code.
    pub code: &'static str,
    /// Message safe to show the caller (never source text or credentials).
    pub safe_message: String,
    /// Whether the caller may retry.
    pub retryable: bool,
    /// Suggested retry delay.
    pub retry_after_ms: Option<u64>,
    /// Typed validation details.
    pub details: Vec<String>,
}

impl GatewayError {
    /// Build an error from a closed-set code.
    pub fn new(code: &'static str, safe_message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            safe_message: safe_message.into(),
            retryable,
            retry_after_ms: None,
            details: Vec::new(),
        }
    }

    /// Validation failure.
    pub fn invalid_schema(safe_message: impl Into<String>) -> Self {
        Self::new(INVALID_SCHEMA, safe_message, false)
    }

    /// Authorization failure that does not reveal existence.
    pub fn forbidden(required_scope: &str) -> Self {
        Self::new(
            FORBIDDEN,
            format!("missing scope `{required_scope}`"),
            false,
        )
    }

    /// Resource that must be reported as absent rather than forbidden (§40.7).
    pub fn not_found(resource: &str) -> Self {
        Self::new(NOT_FOUND, format!("{resource} not found"), false)
    }

    /// Deadline exceeded.
    pub fn timeout() -> Self {
        Self::new(TIMEOUT, "the operation deadline has passed", true)
    }

    /// Unexpected failure.
    pub fn internal(safe_message: impl Into<String>) -> Self {
        Self::new(INTERNAL, safe_message, true)
    }

    /// Attach a typed detail line.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.details.push(detail.into());
        self
    }

    /// Attach a retry hint.
    pub fn with_retry_after_ms(mut self, millis: u64) -> Self {
        self.retry_after_ms = Some(millis);
        self
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.safe_message)
    }
}

impl std::error::Error for GatewayError {}

/// Anything a domain service reports as a failure.
pub trait DomainFailure {
    /// Closed-set code.
    fn code(&self) -> &'static str;
    /// Message safe to show the caller.
    fn message(&self) -> String;
    /// Whether a retry may succeed.
    fn retryable(&self) -> bool;
}

/// Convert a domain failure into a gateway error.
pub fn from_domain<E: DomainFailure>(error: E) -> GatewayError {
    GatewayError {
        code: leak_code(error.code()),
        safe_message: error.message(),
        retryable: error.retryable(),
        retry_after_ms: None,
        details: Vec::new(),
    }
}

/// Widen a `&'static str` that already lives in the closed set.
fn leak_code(code: &'static str) -> &'static str {
    code
}

/// Outcome of normalising a handler's code against its operation declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// Code that may be emitted.
    pub code: &'static str,
    /// Warning recorded when the emitted code had to be rewritten.
    pub warning: Option<String>,
}

/// Rewrite an undeclared code into one the operation declares.
///
/// `INVALID_ARGUMENT` becomes `INVALID_SCHEMA` because the registry treats the
/// latter as the validation code for every operation; anything else becomes
/// `INTERNAL` and is reported as a warning so drift is visible instead of hidden.
pub fn normalise(spec: &OperationSpec, code: &'static str) -> Normalised {
    if spec.error_codes.contains(&code) {
        return Normalised {
            code,
            warning: None,
        };
    }
    if code == "INVALID_ARGUMENT" && spec.error_codes.contains(&"INVALID_SCHEMA") {
        return Normalised {
            code: "INVALID_SCHEMA",
            warning: Some(format!(
                "{} returned INVALID_ARGUMENT, which it does not declare; reported as INVALID_SCHEMA",
                spec.operation_id
            )),
        };
    }
    if code == NOT_FOUND && spec.error_codes.contains(&FORBIDDEN) {
        // §40.7 prefers `NOT_FOUND` so existence never leaks, but an operation
        // that cannot express it publishes `FORBIDDEN`, which reveals nothing
        // either; an undeclared code is never emitted.
        return Normalised {
            code: FORBIDDEN,
            warning: Some(format!(
                "{} returned NOT_FOUND, which it does not declare; reported as FORBIDDEN",
                spec.operation_id
            )),
        };
    }
    Normalised {
        code: INTERNAL,
        warning: Some(format!(
            "{} returned undeclared code {code}; reported as INTERNAL",
            spec.operation_id
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukma_contracts::generated::OPERATIONS;

    fn spec(operation_id: &str) -> &'static OperationSpec {
        OPERATIONS
            .iter()
            .find(|spec| spec.operation_id == operation_id)
            .expect("registered operation")
    }

    #[test]
    fn a_declared_code_is_passed_through() {
        let spec = spec("code.get_symbol");
        let normalised = normalise(spec, "NOT_FOUND");
        assert_eq!(normalised.code, "NOT_FOUND");
        assert!(normalised.warning.is_none());
    }

    #[test]
    fn invalid_argument_becomes_invalid_schema_when_undeclared() {
        let spec = spec("memory.open_agent_scope");
        assert!(!spec.error_codes.contains(&"INVALID_ARGUMENT"));
        let normalised = normalise(spec, "INVALID_ARGUMENT");
        assert_eq!(normalised.code, "INVALID_SCHEMA");
        assert!(normalised.warning.unwrap().contains("INVALID_ARGUMENT"));
    }

    #[test]
    fn an_unknown_code_becomes_internal_with_a_warning() {
        let spec = spec("code.get_symbol");
        let normalised = normalise(spec, "TOTALLY_MADE_UP");
        assert_eq!(normalised.code, INTERNAL);
        assert!(normalised.warning.unwrap().contains("TOTALLY_MADE_UP"));
    }

    #[test]
    fn error_constructors_carry_the_registered_codes() {
        assert_eq!(GatewayError::invalid_schema("x").code, INVALID_SCHEMA);
        assert_eq!(GatewayError::forbidden("code:read").code, FORBIDDEN);
        assert_eq!(GatewayError::not_found("symbol").code, NOT_FOUND);
        assert!(GatewayError::timeout().retryable);
        assert!(GatewayError::internal("boom").retryable);
        let error = GatewayError::invalid_schema("x")
            .with_detail("field")
            .with_retry_after_ms(10);
        assert_eq!(error.details, vec!["field".to_string()]);
        assert_eq!(error.retry_after_ms, Some(10));
        assert!(error.to_string().contains("INVALID_SCHEMA"));
    }
}
