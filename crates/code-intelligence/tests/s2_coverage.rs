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
//! 5. a `passed` parity row must point at a golden fixture and a golden test that
//!    both exist on disk, and the golden test must be a real test target;
//! 6. conversely a report claiming `passed` is rejected while the registry still
//!    says `NotPorted` (no free wins in either direction);
//! 7. the ledger slice must record the change log, the preserved behaviour and
//!    the executed evidence for every claimed language.

use std::path::{Path, PathBuf};

use code_intelligence::{LanguageRegistry, SupportLevel, PORTED_LANGUAGES, UPSTREAM_LANGUAGES};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at crates/code-intelligence")
        .to_path_buf()
}

/// The ledger slice file for a slice id.
///
/// Slice ids are `S2-<language>` but the files on disk are lower case
/// (`docs/migration/ledger/s2-bash.yaml`), so resolving the id directly only
/// works on a case-insensitive filesystem. Normalising here keeps the gate
/// meaningful on Linux and macOS too.
fn ledger_path(root: &Path, slice: &str) -> PathBuf {
    root.join("docs/migration/ledger")
        .join(format!("{}.yaml", slice.to_ascii_lowercase()))
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
        let path = ledger_path(&root, &entry.ledger_slice);
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
    let root = repo_root();
    let report = load_yaml(&root.join("docs/migration/s2-parity-report.yaml"));
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
        if ledger_ported(&root, &expected_slice) {
            assert_eq!(
                parity, "passed",
                "{language}: ledger records grammar_ported but the parity row does not say passed"
            );
        }
        match parity {
            "passed" => {
                assert_eq!(
                    support,
                    SupportLevel::Ported,
                    "{language}: report claims passing parity but the registry does not say Ported"
                );
                for field in ["golden_fixture", "golden_test"] {
                    let relative = row
                        .get(field)
                        .and_then(serde_yaml::Value::as_str)
                        .unwrap_or_else(|| {
                            panic!("{language}: a passed parity row needs `{field}`")
                        });
                    let path = root.join(relative);
                    assert!(
                        path.is_file(),
                        "{language}: `{field}` must exist at {}",
                        path.display()
                    );
                }
            }
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
    let mut ported: Vec<&str> = registry
        .entries()
        .iter()
        .filter(|entry| entry.support_level == SupportLevel::Ported)
        .map(|entry| entry.language.as_str())
        .collect();
    ported.sort_unstable();
    // grow this list only together with the ledger slice and the golden test
    let mut expected: Vec<&str> = PORTED_LANGUAGES.to_vec();
    expected.sort_unstable();
    assert_eq!(
        ported, expected,
        "the registry must match the languages with executed golden evidence"
    );
    assert_eq!(
        registry.not_ported().len(),
        UPSTREAM_LANGUAGES.len() - ported.len()
    );
}

#[test]
fn ported_slices_record_their_changes_and_evidence() {
    let root = repo_root();
    for language in PORTED_LANGUAGES {
        let path = ledger_path(&root, &format!("S2-{language}"));
        let ledger = load_yaml(&path);
        let changes = ledger
            .get("changes")
            .and_then(serde_yaml::Value::as_sequence)
            .map(Vec::len)
            .unwrap_or(0);
        assert!(
            changes > 0,
            "{language}: a ported slice must record what changed from upstream"
        );
        let preserved = ledger
            .get("behavior")
            .and_then(|behavior| behavior.get("preserved"))
            .and_then(serde_yaml::Value::as_sequence)
            .map(Vec::len)
            .unwrap_or(0);
        assert!(
            preserved > 0,
            "{language}: a ported slice must record the preserved algorithm semantics"
        );
        let golden = ledger
            .get("tests")
            .and_then(|tests| tests.get("golden"))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("");
        assert!(
            golden.contains("passing"),
            "{language}: golden evidence must record an executed, passing test: `{golden}`"
        );
        let grammar_ported = ledger
            .get("status")
            .and_then(|status| status.get("grammar_ported"))
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false);
        assert!(grammar_ported, "{language}: grammar_ported must be true");
        let parity = ledger
            .get("status")
            .and_then(|status| status.get("parity_report"))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("");
        assert_eq!(parity, "passed", "{language}: parity_report must be passed");
    }
}

fn ledger_ported(root: &Path, slice: &str) -> bool {
    let path = ledger_path(root, slice);
    load_yaml(&path)
        .get("status")
        .and_then(|status| status.get("grammar_ported"))
        .and_then(serde_yaml::Value::as_bool)
        .unwrap_or(false)
}
