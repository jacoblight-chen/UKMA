//! Legacy CodeGraph MCP alias layer (§P8-WP5).
//!
//! The alias layer is **inert by default**. It can only be enabled with the pinned
//! upstream commit *and* a parity test recorded for every alias; until then the
//! difference matrix reports zero enabled aliases and no compatibility is claimed.

use std::collections::BTreeMap;

/// Upstream commit recorded in `docs/migration/upstream-lock.yaml`.
pub const PINNED_UPSTREAM_COMMIT: &str = "590c2ca7dd7b37bd1d41caf45fac535b6522f969";

/// One legacy tool name mapped onto a UKMA operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    /// Legacy tool name as exposed by the upstream server.
    pub legacy_tool: &'static str,
    /// UKMA operation it maps to.
    pub operation_id: &'static str,
    /// Parity test that proves the mapping, recorded in the migration ledger.
    pub parity_test: &'static str,
}

/// The alias table and its activation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasTable {
    pinned_commit: &'static str,
    enabled: bool,
    entries: Vec<AliasEntry>,
}

impl Default for AliasTable {
    fn default() -> Self {
        Self::disabled()
    }
}

impl AliasTable {
    /// A table that resolves nothing.
    pub fn disabled() -> Self {
        Self {
            pinned_commit: PINNED_UPSTREAM_COMMIT,
            enabled: false,
            entries: Vec::new(),
        }
    }

    /// Enable the layer for a pinned commit.
    ///
    /// Every entry must name a parity test, and the pinned commit must be the one
    /// recorded in the migration ledger: an unverified alias is refused rather
    /// than silently served.
    pub fn enable(pinned_commit: &'static str, entries: Vec<AliasEntry>) -> Result<Self, String> {
        if pinned_commit != PINNED_UPSTREAM_COMMIT {
            return Err(format!(
                "{pinned_commit} is not the pinned upstream commit {PINNED_UPSTREAM_COMMIT}"
            ));
        }
        for entry in &entries {
            if entry.parity_test.trim().is_empty() {
                return Err(format!(
                    "alias {} has no parity test and cannot be enabled",
                    entry.legacy_tool
                ));
            }
            if ukma_contracts::generated::OPERATIONS
                .iter()
                .all(|spec| spec.operation_id != entry.operation_id)
            {
                return Err(format!(
                    "alias {} targets unregistered operation {}",
                    entry.legacy_tool, entry.operation_id
                ));
            }
        }
        Ok(Self {
            pinned_commit,
            enabled: true,
            entries,
        })
    }

    /// Whether the layer is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Pinned commit the table was built for.
    pub fn pinned_commit(&self) -> &'static str {
        self.pinned_commit
    }

    /// Number of active aliases.
    pub fn len(&self) -> usize {
        if self.enabled {
            self.entries.len()
        } else {
            0
        }
    }

    /// Whether the table resolves nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Resolve a legacy tool name.
    pub fn resolve(&self, legacy_tool: &str) -> Option<&'static str> {
        if !self.enabled {
            return None;
        }
        self.entries
            .iter()
            .find(|entry| entry.legacy_tool == legacy_tool)
            .map(|entry| entry.operation_id)
    }

    /// Machine-readable difference matrix.
    pub fn difference_matrix_json(&self) -> String {
        let mut by_legacy: BTreeMap<&'static str, &'static str> = BTreeMap::new();
        for entry in &self.entries {
            by_legacy.insert(entry.legacy_tool, entry.operation_id);
        }
        serde_json::json!({
            "pinned_upstream_commit": self.pinned_commit,
            "enabled": self.enabled,
            "alias_count": self.len(),
            "aliases": by_legacy,
            "note": if self.enabled {
                "every alias has a recorded parity test; this is not a claim of full compatibility"
            } else {
                "alias layer inert: no measured upstream tool manifest with parity tests was recorded"
            },
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_table_resolves_nothing() {
        let table = AliasTable::disabled();
        assert!(!table.is_enabled());
        assert!(table.is_empty());
        assert_eq!(table.resolve("codegraph_find_symbol"), None);
        let matrix: serde_json::Value =
            serde_json::from_str(&table.difference_matrix_json()).unwrap();
        assert_eq!(matrix["alias_count"], 0);
        assert_eq!(matrix["enabled"], false);
        assert!(matrix["note"].as_str().unwrap().contains("inert"));
        assert_eq!(table.pinned_commit(), PINNED_UPSTREAM_COMMIT);
    }

    #[test]
    fn enabling_requires_the_pinned_commit_and_a_parity_test() {
        assert!(AliasTable::enable("0000000", Vec::new()).is_err());

        let no_parity = vec![AliasEntry {
            legacy_tool: "legacy_tool",
            operation_id: "code.get_symbol",
            parity_test: " ",
        }];
        assert!(AliasTable::enable(PINNED_UPSTREAM_COMMIT, no_parity)
            .unwrap_err()
            .contains("parity test"));

        let unregistered = vec![AliasEntry {
            legacy_tool: "legacy_tool",
            operation_id: "code.nope",
            parity_test: "tests/parity/legacy.rs",
        }];
        assert!(AliasTable::enable(PINNED_UPSTREAM_COMMIT, unregistered)
            .unwrap_err()
            .contains("unregistered"));

        let verified = vec![AliasEntry {
            legacy_tool: "legacy_tool",
            operation_id: "code.get_symbol",
            parity_test: "tests/parity/legacy.rs",
        }];
        let table = AliasTable::enable(PINNED_UPSTREAM_COMMIT, verified).unwrap();
        assert!(table.is_enabled());
        assert_eq!(table.len(), 1);
        assert_eq!(table.resolve("legacy_tool"), Some("code.get_symbol"));
        assert_eq!(table.resolve("other"), None);
        let matrix: serde_json::Value =
            serde_json::from_str(&table.difference_matrix_json()).unwrap();
        assert_eq!(matrix["alias_count"], 1);
        assert!(matrix["note"]
            .as_str()
            .unwrap()
            .contains("not a claim of full compatibility"));
    }
}
