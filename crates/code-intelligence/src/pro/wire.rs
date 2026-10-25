//! Wire payload types for the 27 CodeGraph Pro operations (§36.5).
//!
//! The Pro analyzers are keyed by a single identifier (a `revision`, a `file`,
//! or a `query`) and return a free-form JSON report. These typed wrappers give
//! the contract a resolvable, typed payload while staying honest about the
//! free-form nature of the analyzer output (the report body is transparent).

/// Key for the revision-scoped Pro operations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProRevisionRequest {
    /// The indexed revision to analyze.
    pub revision: String,
}

/// Key for `pro.mine_git_history_for_file`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProFileRequest {
    /// The repository-relative file path.
    pub file: String,
}

/// Key for `pro.cross_project_search` / `pro.search_git_history`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProQueryRequest {
    /// The search term.
    pub query: String,
}

/// Key for `pro.compare_symbols`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProCompareSymbolsRequest {
    /// The indexed revision the symbols resolve against.
    pub revision: String,
    /// The first symbol name.
    pub a: String,
    /// The second symbol name.
    pub b: String,
}

/// A Pro analyzer report. The body is a free-form JSON object (the detector's
/// output), wrapped so the payload stays typed while remaining honest about
/// its free-form nature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProReport {
    /// The analyzer's report payload (free-form JSON object).
    pub report: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_types_round_trip() {
        let request: ProRevisionRequest =
            serde_json::from_value(serde_json::json!({ "revision": "a".repeat(40) }))
                .expect("revision request decodes");
        assert_eq!(request.revision.len(), 40);
        let compare: ProCompareSymbolsRequest = serde_json::from_value(
            serde_json::json!({ "revision": "a".repeat(40), "a": "alpha", "b": "beta" }),
        )
        .expect("compare request decodes");
        assert_eq!(compare.a, "alpha");
        assert_eq!(compare.b, "beta");
        let report = ProReport {
            report: serde_json::json!({ "findings": [] }),
        };
        let value = serde_json::to_value(&report).expect("report serialises");
        assert_eq!(value["report"]["findings"].as_array().unwrap().len(), 0);
    }
}
