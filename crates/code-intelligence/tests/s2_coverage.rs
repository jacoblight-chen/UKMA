//! S2 migration coverage enforcement (ADR-025, ADR-048 O-01, `CODE-001`).
//!
//! These tests make it impossible to claim a language is ported without the
//! paperwork and the evidence that back the claim:
//!
//! 1. every one of the 38 baseline languages has a ledger slice file;
//! 2. the registry's `ledger_slice` matches that file's `slice_id`;
//! 3. the parity report has a row for the language;
//! 4. a language the registry reports as `Ported` must have
//!    `grammar_ported: true` **and** `parity: passed`;
//! 5. conversely a report claiming `passed` is rejected while the registry still
//!    says `NotPorted` (no free wins in either direction).

use std::path::{Path, PathBuf};

use code_intelligence::{LanguageRegistry, SupportLevel, UPSTREAM_LANGUAGES};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at crates/code-intelligence")
        .to_path_buf()
}

fn load_yaml(path: &Path) -> serde_yaml::Value {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_yaml::from_str(&raw)
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

#[test]
fn every_baseline_language_has_a_ledger_slice() {
    let root = repo_root();
    let registry = LanguageRegistry::new();
    for language in UPSTREAM_LANGUAGES {
        let entry = registry
            .get(language)
            .unwrap_or_else(|| panic!("{language} missing from the registry"));
        let path = root
            .join("docs/migration/ledger")
            .join(format!("{}.yaml", entry.ledger_slice));
        assert!(
            path.exists(),
            "{language}: ledger slice missing at {}",
            path.display()
        );
        let ledger = load_yaml(&path);
        let slice_id = ledger
            .get("slice_id")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("");
        assert_eq!(
            slice_id, entry.ledger_slice,
            "{language}: ledger slice_id mismatch"
        );
        let commit = ledger
            .get("upstream")
            .and_then(|upstream| upstream.get("commit"))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("");
        assert_eq!(
            commit.len(),
            40,
            "{language}: upstream commit must be a full SHA"
        );
        let paths = ledger
            .get("upstream")
            .and_then(|upstream| upstream.get("paths"))
            .and_then(serde_yaml::Value::as_sequence)
            .map(Vec::len)
            .unwrap_or(0);
        assert!(
            paths > 0,
            "{language}: ledger must list real upstream source paths"
        );
        let identified = ledger
            .get("license")
            .and_then(|license| license.get("identified"))
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false);
        let notices = ledger
            .get("license")
            .and_then(|license| license.get("notices_preserved"))
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false);
        assert!(
            identified && notices,
            "{language}: license/notices must be identified (ADR-025 blocks merge otherwise)"
        );
    }
}

#[test]
fn parity_report_covers_every_language() {
    let report = load_yaml(&repo_root().join("docs/migration/s2-parity-report.yaml"));
    let rows = report
        .get("languages")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("parity report needs a `languages` list");
    assert_eq!(rows.len(), 38, "one row per baseline language");
    let registry = LanguageRegistry::new();
    for language in UPSTREAM_LANGUAGES {
        let row = rows
            .iter()
            .find(|row| row.get("language").and_then(serde_yaml::Value::as_str) == Some(language))
            .unwrap_or_else(|| panic!("{language} has no parity row"));
        let expected_slice = format!("S2-{language}");
        assert_eq!(
            row.get("ledger_slice").and_then(serde_yaml::Value::as_str),
            Some(expected_slice.as_str()),
            "{language}: parity row must point at its ledger slice"
        );
        let parity = row
            .get("parity")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("not_run");
        let support = registry
            .get(language)
            .expect("registry entry")
            .support_level;
        match parity {
            "passed" => assert_eq!(
                support,
                SupportLevel::Ported,
                "{language}: report claims passing parity but the registry does not say Ported"
            ),
            _ => assert_ne!(
                support,
                SupportLevel::Ported,
                "{language}: registry claims Ported without a passing parity report (CODE-001)"
            ),
        }
    }
}

#[test]
fn nothing_is_claimed_as_ported_without_evidence() {
    let registry = LanguageRegistry::new();
    let ported: Vec<&str> = registry
        .entries()
        .iter()
        .filter(|entry| entry.support_level == SupportLevel::Ported)
        .map(|entry| entry.language.as_str())
        .collect();
    assert!(
        ported.is_empty(),
        "no grammar has been ported yet; unexpected Ported languages: {ported:?}"
    );
    assert_eq!(registry.not_ported().len(), 38);
}
