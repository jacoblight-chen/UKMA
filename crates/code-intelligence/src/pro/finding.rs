//! Pro finding model + SARIF 2.1.0 export (CodeGraph Pro parity).
//!
//! A [`Finding`] is a single, location-anchored result from a Pro detector.
//! The SARIF export is a faithful SARIF 2.1.0 document so findings can be
//! consumed by any SARIF tooling (CodeGraph `security_export_sarif` parity).

use serde::{Deserialize, Serialize};

/// Severity of a Pro finding (ordered, for sorting and SARIF `level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational.
    Info,
    /// Warning.
    Warning,
    /// Error.
    Error,
    /// Critical.
    Critical,
}

impl Severity {
    /// The SARIF 2.1.0 `level` string.
    pub fn sarif_level(self) -> &'static str {
        match self {
            Severity::Info => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Critical => "error",
        }
    }
}

/// A single Pro finding (CodeGraph-compatible shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable finding id (rule + file + line + message digest).
    pub id: String,
    /// The rule / detector that produced it.
    pub rule: String,
    /// Severity.
    pub severity: Severity,
    /// The file (relative path).
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// Human-readable message.
    pub message: String,
    /// Optional evidence (the offending snippet).
    pub evidence: Option<String>,
}

impl Finding {
    /// Build a finding with a stable id derived from rule + file + line.
    pub fn new(
        rule: &str,
        severity: Severity,
        file: &str,
        line: u32,
        message: &str,
        evidence: Option<String>,
    ) -> Self {
        Self {
            id: format!("{rule}:{file}:{line}"),
            rule: rule.to_string(),
            severity,
            file: file.to_string(),
            line,
            message: message.to_string(),
            evidence,
        }
    }
}

/// Export findings as a SARIF 2.1.0 document (CodeGraph
/// `security_export_sarif` parity).
pub fn to_sarif(findings: &[Finding], tool_name: &str) -> serde_json::Value {
    let results: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "ruleId": f.rule,
                "level": f.severity.sarif_level(),
                "message": { "text": f.message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": f.file },
                        "region": { "startLine": f.line }
                    }
                }]
            })
        })
        .collect();
    serde_json::json!({
        "version": "2.1.0",
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemas/sarif-2.1.0.json",
        "runs": [{
            "tool": {
                "driver": {
                    "name": tool_name,
                    "version": "0.1.0",
                    "informationUri": "https://ukma.dev"
                }
            },
            "results": results
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Finding {
        Finding::new(
            "hardcoded-secret",
            Severity::Error,
            "src/a.rs",
            10,
            "possible hardcoded secret",
            Some("let api_key = \"sk-...\";".into()),
        )
    }

    #[test]
    fn finding_id_is_stable_and_derived() {
        let f = finding();
        assert_eq!(f.id, "hardcoded-secret:src/a.rs:10");
        assert_eq!(finding().id, f.id, "the id is deterministic");
    }

    #[test]
    fn the_sarif_export_is_a_valid_2_1_0_document() {
        let sarif = to_sarif(&[finding()], "ukma-pro");
        assert_eq!(sarif["version"], "2.1.0");
        assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "ukma-pro");
        let result = &sarif["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "hardcoded-secret");
        assert_eq!(result["level"], "error");
        assert_eq!(
            result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "src/a.rs"
        );
        assert_eq!(
            result["locations"][0]["physicalLocation"]["region"]["startLine"],
            10
        );
        assert_eq!(result["message"]["text"], "possible hardcoded secret");
    }
}
