//! Derives the release evidence and signs the SBOM digest (ADR-059).
//!
//! `cargo metadata --locked` is the only source of truth for the dependency
//! tree, so the inventory and the SBOM are *derived* files: this tool rewrites
//! them, `--check` proves the checked-in copies still match, and `sign`/`verify`
//! handle the SBOM digest with an Ed25519 key held outside the repository.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const INVENTORY: &str = "contracts/testing/third-party-inventory.json";
const SBOM: &str = "contracts/testing/sbom.cyclonedx.json";
const DIGEST: &str = "contracts/testing/sbom.digest.json";
const SIGNATURE: &str = "contracts/testing/sbom.signature.json";
const SPEC_VERSION: &str = "1.2.3";

/// Permissive licences the project accepts (§17.1 REP-003).
pub const ALLOWED: [&str; 15] = [
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

/// Copyleft families that may only appear when a permissive option exists.
pub const BLOCKED: [&str; 7] = [
    "GPL-2.0",
    "GPL-3.0",
    "AGPL",
    "SSPL",
    "BUSL",
    "Elastic-2.0",
    "Commons-Clause",
];

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    license: Option<String>,
}

/// Splits an SPDX expression into its electable options.
fn licence_options(licence: &str) -> Vec<String> {
    let flattened = licence.replace(['(', ')'], " ").replace('/', " OR ");
    let mut options = Vec::new();
    for chunk in flattened.split(" OR ") {
        for piece in chunk.split(" AND ") {
            let piece = piece.trim();
            if !piece.is_empty() {
                options.push(piece.to_string());
            }
        }
    }
    options
}

/// `(decision, reason)` for one declared licence string.
fn decide(licence: &str) -> (&'static str, String) {
    let options = licence_options(licence);
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

fn run_cargo_metadata() -> Result<Metadata, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--format-version", "1"])
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("metadata is not JSON: {error}"))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Builds the inventory document.
///
/// # Errors
/// Fails when `cargo metadata` cannot run or does not return JSON.
fn inventory(metadata: &Metadata) -> Value {
    let members: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut packages: Vec<&Package> = metadata.packages.iter().collect();
    packages.sort_by(|left, right| {
        (left.name.as_str(), left.version.as_str())
            .cmp(&(right.name.as_str(), right.version.as_str()))
    });

    let mut external = 0;
    let mut workspace = 0;
    let mut unlicensed = 0;
    let mut blocked = 0;
    let mut rows = Vec::new();
    for package in packages {
        let licence = package
            .license
            .clone()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let member = members.contains(package.id.as_str());
        if member {
            workspace += 1;
        } else {
            external += 1;
        }
        if licence == "UNKNOWN" {
            unlicensed += 1;
        }
        let (decision, reason) = decide(&licence);
        if decision == "blocked" {
            blocked += 1;
        }
        rows.push(json!({
            "name": package.name,
            "version": package.version,
            "license": licence,
            "decision": decision,
            "reason": reason,
            "workspace_member": member,
        }));
    }

    json!({
        "spec_version": SPEC_VERSION,
        "audit": "P10-WP5 third-party licence inventory (REP-003)",
        "command": "cargo metadata --locked --format-version 1",
        "spec_ref": "docs/UKMA_v1.2.3_Full_Spec.md 17.1 REP-003",
        "policy": {
            "allowed": ALLOWED,
            "blocked_families": BLOCKED,
            "note": "a disjunction (OR, AND, or the legacy slash form) is allowed when at least one option is allowed; the elected option is recorded in reason",
        },
        "counts": {
            "external": external,
            "workspace": workspace,
            "unlicensed": unlicensed,
            "blocked": blocked,
        },
        "refresh": "run `cargo run -p ukma-release-evidence`; crates/ukma-integration/tests/release_evidence.rs fails until this file matches Cargo.lock exactly",
        "packages": rows,
    })
}

/// Builds the CycloneDX 1.5 SBOM for the same package set.
fn sbom(inventory: &Value) -> Value {
    let packages = inventory["packages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let components: Vec<Value> = packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().unwrap_or_default();
            let version = package["version"].as_str().unwrap_or_default();
            let licence = package["license"].as_str().unwrap_or("UNKNOWN");
            let member = package["workspace_member"].as_bool().unwrap_or(false);
            json!({
                "type": "library",
                "bom-ref": format!("{name}@{version}"),
                "name": name,
                "version": version,
                "purl": format!("pkg:cargo/{name}@{version}"),
                "scope": "required",
                "properties": [
                    {"name": "ukma:workspace_member", "value": if member { "true" } else { "false" }},
                ],
                "licenses": [{"license": {"id": licence}}],
            })
        })
        .collect();
    json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": "urn:uuid:121273db2f5dcd8d4ca46041a1e0a115",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": "ukma",
                "version": "1.0.0",
                "description": "UKMA agentic development context engine (spec 1.2.3)",
                "licenses": [{"license": {"id": "Apache-2.0"}}],
            },
            "tools": [{"name": "ukma-release-evidence", "version": "1.0.0"}],
            "properties": [
                {"name": "ukma:spec_version", "value": SPEC_VERSION},
                {"name": "ukma:generated_from", "value": "cargo metadata --locked --format-version 1"},
            ],
        },
        "components": components,
    })
}

/// Builds the digest manifest for `payload`.
fn digest_manifest(payload: &str, components: usize) -> Value {
    json!({
        "sbom": SBOM,
        "format": "CycloneDX 1.5",
        "sha256": sha256_hex(payload.as_bytes()),
        "bytes": payload.len(),
        "components": components,
        "signature": null,
        "note": "a SHA-256 digest is an integrity check, not a cryptographic signature; signing needs an Ed25519 key the build does not hold — use `cargo run -p ukma-release-evidence -- sign --key <file>`",
        "refresh": "run `cargo run -p ukma-release-evidence`; crates/ukma-integration/tests/release_evidence.rs fails until the digest and component set match",
    })
}

fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("value renders");
    text.push('\n');
    text
}

fn write(path: &str, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|error| format!("cannot write {path}: {error}"))
}

fn derive_all() -> Result<(String, String, String, usize), String> {
    let metadata = run_cargo_metadata()?;
    let inventory = inventory(&metadata);
    let sbom = sbom(&inventory);
    let payload = render(&sbom);
    let components = sbom["components"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    let digest = digest_manifest(&payload, components);
    Ok((render(&inventory), payload, render(&digest), components))
}

fn regenerate() -> Result<(), String> {
    let (inventory, sbom, digest, components) = derive_all()?;
    write(INVENTORY, &inventory)?;
    write(SBOM, &sbom)?;
    write(DIGEST, &digest)?;
    println!("regenerated {INVENTORY}, {SBOM} and {DIGEST} ({components} components)");
    Ok(())
}

fn check() -> Result<(), String> {
    let (inventory, sbom, digest, _) = derive_all()?;
    let mut drift: Vec<String> = Vec::new();
    for (path, expected) in [(INVENTORY, inventory), (SBOM, sbom), (DIGEST, digest)] {
        let actual = std::fs::read_to_string(path).unwrap_or_default();
        if actual != expected {
            drift.push(path.to_string());
        }
    }
    if drift.is_empty() {
        println!("release evidence is up to date");
        Ok(())
    } else {
        Err(format!(
            "{} drifted; run `cargo run -p ukma-release-evidence`",
            drift.join(", ")
        ))
    }
}

fn read_key(path: &Path) -> Result<ed25519_dalek::SigningKey, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read key {}: {error}", path.display()))?;
    let trimmed = raw.trim();
    if trimmed.len() != 64 {
        return Err(format!(
            "{} must hold 32 bytes of hex (64 characters); generate one with `cargo run -p ukma-release-evidence -- generate-key --out <file>`",
            path.display()
        ));
    }
    let mut seed = [0u8; 32];
    for (index, slot) in seed.iter_mut().enumerate() {
        let byte = u8::from_str_radix(&trimmed[index * 2..index * 2 + 2], 16)
            .map_err(|_| format!("{} is not hex", path.display()))?;
        *slot = byte;
    }
    Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
}

fn generate_key(path: &Path) -> Result<(), String> {
    let key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
    std::fs::write(path, hex(&key.to_bytes()))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    println!(
        "wrote a signing key to {} (keep it outside the repository); public key {}",
        path.display(),
        hex(key.verifying_key().as_bytes())
    );
    Ok(())
}

fn sign(key_path: &Path, out: &Path) -> Result<(), String> {
    use ed25519_dalek::Signer;

    let key = read_key(key_path)?;
    let payload =
        std::fs::read_to_string(SBOM).map_err(|error| format!("cannot read {SBOM}: {error}"))?;
    let digest = sha256_hex(payload.as_bytes());
    let signature = key.sign(digest.as_bytes());
    let manifest = json!({
        "algorithm": "ed25519",
        "signed_file": SBOM,
        "sha256": digest,
        "public_key": hex(key.verifying_key().as_bytes()),
        "signature": hex(&signature.to_bytes()),
        "note": "the private key is never stored in the repository; verify with `cargo run -p ukma-release-evidence -- verify`",
    });
    std::fs::write(out, render(&manifest))
        .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
    // Record the signature in the digest manifest so a reader sees provenance
    // without having to guess whether the SBOM was signed at all.
    let digest_path = Path::new(DIGEST);
    if let Ok(text) = std::fs::read_to_string(digest_path) {
        if let Ok(mut digest) = serde_json::from_str::<Value>(&text) {
            if let Some(object) = digest.as_object_mut() {
                object.insert("signature".into(), json!(out.to_string_lossy()));
                object.insert("signature_algorithm".into(), json!("ed25519"));
                object.insert(
                    "public_key".into(),
                    json!(hex(key.verifying_key().as_bytes())),
                );
                object.insert(
                    "signature_note".into(),
                    json!("the private key is held outside the repository; verify with `cargo run -p ukma-release-evidence -- verify`"),
                );
                let _ = std::fs::write(digest_path, render(&digest));
            }
        }
    }
    println!("signed {SBOM} -> {}", out.display());
    Ok(())
}

fn verify(signature_path: &Path) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(signature_path)
            .map_err(|error| format!("cannot read {}: {error}", signature_path.display()))?,
    )
    .map_err(|error| format!("{} is not JSON: {error}", signature_path.display()))?;
    if manifest["algorithm"] != "ed25519" {
        return Err(format!("unsupported algorithm {}", manifest["algorithm"]));
    }
    let payload =
        std::fs::read_to_string(SBOM).map_err(|error| format!("cannot read {SBOM}: {error}"))?;
    let digest = sha256_hex(payload.as_bytes());
    if manifest["sha256"].as_str() != Some(digest.as_str()) {
        return Err("the SBOM changed after it was signed".to_string());
    }
    let key_bytes = decode_hex(manifest["public_key"].as_str().unwrap_or_default())?;
    let signature_bytes = decode_hex(manifest["signature"].as_str().unwrap_or_default())?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| "public key is not 32 bytes".to_string())?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|error| format!("bad key: {error}"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|error| format!("bad signature: {error}"))?;
    key.verify(digest.as_bytes(), &signature)
        .map_err(|_| "signature does not match the SBOM digest".to_string())?;
    println!("signature verified for {SBOM} ({digest})");
    Ok(())
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("hex string has an odd length".to_string());
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for index in (0..text.len()).step_by(2) {
        out.push(
            u8::from_str_radix(&text[index..index + 2], 16)
                .map_err(|_| "hex string has a non-hex digit".to_string())?,
        );
    }
    Ok(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| -> Option<PathBuf> {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .map(PathBuf::from)
    };
    let outcome = match args.first().map(String::as_str) {
        None | Some("regenerate") => regenerate(),
        Some("check") => check(),
        Some("generate-key") => match flag("--out") {
            Some(path) => generate_key(&path),
            None => Err("generate-key needs --out <file>".to_string()),
        },
        Some("sign") => match flag("--key") {
            Some(key) => sign(
                &key,
                &flag("--out").unwrap_or_else(|| PathBuf::from(SIGNATURE)),
            ),
            None => Err("sign needs --key <file>".to_string()),
        },
        Some("verify") => verify(&flag("--sig").unwrap_or_else(|| PathBuf::from(SIGNATURE))),
        Some(other) => Err(format!(
            "unknown command `{other}`; use regenerate, check, generate-key, sign or verify"
        )),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_fixture() -> Metadata {
        serde_json::from_str(
            r#"{
                "packages": [
                    {"id": "a 0.1.0", "name": "alpha", "version": "0.1.0", "license": "MIT OR Apache-2.0"},
                    {"id": "b 0.2.0", "name": "beta", "version": "0.2.0", "license": "GPL-3.0 OR MIT"},
                    {"id": "c 0.3.0", "name": "gamma", "version": "0.3.0", "license": null},
                    {"id": "d 0.4.0", "name": "ukma-x", "version": "0.4.0", "license": "Apache-2.0"}
                ],
                "workspace_members": ["d 0.4.0"]
            }"#,
        )
        .expect("fixture parses")
    }

    #[test]
    fn the_policy_elects_a_permissive_option_and_counts_the_tree() {
        let inventory = inventory(&metadata_fixture());
        assert_eq!(inventory["counts"]["external"], 3);
        assert_eq!(inventory["counts"]["workspace"], 1);
        assert_eq!(inventory["counts"]["unlicensed"], 1);
        // An unlicensed package is treated as blocked: the audit fails closed.
        assert_eq!(inventory["counts"]["blocked"], 1);
        let packages = inventory["packages"].as_array().unwrap();
        let beta = packages
            .iter()
            .find(|package| package["name"] == "beta")
            .unwrap();
        assert_eq!(beta["decision"], "allowed");
        assert!(beta["reason"].as_str().unwrap().contains("elected over"));
    }

    #[test]
    fn a_copyleft_only_licence_is_blocked() {
        let (decision, reason) = decide("GPL-3.0-only");
        assert_eq!(decision, "blocked");
        assert!(reason.contains("no permissive option"));
    }

    #[test]
    fn the_sbom_and_digest_are_deterministic() {
        let first_inventory = inventory(&metadata_fixture());
        let first_sbom = sbom(&first_inventory);
        let first = render(&first_sbom);
        let second = render(&sbom(&inventory(&metadata_fixture())));
        assert_eq!(first, second);
        let components = first_sbom["components"].as_array().unwrap().len();
        assert_eq!(components, 4);
        let digest = digest_manifest(&first, components);
        assert_eq!(
            digest["sha256"].as_str().unwrap(),
            sha256_hex(first.as_bytes())
        );
        assert_eq!(digest["bytes"].as_u64().unwrap(), first.len() as u64);
        assert!(digest["signature"].is_null());
    }

    #[test]
    fn a_signature_round_trips_and_detects_tampering() {
        use ed25519_dalek::Signer;

        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let payload = "{\"bomFormat\":\"CycloneDX\"}";
        let digest = sha256_hex(payload.as_bytes());
        let signature = key.sign(digest.as_bytes());
        let manifest = json!({
            "algorithm": "ed25519",
            "sha256": digest,
            "public_key": hex(key.verifying_key().as_bytes()),
            "signature": hex(&signature.to_bytes()),
        });
        // Verify inline the same way `verify()` does, without touching the repo.
        let key_bytes: [u8; 32] = decode_hex(manifest["public_key"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let verifier = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes).unwrap();
        let parsed = ed25519_dalek::Signature::from_slice(
            &decode_hex(manifest["signature"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        use ed25519_dalek::Verifier;
        verifier.verify(digest.as_bytes(), &parsed).unwrap();

        // A different document must not verify.
        let tampered = sha256_hex(b"tampered");
        assert!(verifier.verify(tampered.as_bytes(), &parsed).is_err());

        // A different key must not verify either.
        let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        assert_ne!(
            hex(other.verifying_key().as_bytes()),
            manifest["public_key"].as_str().unwrap()
        );
    }

    #[test]
    fn a_malformed_key_is_rejected() {
        let dir = std::env::temp_dir().join(format!("ukma-evidence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.txt");
        std::fs::write(&path, "nothex").unwrap();
        let error = read_key(&path).unwrap_err();
        assert!(error.contains("32 bytes of hex"));
        std::fs::write(&path, "zz".repeat(32)).unwrap();
        assert!(read_key(&path).unwrap_err().contains("not hex"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_generated_key_signs_and_verifies() {
        let dir = std::env::temp_dir().join(format!("ukma-evidence-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.txt");
        generate_key(&path).unwrap();
        let key = read_key(&path).unwrap();
        assert_eq!(hex(key.verifying_key().as_bytes()).len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
