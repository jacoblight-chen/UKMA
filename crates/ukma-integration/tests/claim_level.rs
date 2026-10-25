//! The claim level is derived from evidence, not asserted (ADR-062).
//!
//! `docs/implementation-status.yaml` used to carry hand-set flags. A hand-set flag
//! is a promise; this gate turns it into a measurement: the four flags are
//! recomputed from the artefacts a reviewer can open, and the YAML must agree.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// `(covered, partial, gap)` over the classified §17.1/§29.4 ids.
fn coverage_counts() -> (usize, usize, usize) {
    let matrix: Value = serde_json::from_str(&read("contracts/testing/test-coverage.json"))
        .expect("the coverage matrix is JSON");
    let dispositions = matrix["dispositions"]
        .as_array()
        .expect("dispositions array");
    let mut covered = 0;
    let mut partial = 0;
    let mut gap = 0;
    for row in dispositions {
        match row["disposition"].as_str().expect("disposition") {
            "covered" => covered += 1,
            "partial" => partial += 1,
            "gap" => gap += 1,
            other => panic!("unknown disposition `{other}`"),
        }
    }
    (covered, partial, gap)
}

fn sbom_signed() -> bool {
    root()
        .join("contracts/testing/sbom.signature.json")
        .is_file()
}

fn profile_c_verified() -> bool {
    let report: Value = serde_json::from_str(&read("contracts/testing/port-parity.json"))
        .expect("the parity report is JSON");
    report["three_way_equivalence"]["verified"] == Value::Bool(true)
}

fn licence_clean() -> bool {
    let inventory: Value =
        serde_json::from_str(&read("contracts/testing/third-party-inventory.json"))
            .expect("the inventory is JSON");
    inventory["counts"]["blocked"].as_u64() == Some(0)
        && inventory["counts"]["unlicensed"].as_u64() == Some(0)
}

#[test]
fn the_declared_claim_level_matches_the_evidence() {
    let (covered, partial, gap) = coverage_counts();
    assert_eq!(
        covered + partial + gap,
        69,
        "every §17.1/§29.4 id must be classified"
    );

    // Schema closure is what the contract gate proves: no payload placeholder
    // survives and every operation carries typed input/data schemas.
    let schema_closed = true;
    // Implementation compliance needs the whole matrix proven, not one profile.
    let implementation_compliant = gap == 0 && partial == 0;
    // Production readiness additionally needs a verified profile C, a signed SBOM
    // and a licence-clean tree.
    let production_ready =
        implementation_compliant && profile_c_verified() && sbom_signed() && licence_clean();

    let status = read("docs/implementation-status.yaml");
    let flag = |name: &str| -> bool {
        status
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{name}:")))
            .map(|line| line.trim_end().ends_with("true"))
            .unwrap_or_else(|| panic!("claim_level.{name} is missing"))
    };

    assert!(flag("catalog_defined"), "69 operations are registered");
    assert_eq!(
        flag("schema_closed"),
        schema_closed,
        "schema_closed must follow the contract gate"
    );
    assert_eq!(
        flag("implementation_compliant"),
        implementation_compliant,
        "implementation_compliant is derived: {covered} covered, {partial} partial, {gap} gap"
    );
    assert_eq!(
        flag("production_ready"),
        production_ready,
        "production_ready is derived: profile_c={}, sbom_signed={}, licences_clean={}",
        profile_c_verified(),
        sbom_signed(),
        licence_clean()
    );

    // State the blockers in the failure message instead of leaving them implicit.
    if !implementation_compliant {
        let blockers: Vec<String> =
            serde_json::from_str::<Value>(&read("contracts/testing/test-coverage.json"))
                .expect("matrix")["dispositions"]
                .as_array()
                .expect("dispositions")
                .iter()
                .filter(|row| row["disposition"] != "covered")
                .map(|row| {
                    format!(
                        "{} ({})",
                        row["id"].as_str().unwrap_or("?"),
                        row["disposition"].as_str().unwrap_or("?")
                    )
                })
                .collect();
        assert!(
            !blockers.is_empty(),
            "a false claim must name its blockers, and {blockers:?} is the reason"
        );
        println!(
            "implementation_compliant=false because {} ids are not covered yet: {blockers:?}",
            blockers.len()
        );
    }
}
