//! P10-WP5: licence audit and release evidence (REP-003, R8).
//!
//! The inventory is derived data (`cargo metadata --locked --format-version 1`), so
//! the gate only trusts it when it matches the dependency tree and re-derives every
//! decision. Adding a dependency therefore breaks the build until the audit is
//! refreshed, and adding a copyleft-only dependency breaks it for good.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

const INVENTORY: &str = "contracts/testing/third-party-inventory.json";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Permissive licences the project accepts.
const ALLOWED: [&str; 15] = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "0BSD",
    "Zlib",
    "Unicode-3.0",
    "CC0-1.0",
    "MIT-0",
    "BSL-1.0",
    "CDLA-Permissive-2.0",
    "Unlicense",
    "MPL-2.0",
];

/// Copyleft families that must not appear unless a permissive option is elected.
const BLOCKED: [&str; 7] = [
    "GPL-2.0",
    "GPL-3.0",
    "AGPL",
    "SSPL",
    "BUSL",
    "Elastic-2.0",
    "Commons-Clause",
];

fn options(licence: &str) -> Vec<String> {
    let flattened = licence.replace(['(', ')'], " ").replace('/', " OR ");
    let mut parts = Vec::new();
    for chunk in flattened.split(" OR ") {
        for piece in chunk.split(" AND ") {
            let piece = piece.trim();
            if !piece.is_empty() {
                parts.push(piece.to_string());
            }
        }
    }
    parts
}

/// Whether a declared licence string may be used, and why.
fn decision(licence: &str) -> (&'static str, String) {
    let options = options(licence);
    let chosen: Vec<&String> = options
        .iter()
        .filter(|option| ALLOWED.contains(&option.as_str()))
        .collect();
    let blocked: Vec<&String> = options
        .iter()
        .filter(|option| BLOCKED.iter().any(|family| option.starts_with(family)))
        .collect();
    if chosen.is_empty() {
        return ("blocked", format!("no permissive option: {licence}"));
    }
    if !blocked.is_empty() {
        let elected: Vec<&str> = blocked.iter().map(|option| option.as_str()).collect();
        return (
            "allowed",
            format!("permissive option elected over {}", elected.join(", ")),
        );
    }
    if chosen.iter().any(|option| option.as_str() == "MPL-2.0") {
        return (
            "allowed",
            "file-level copyleft: dependency use only, do not modify or relicense MPL sources"
                .to_string(),
        );
    }
    ("allowed", "permissive".to_string())
}

/// `(name, version)` pairs from the workspace lockfile.
fn locked_packages() -> BTreeSet<String> {
    let text = read("Cargo.lock");
    let mut packages = BTreeSet::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("name = ") {
            name = Some(value.trim_matches('"').to_string());
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("version = ") {
            if let Some(name) = name.take() {
                packages.insert(format!("{name} {}", value.trim_matches('"')));
            }
        }
    }
    packages
}

#[test]
fn the_inventory_covers_the_whole_dependency_tree() {
    let inventory: Value = serde_json::from_str(&read(INVENTORY)).expect("the inventory is JSON");
    let packages = inventory["packages"].as_array().expect("packages array");

    let mut recorded: BTreeSet<String> = BTreeSet::new();
    for package in packages {
        let entry = format!(
            "{} {}",
            package["name"].as_str().expect("name"),
            package["version"].as_str().expect("version")
        );
        assert!(recorded.insert(entry.clone()), "{entry} is recorded twice");
    }

    // Every workspace crate must appear with a license field.
    let mut workspace_members = 0;
    for package in packages {
        if package["workspace_member"] == true {
            workspace_members += 1;
            assert_ne!(
                package["license"], "UNKNOWN",
                "a workspace crate has no licence"
            );
        }
    }
    assert!(
        workspace_members >= 27,
        "only {workspace_members} workspace crates recorded"
    );

    // The lockfile is the reference: an unrecorded or stale entry fails the gate.
    let locked = locked_packages();
    let missing: Vec<&String> = locked.difference(&recorded).collect();
    assert!(
        missing.is_empty(),
        "Cargo.lock packages missing from the inventory: {missing:?}"
    );
    let stale: Vec<&String> = recorded.difference(&locked).collect();
    assert!(
        stale.is_empty(),
        "the inventory lists packages that are no longer locked: {stale:?}"
    );
}

#[test]
fn every_recorded_decision_follows_the_policy() {
    let inventory: Value = serde_json::from_str(&read(INVENTORY)).expect("the inventory is JSON");
    let declared: Vec<&str> = inventory["policy"]["allowed"]
        .as_array()
        .expect("allowed list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for licence in ALLOWED {
        assert!(
            declared.contains(&licence),
            "{licence} is accepted by the gate but missing from the policy"
        );
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for package in inventory["packages"].as_array().expect("packages") {
        let licence = package["license"].as_str().expect("license");
        let recorded = package["decision"].as_str().expect("decision");
        let (expected, _reason) = decision(licence);
        assert_eq!(
            recorded, expected,
            "{} {} records `{recorded}` for `{licence}`",
            package["name"], package["version"]
        );
        *counts.entry(recorded).or_default() += 1;
    }
    assert_eq!(
        counts.get("blocked").copied().unwrap_or(0),
        0,
        "a blocked licence is present: {counts:?}"
    );
    assert_eq!(
        inventory["counts"]["unlicensed"].as_u64(),
        Some(0),
        "an unlicensed package is present"
    );
}

#[test]
fn no_copyleft_or_backend_sdk_reaches_the_build() {
    let inventory: Value = serde_json::from_str(&read(INVENTORY)).expect("the inventory is JSON");
    for package in inventory["packages"].as_array().expect("packages") {
        let name = package["name"].as_str().expect("name");
        for blocked in [
            "rocksdb",
            "librocksdb-sys",
            "sqlx",
            "qdrant-client",
            "neo4j",
        ] {
            assert_ne!(name, blocked, "{name} must not be a dependency");
        }
    }
    assert_eq!(
        inventory["policy"]["blocked_families"]
            .as_array()
            .map(Vec::len),
        Some(7)
    );
}

#[test]
fn the_licence_and_notice_files_exist_and_name_the_upstream_attribution() {
    let licence = read("LICENSE");
    assert!(
        licence.contains("Apache License") && licence.contains("Version 2.0"),
        "LICENSE is not the Apache-2.0 text"
    );
    assert!(licence.len() > 8_000, "LICENSE looks truncated");

    let notice = read("NOTICE");
    assert!(notice.contains("UKMA"));
    assert!(
        notice.contains("Andrey Vasilevsky"),
        "NOTICE must carry the upstream attribution the lockfile records"
    );
    assert!(
        notice.contains("590c2ca7dd7b37bd1d41caf45fac535b6522f969"),
        "NOTICE must name the pinned upstream commit"
    );
    assert!(notice.contains("contracts/testing/third-party-inventory.json"));

    // The upstream lockfile really records the same attribution.
    let lock = read("docs/migration/upstream-lock.yaml");
    assert!(lock.contains("Andrey Vasilevsky"));
    assert!(lock.contains("Apache-2.0"));
}

#[test]
fn every_workspace_crate_declares_the_apache_licence() {
    let mut checked = 0;
    for base in ["crates", "apps", "crates/adapters"] {
        let directory = root().join(base);
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&manifest).unwrap();
            assert!(
                text.contains("license = \"Apache-2.0\""),
                "{} does not declare its licence",
                manifest.display()
            );
            checked += 1;
        }
    }
    assert!(checked >= 25, "only {checked} manifests were checked");
}

#[test]
fn the_sbom_covers_every_locked_package() {
    let sbom: Value = serde_json::from_str(&read("contracts/testing/sbom.cyclonedx.json"))
        .expect("the SBOM is JSON");
    assert_eq!(sbom["bomFormat"], "CycloneDX");
    assert_eq!(sbom["specVersion"], "1.5");

    let components = sbom["components"].as_array().expect("components");
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    let mut workspace_members = 0;
    for component in components {
        let name = component["name"].as_str().expect("name");
        let version = component["version"].as_str().expect("version");
        assert!(
            recorded.insert(format!("{name} {version}")),
            "{name} {version} appears twice in the SBOM"
        );
        assert_eq!(
            component["purl"],
            format!("pkg:cargo/{name}@{version}"),
            "the purl must identify the crate"
        );
        // Every component states its licence, so the SBOM cannot hide one.
        assert!(
            component["licenses"]
                .as_array()
                .is_some_and(|licences| !licences.is_empty()),
            "{name} has no licence in the SBOM"
        );
        let member = component["properties"]
            .as_array()
            .and_then(|properties| {
                properties
                    .iter()
                    .find(|property| property["name"].as_str() == Some("ukma:workspace_member"))
            })
            .and_then(|property| property["value"].as_str());
        if member == Some("true") {
            workspace_members += 1;
        }
    }
    assert!(
        workspace_members >= 25,
        "only {workspace_members} workspace crates"
    );

    let locked = locked_packages();
    let missing: Vec<&String> = locked.difference(&recorded).collect();
    assert!(missing.is_empty(), "SBOM is missing: {missing:?}");
    let extra: Vec<&String> = recorded.difference(&locked).collect();
    assert!(extra.is_empty(), "SBOM lists stale packages: {extra:?}");
}

#[test]
fn the_sbom_digest_matches_the_file() {
    let manifest: Value = serde_json::from_str(&read("contracts/testing/sbom.digest.json"))
        .expect("the digest manifest is JSON");
    let recorded = manifest["sha256"].as_str().expect("sha256");
    let payload = read("contracts/testing/sbom.cyclonedx.json");
    let actual = {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(payload.as_bytes());
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    assert_eq!(
        actual, recorded,
        "the SBOM changed without refreshing its digest"
    );
    assert_eq!(
        manifest["bytes"].as_u64(),
        Some(payload.len() as u64),
        "the recorded size does not match"
    );
    // Stated honestly: a digest is integrity, not a signature.
    assert_eq!(manifest["signature"], Value::Null);
    assert!(manifest["note"]
        .as_str()
        .is_some_and(|note| note.contains("not a cryptographic signature")));
}

#[test]
fn the_release_evidence_pack_is_complete() {
    // The R8 evidence pack: every gate has an artefact a reviewer can open.
    for artefact in [
        "LICENSE",
        "NOTICE",
        "docs/testing/coverage-matrix.md",
        "contracts/testing/test-coverage.json",
        "contracts/testing/port-parity.json",
        "contracts/testing/third-party-inventory.json",
        "docs/migration/license-audit.md",
        "docs/migration/upstream-lock.yaml",
        "docs/implementation-status.yaml",
        "docs/performance/baseline.md",
        "docs/adr/ADR-055.md",
        "docs/adr/ADR-056.md",
        "docs/adr/ADR-057.md",
        "docs/adr/ADR-058.md",
        "contracts/schema/v1/wire.schema.json",
        "contracts/testing/sbom.cyclonedx.json",
        "contracts/testing/sbom.digest.json",
    ] {
        let path = root().join(artefact);
        assert!(
            path.is_file(),
            "{artefact} is missing from the evidence pack"
        );
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 100, "{artefact} looks empty ({size} bytes)");
    }
}
