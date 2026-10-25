//! Dependency vulnerability source port (OSV online audit).
//!
//! The domain depends only on the [`VulnerabilitySource`] port. The default
//! [`OfflineVulnerabilitySource`] returns no findings, so `security_audit_deps`
//! reports the offline manifest. The [`OsVulnerabilitySource`] (feature `osv`)
//! queries the OSV API — a *network* operation, enabled by `--online`. On any
//! transport failure it degrades (the caller falls back to the manifest).

#[cfg(feature = "osv")]
use crate::CodeError;
use crate::CodeResult;

/// A dependency to audit (name, version, ecosystem).
#[derive(Debug, Clone, Default)]
pub struct DependencyRef {
    /// Package name.
    pub name: String,
    /// Resolved version (empty when the manifest does not pin one).
    pub version: String,
    /// OSV ecosystem (npm, PyPI, Go, crates.io, Maven, ...).
    pub ecosystem: String,
}

/// A single OSV vulnerability finding.
#[derive(Debug, Clone)]
pub struct OsVulnerability {
    /// The OSV / GHSA id.
    pub id: String,
    /// A short human summary.
    pub summary: String,
}

/// Port for dependency vulnerability lookup. The domain never touches the
/// network directly; it depends on this port.
pub trait VulnerabilitySource: Send + Sync {
    /// Audit one dependency. Empty when there is nothing to report.
    fn audit(&self, dep: &DependencyRef) -> CodeResult<Vec<OsVulnerability>>;

    /// Whether this source performs a live (network) lookup.
    fn is_online(&self) -> bool {
        false
    }
}

/// No findings (offline default).
#[derive(Debug, Default)]
pub struct OfflineVulnerabilitySource;

impl VulnerabilitySource for OfflineVulnerabilitySource {
    fn audit(&self, _dep: &DependencyRef) -> CodeResult<Vec<OsVulnerability>> {
        Ok(Vec::new())
    }
}

/// The OSV API (https://api.osv.dev). A network operation; on transport or
/// parse failure it returns an error so the caller can degrade to the
/// offline manifest.
#[cfg(feature = "osv")]
pub struct OsVulnerabilitySource {
    endpoint: String,
    timeout: std::time::Duration,
}

#[cfg(feature = "osv")]
impl OsVulnerabilitySource {
    /// Build with a custom endpoint (for tests) and a per-query timeout.
    pub fn with_endpoint(endpoint: String, timeout: std::time::Duration) -> Self {
        Self { endpoint, timeout }
    }
}

#[cfg(feature = "osv")]
impl Default for OsVulnerabilitySource {
    fn default() -> Self {
        Self {
            endpoint: "https://api.osv.dev/v1/query".to_string(),
            timeout: std::time::Duration::from_secs(10),
        }
    }
}

#[cfg(feature = "osv")]
impl VulnerabilitySource for OsVulnerabilitySource {
    fn is_online(&self) -> bool {
        true
    }

    fn audit(&self, dep: &DependencyRef) -> CodeResult<Vec<OsVulnerability>> {
        let body = serde_json::json!({
            "package": {
                "name": dep.name,
                "version": dep.version,
                "ecosystem": dep.ecosystem,
            }
        })
        .to_string();
        let response = ureq::post(self.endpoint.as_str())
            .timeout(self.timeout)
            .set("Content-Type", "application/json")
            .send_string(&body)
            .map_err(|err| CodeError::ResourceLimit {
                detail: format!("osv transport: {err}"),
            })?;
        let text = response
            .into_string()
            .map_err(|err| CodeError::ResourceLimit {
                detail: format!("osv body: {err}"),
            })?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|err| CodeError::ResourceLimit {
                detail: format!("osv response: {err}"),
            })?;
        let vulns = parsed
            .get("vulns")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let findings = vulns
            .iter()
            .filter_map(|v| {
                let id = v.get("id")?.as_str()?.to_string();
                let summary = v
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Some(OsVulnerability { id, summary })
            })
            .collect();
        Ok(findings)
    }
}

/// Infer the OSV ecosystem from a lockfile / manifest name.
pub fn ecosystem_for(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "package-lock.json" | "yarn.lock" | ".npmrc" => "npm".to_string(),
        "requirements.txt" | "pipfile.lock" | "poetry.lock" => "PyPI".to_string(),
        "go.sum" | "go.mod" => "Go".to_string(),
        "cargo.lock" => "crates.io".to_string(),
        "pom.xml" | "build.gradle" | "build.gradle.kts" => "Maven".to_string(),
        _ => "generic".to_string(),
    }
}
