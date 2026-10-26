//! The `npx` launcher is a second, non-Rust entry point, so it has to be kept
//! honest: it must stay a thin launcher, name the same release assets the Rust
//! code names, and still parse as JavaScript.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // No `canonicalize`: on Windows it returns a `\\?\` UNC path, which the
    // Node loader refuses.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .to_path_buf()
}

fn npm_dir() -> PathBuf {
    repo_root().join("npm")
}

fn shim() -> String {
    let path = npm_dir().join("bin").join("ukma.js");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn the_package_points_at_an_existing_launcher() {
    let manifest = npm_dir().join("package.json");
    let text = std::fs::read_to_string(&manifest).expect("package.json");
    let json: serde_json::Value = serde_json::from_str(&text).expect("package.json is JSON");
    let bin = json["bin"]["ukma"]
        .as_str()
        .expect("the package declares a `ukma` bin");
    assert_eq!(bin, "bin/ukma.js");
    assert!(
        npm_dir().join(bin).is_file(),
        "{bin} is missing from the package"
    );
    assert_eq!(json["name"], "ukma-cli");
    assert!(
        json["files"]
            .as_array()
            .expect("a files list")
            .iter()
            .any(|entry| entry == "bin/ukma.js"),
        "the launcher must be published"
    );
}

#[test]
fn the_launcher_stays_a_thin_forwarder() {
    let text = shim();
    // It execs the binary with every argument forwarded, and defaults to
    // `install` when given none (CodeGraph's `npx <pkg>` behaviour).
    assert!(text.contains("spawnSync(binary, forwarded, { stdio: 'inherit' })"));
    assert!(text.contains("args.length === 0 ? ['install'] : args"));
    // No code-graph logic: the shim may not build envelopes or touch the index.
    for forbidden in ["tool-manifest", "RequestEnvelope", "index.db", "codemap"] {
        assert!(
            !text.contains(forbidden),
            "the launcher must not implement `{forbidden}`"
        );
    }
}

#[test]
fn the_launcher_names_the_same_release_triples_as_the_rust_code() {
    let text = shim();
    // These are exactly `ukma_cli::setup::upgrade::target_triple`'s triples.
    for triple in [
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        assert!(text.contains(triple), "the launcher omits `{triple}`");
    }
    // And the asset naming has to match what `ukma upgrade` reads.
    assert!(text.contains("releases/download/v${version}"));
    assert!(text.contains("ukma-${triple}"));
}

#[test]
fn the_launcher_is_valid_javascript_and_reports_its_usage() {
    // `node --check` is the cheapest real syntax gate; skip when Node is absent
    // (the Rust toolchain does not require Node).
    let check = Command::new("node")
        .arg("--check")
        .arg(npm_dir().join("bin").join("ukma.js"))
        .output();
    let Ok(output) = check else {
        eprintln!("node is not installed; skipping the syntax check");
        return;
    };
    assert!(
        output.status.success(),
        "node --check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_launcher_runs_the_binary_it_is_pointed_at() {
    // `UKMA_BIN` makes the launcher exec a given executable instead of
    // downloading one, which is what makes it testable without a release.
    let Some(node) = which_node() else {
        eprintln!("node is not installed; skipping the launcher run");
        return;
    };
    // The real CLI binary, so this proves forwarding, not a stub's echo.
    let binary = Path::new(env!("CARGO_BIN_EXE_ukma"));

    let direct = Command::new(binary)
        .arg("--version")
        .output()
        .expect("run ukma");
    let forwarded = Command::new(&node)
        .arg(npm_dir().join("bin").join("ukma.js"))
        .arg("--version")
        .env("UKMA_BIN", binary)
        .output()
        .expect("run the launcher");
    assert_eq!(
        String::from_utf8_lossy(&forwarded.stdout),
        String::from_utf8_lossy(&direct.stdout),
        "the launcher returns the binary's stdout unchanged"
    );

    // Exit codes are propagated too: an unknown command is a usage error.
    let direct_error = Command::new(binary).arg("nope").output().expect("run ukma");
    let forwarded_error = Command::new(&node)
        .arg(npm_dir().join("bin").join("ukma.js"))
        .arg("nope")
        .env("UKMA_BIN", binary)
        .output()
        .expect("run the launcher");
    assert_eq!(
        forwarded_error.status.code(),
        direct_error.status.code(),
        "the child's exit code is propagated"
    );
    assert_ne!(direct_error.status.code(), Some(0), "the probe must fail");
}

fn which_node() -> Option<PathBuf> {
    let probe = if cfg!(windows) { "node.exe" } else { "node" };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(probe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
