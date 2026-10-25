//! REST surface (§31.2).
//!
//! Every domain operation is `POST /api/v1/{domain}/{operation}` — reads are POST
//! too. `GET` exists only for health and job status, as the registry declares.

use ukma_contracts::generated::OPERATIONS;

/// Path prefix of the public API.
pub const API_PREFIX: &str = "/api/v1";
/// Health route.
pub const HEALTH_PATH: &str = "/api/v1/health";
/// Job status route prefix.
pub const JOB_PATH_PREFIX: &str = "/api/v1/jobs/";

/// One resolved route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A registered operation.
    Operation {
        /// Canonical operation id.
        operation_id: &'static str,
        /// HTTP method the registry declares.
        method: &'static str,
        /// Path.
        path: &'static str,
    },
    /// `GET /api/v1/health`.
    Health,
    /// `GET /api/v1/jobs/{id}`.
    JobStatus {
        /// Job id from the path.
        job_id: String,
    },
    /// The path exists but the method is not allowed.
    MethodNotAllowed {
        /// Method that is allowed.
        allowed: &'static str,
    },
    /// Nothing is registered at this path.
    NotFound,
}

/// Split a registry `server_method` into `(method, path)`.
pub fn route_of(server_method: &str) -> Option<(&str, &str)> {
    let mut parts = server_method.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    if parts.next().is_some() || !path.starts_with(API_PREFIX) {
        return None;
    }
    Some((method, path))
}

/// Resolve an HTTP request to a route.
pub fn resolve(method: &str, path: &str) -> Route {
    let method = method.to_ascii_uppercase();
    let path_only = path.split('?').next().unwrap_or(path).to_string();

    if path_only == HEALTH_PATH {
        return if method == "GET" {
            Route::Health
        } else {
            Route::MethodNotAllowed { allowed: "GET" }
        };
    }
    if let Some(job_id) = path_only.strip_prefix(JOB_PATH_PREFIX) {
        if !job_id.is_empty() && !job_id.contains('/') {
            return if method == "GET" {
                Route::JobStatus {
                    job_id: job_id.to_string(),
                }
            } else {
                Route::MethodNotAllowed { allowed: "GET" }
            };
        }
    }

    let registered = OPERATIONS.iter().find(|spec| {
        route_of(spec.server_method).map(|(_, route)| route) == Some(path_only.as_str())
    });
    match registered {
        Some(spec) => match route_of(spec.server_method) {
            Some((_declared, route)) if route == path_only => {
                if method == "POST" {
                    Route::Operation {
                        operation_id: spec.operation_id,
                        method: "POST",
                        path: spec
                            .server_method
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or(route),
                    }
                } else {
                    Route::MethodNotAllowed { allowed: "POST" }
                }
            }
            _ => Route::NotFound,
        },
        None => Route::NotFound,
    }
}

/// Operation id of a REST path, when registered.
pub fn operation_of_path(path: &str) -> Option<&'static str> {
    OPERATIONS
        .iter()
        .find(|spec| route_of(spec.server_method).map(|(_, route)| route) == Some(path))
        .map(|spec| spec.operation_id)
}

/// Health payload.
pub fn health_json() -> String {
    serde_json::json!({
        "spec_version": "1.2.3",
        "api_major": 1,
        "status": "ok",
        "operations": OPERATIONS.len(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_route_parses_and_resolves() {
        for spec in OPERATIONS {
            let (method, path) = route_of(spec.server_method).expect("parsable route");
            assert_eq!(method, "POST");
            assert_eq!(operation_of_path(path), Some(spec.operation_id));
            match resolve("POST", path) {
                Route::Operation { operation_id, .. } => {
                    assert_eq!(operation_id, spec.operation_id);
                }
                other => panic!("{path} resolved to {other:?}"),
            }
        }
    }

    #[test]
    fn reads_are_posted_and_get_is_refused() {
        match resolve("GET", "/api/v1/code/get_symbol") {
            Route::MethodNotAllowed { allowed } => assert_eq!(allowed, "POST"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn health_and_job_status_are_the_only_get_routes() {
        assert_eq!(resolve("GET", "/api/v1/health"), Route::Health);
        assert_eq!(
            resolve("POST", "/api/v1/health"),
            Route::MethodNotAllowed { allowed: "GET" }
        );
        assert_eq!(
            resolve("GET", "/api/v1/jobs/job-1"),
            Route::JobStatus {
                job_id: "job-1".to_string()
            }
        );
        assert_eq!(
            resolve("POST", "/api/v1/jobs/job-1"),
            Route::MethodNotAllowed { allowed: "GET" }
        );
        let json: serde_json::Value = serde_json::from_str(&health_json()).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["operations"], OPERATIONS.len());
    }

    #[test]
    fn unknown_paths_and_bad_methods_are_reported() {
        assert_eq!(resolve("POST", "/api/v1/nope/nothing"), Route::NotFound);
        assert_eq!(
            resolve("DELETE", "/api/v1/code/get_symbol"),
            Route::MethodNotAllowed { allowed: "POST" }
        );
        assert_eq!(resolve("GET", "/api/v1/jobs/"), Route::NotFound);
        assert!(route_of("GET /health").is_none());
        assert!(route_of("POST /api/v1/x extra").is_none());
        // Query strings are ignored.
        assert!(matches!(
            resolve("POST", "/api/v1/code/get_symbol?x=1"),
            Route::Operation { .. }
        ));
    }
}
