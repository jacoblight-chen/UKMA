//! `ukma upgrade`: check for, and install, a newer `ukma` binary.
//!
//! Two paths, both explicit:
//! - **offline** — `--from <file>` installs a binary (or a release asset) that
//!   is already on disk, optionally verified against `--sha256`;
//! - **network** — a release manifest is fetched over HTTPS from
//!   [`DEFAULT_MANIFEST_URL`] or `--manifest`, the asset for this platform is
//!   downloaded, its SHA-256 is verified, and the binary is replaced.
//!
//! The fetch is in-process (`ureq`, the workspace's existing HTTP client), so
//! `ukma upgrade` does not depend on `curl` being installed.
//!
//! The replacement is a two-step rename, so a failed upgrade leaves the
//! previous binary in place. On Windows a running executable can be renamed but
//! not deleted, so a stale `ukma.exe.old` may survive one run.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{io_error, SetupContext, SetupError};

/// The release manifest fetched when no `--manifest` is given.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/jacoblight-chen/UKMA/releases/latest/download/latest.json";

/// A request to upgrade (or only to check).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpgradeRequest {
    /// Only report whether a newer release exists.
    pub check: bool,
    /// Install this local file instead of downloading anything.
    pub from: Option<PathBuf>,
    /// The install directory (default: the directory of the running binary).
    pub dir: Option<PathBuf>,
    /// The manifest path or URL (default: [`DEFAULT_MANIFEST_URL`]).
    pub manifest: Option<String>,
    /// The expected SHA-256 of `--from`.
    pub sha256: Option<String>,
    /// Compute the report without replacing the binary.
    pub dry_run: bool,
}

/// The release manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeManifest {
    /// The released version (`1.2.3`, `v1.2.3` also parses).
    pub version: String,
    /// One asset per target triple.
    #[serde(default)]
    pub assets: BTreeMap<String, UpgradeAsset>,
}

/// One downloadable asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeAsset {
    /// The download URL.
    pub url: String,
    /// The expected SHA-256 (empty when the manifest does not pin one).
    #[serde(default)]
    pub sha256: String,
}

/// The outcome of `ukma upgrade`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeReport {
    /// The running version.
    pub current: String,
    /// The version offered by the manifest, when one was read.
    pub available: Option<String>,
    /// Whether the binary was replaced.
    pub applied: bool,
    /// The binary path.
    pub target: PathBuf,
    /// A short human note.
    pub detail: String,
}

impl UpgradeReport {
    /// The rendered report.
    pub fn text(&self) -> String {
        let available = self
            .available
            .as_deref()
            .map(|version| format!(", available {version}"))
            .unwrap_or_default();
        format!(
            "ukma upgrade: current {}{available}\n{} {}",
            self.current,
            if self.applied { "installed" } else { "target" },
            self.target.display()
        ) + &format!("\n{}", self.detail)
    }
}

/// Check for, and optionally install, a newer binary.
pub fn run(ctx: &SetupContext, request: &UpgradeRequest) -> Result<UpgradeReport, SetupError> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let dir = request
        .dir
        .clone()
        .or_else(|| ctx.binary.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| ctx.default_install_dir());
    let target = dir.join(SetupContext::binary_name());

    if let Some(from) = &request.from {
        let bytes = fs::read(from).map_err(|error| io_error("read", from, &error))?;
        if let Some(expected) = &request.sha256 {
            verify_sha256(&bytes, expected, from)?;
        }
        if request.dry_run {
            return Ok(UpgradeReport {
                current,
                available: None,
                applied: false,
                target,
                detail: format!("would install {}", from.display()),
            });
        }
        replace_binary(&target, &bytes)?;
        return Ok(UpgradeReport {
            current,
            available: None,
            applied: true,
            target,
            detail: format!("installed from {}", from.display()),
        });
    }

    let source = request
        .manifest
        .clone()
        .unwrap_or_else(|| DEFAULT_MANIFEST_URL.to_string());
    let manifest = fetch_manifest(&source)?;
    let newer = is_newer(&manifest.version, &current)?;
    if request.check {
        return Ok(UpgradeReport {
            current,
            available: Some(manifest.version),
            applied: false,
            target,
            detail: if newer {
                "an update is available; re-run `ukma upgrade` to install it".into()
            } else {
                "already up to date".into()
            },
        });
    }
    if !newer {
        return Ok(UpgradeReport {
            current,
            available: Some(manifest.version),
            applied: false,
            target,
            detail: "already up to date".into(),
        });
    }

    let triple = target_triple()?;
    let asset = manifest.assets.get(triple).ok_or_else(|| {
        SetupError::Unsupported(format!(
            "release {} has no asset for `{triple}`",
            manifest.version
        ))
    })?;
    let staging = std::env::temp_dir().join(format!("ukma-upgrade-{}.bin", std::process::id()));
    fetch_to(&asset.url, &staging)?;
    let bytes = fs::read(&staging).map_err(|error| io_error("read", &staging, &error))?;
    let _ = fs::remove_file(&staging);
    if !asset.sha256.is_empty() {
        verify_sha256(&bytes, &asset.sha256, &target)?;
    }
    if request.dry_run {
        return Ok(UpgradeReport {
            current,
            available: Some(manifest.version),
            applied: false,
            target,
            detail: format!("would install {} for `{triple}`", asset.url),
        });
    }
    replace_binary(&target, &bytes)?;
    Ok(UpgradeReport {
        current,
        available: Some(manifest.version.clone()),
        applied: true,
        target,
        detail: format!("installed {} for `{triple}`", manifest.version),
    })
}

/// The release target triple for the running platform.
pub fn target_triple() -> Result<&'static str, SetupError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        (os, arch) => Err(SetupError::Unsupported(format!(
            "no release asset is defined for {os}/{arch}; use `ukma upgrade --from <file>`"
        ))),
    }
}

/// Parse a dotted version (`v1.2.3`, `1.2`, `1.2.3-rc1`).
pub fn parse_version(text: &str) -> Result<Vec<u64>, SetupError> {
    let trimmed = text.trim();
    let core = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let core = core
        .split(['-', '+'])
        .next()
        .filter(|core| !core.is_empty())
        .ok_or_else(|| SetupError::Unsupported(format!("`{text}` is not a version")))?;
    core.split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| SetupError::Unsupported(format!("`{text}` is not a version")))
        })
        .collect()
}

/// Whether `available` is a newer version than `current`.
pub fn is_newer(available: &str, current: &str) -> Result<bool, SetupError> {
    let available = parse_version(available)?;
    let current = parse_version(current)?;
    let len = available.len().max(current.len());
    for index in 0..len {
        let left = available.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return Ok(left > right);
        }
    }
    Ok(false)
}

fn fetch_manifest(source: &str) -> Result<UpgradeManifest, SetupError> {
    let path = PathBuf::from(source);
    let text = if source.starts_with("https://") || source.starts_with("http://") {
        String::from_utf8(fetch(source)?)
            .map_err(|error| SetupError::Network(format!("{source} is not UTF-8 ({error})")))?
    } else {
        fs::read_to_string(&path).map_err(|error| io_error("read", &path, &error))?
    };
    serde_json::from_str(&text).map_err(|error| SetupError::Config {
        path,
        detail: format!("invalid release manifest: {error}"),
    })
}

/// Download `url` into memory.
///
/// In-process on purpose: the previous implementation shelled out to `curl`,
/// which made `ukma upgrade` fail on any host without it.
fn fetch(url: &str) -> Result<Vec<u8>, SetupError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(concat!("ukma-cli/", env!("CARGO_PKG_VERSION")))
        .build();
    match agent.get(url).call() {
        Ok(response) => {
            let mut bytes = Vec::new();
            response
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|error| SetupError::Network(format!("reading {url} failed: {error}")))?;
            Ok(bytes)
        }
        Err(ureq::Error::Status(status, _)) => Err(SetupError::Network(format!(
            "downloading {url} failed: HTTP {status}"
        ))),
        Err(ureq::Error::Transport(transport)) => Err(SetupError::Network(format!(
            "downloading {url} failed: {transport}"
        ))),
    }
}

fn fetch_to(url: &str, dest: &Path) -> Result<(), SetupError> {
    let bytes = fetch(url)?;
    fs::write(dest, &bytes).map_err(|error| io_error("write", dest, &error))
}

fn verify_sha256(bytes: &[u8], expected: &str, path: &Path) -> Result<(), SetupError> {
    let expected = expected.trim().to_ascii_lowercase();
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Err(SetupError::Integrity(format!(
            "{} failed its SHA-256 check: expected {expected}, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

fn replace_binary(target: &Path, bytes: &[u8]) -> Result<(), SetupError> {
    if let Some(parent) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| SetupContext::binary_name().to_string());
    let staging = target.with_file_name(format!("{name}.new"));
    let backup = target.with_file_name(format!("{name}.old"));
    fs::write(&staging, bytes).map_err(|error| io_error("write", &staging, &error))?;
    let _ = fs::remove_file(&backup);
    if target.exists() {
        fs::rename(target, &backup).map_err(|error| io_error("replace", target, &error))?;
    }
    if let Err(error) = fs::rename(&staging, target) {
        let _ = fs::rename(&backup, target);
        let _ = fs::remove_file(&staging);
        return Err(io_error("install", target, &error));
    }
    let _ = fs::remove_file(&backup);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o755))
            .map_err(|error| io_error("mark executable", target, &error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};

    fn context(dir: &Path) -> SetupContext {
        SetupContext {
            home: dir.join("home"),
            cwd: dir.join("project"),
            binary: dir.join("bin").join(SetupContext::binary_name()),
        }
    }

    #[test]
    fn versions_compare_numerically_not_lexically() {
        assert!(is_newer("0.10.0", "0.9.0").expect("valid versions"));
        assert!(!is_newer("0.9.0", "0.10.0").expect("valid versions"));
        assert!(!is_newer("1.2.3", "1.2.3").expect("valid versions"));
        assert!(is_newer("v1.2.4", "1.2.3").expect("a `v` prefix parses"));
        assert!(is_newer("2.0.0", "1.99.99").expect("valid versions"));
        assert!(!is_newer("1.2", "1.2.0").expect("a missing component is zero"));
        assert!(is_newer("1.3", "1.2.9").expect("valid versions"));
        assert!(is_newer("1.2.3-rc1", "1.2.2").expect("a pre-release suffix is ignored"));
    }

    #[test]
    fn a_non_numeric_version_is_rejected() {
        let error = parse_version("nightly").expect_err("not a version");
        assert!(matches!(error, SetupError::Unsupported(_)));
        assert!(parse_version("").is_err());
    }

    #[test]
    fn the_target_triple_is_known_for_this_platform() {
        let triple = target_triple().expect("the test platform has a release asset name");
        assert!(triple.contains('-'));
    }

    #[test]
    fn an_offline_upgrade_replaces_the_binary_and_keeps_no_staging_files() {
        let base = std::env::temp_dir().join(format!("ukma-upgrade-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("bin")).expect("temp dir");
        let ctx = context(&base);
        let target = base.join("bin").join(SetupContext::binary_name());
        fs::write(&target, b"old-binary").expect("seed");

        let source = base.join("new-binary");
        fs::write(&source, b"new-binary").expect("seed");

        let report = run(
            &ctx,
            &UpgradeRequest {
                from: Some(source),
                ..UpgradeRequest::default()
            },
        )
        .expect("the upgrade applies");
        assert!(report.applied);
        assert_eq!(fs::read(&target).expect("read"), b"new-binary");
        let name = SetupContext::binary_name();
        assert!(!base.join("bin").join(format!("{name}.old")).exists());
        assert!(!base.join("bin").join(format!("{name}.new")).exists());

        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn an_offline_upgrade_verifies_the_checksum() {
        let base = std::env::temp_dir().join(format!("ukma-upgrade-sha-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temp dir");
        let ctx = context(&base);
        let source = base.join("new-binary");
        fs::write(&source, b"new-binary").expect("seed");

        let error = run(
            &ctx,
            &UpgradeRequest {
                from: Some(source.clone()),
                sha256: Some("00".repeat(32)),
                ..UpgradeRequest::default()
            },
        )
        .expect_err("a wrong checksum is rejected");
        assert!(matches!(error, SetupError::Integrity(_)));

        let correct: String = Sha256::digest(b"new-binary")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let report = run(
            &ctx,
            &UpgradeRequest {
                from: Some(source),
                sha256: Some(correct),
                ..UpgradeRequest::default()
            },
        )
        .expect("the matching checksum is accepted");
        assert!(report.applied);

        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn a_dry_run_does_not_touch_the_target() {
        let base = std::env::temp_dir().join(format!("ukma-upgrade-dry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("bin")).expect("temp dir");
        let ctx = context(&base);
        let target = base.join("bin").join(SetupContext::binary_name());
        fs::write(&target, b"old-binary").expect("seed");
        let source = base.join("new-binary");
        fs::write(&source, b"new-binary").expect("seed");

        let report = run(
            &ctx,
            &UpgradeRequest {
                from: Some(source),
                dry_run: true,
                ..UpgradeRequest::default()
            },
        )
        .expect("the dry run reports");
        assert!(!report.applied);
        assert_eq!(fs::read(&target).expect("read"), b"old-binary");

        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn a_local_manifest_reports_an_available_update() {
        let base =
            std::env::temp_dir().join(format!("ukma-upgrade-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temp dir");
        let ctx = context(&base);
        let manifest = base.join("latest.json");
        fs::write(
            &manifest,
            r#"{"version":"99.0.0","assets":{"x86_64-pc-windows-msvc":{"url":"https://example.invalid/a","sha256":""}}}"#,
        )
        .expect("seed");

        let report = run(
            &ctx,
            &UpgradeRequest {
                check: true,
                manifest: Some(manifest.display().to_string()),
                ..UpgradeRequest::default()
            },
        )
        .expect("the check reads the manifest");
        assert!(!report.applied);
        assert_eq!(report.available.as_deref(), Some("99.0.0"));
        assert!(report.text().contains("an update is available"));

        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn an_older_manifest_is_reported_as_up_to_date() {
        let base = std::env::temp_dir().join(format!("ukma-upgrade-old-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("temp dir");
        let ctx = context(&base);
        let manifest = base.join("latest.json");
        fs::write(&manifest, r#"{"version":"0.0.1","assets":{}}"#).expect("seed");

        let report = run(
            &ctx,
            &UpgradeRequest {
                manifest: Some(manifest.display().to_string()),
                ..UpgradeRequest::default()
            },
        )
        .expect("the manifest is read");
        assert!(!report.applied);
        assert!(report.detail.contains("up to date"));

        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn a_missing_manifest_is_a_read_error() {
        let base =
            std::env::temp_dir().join(format!("ukma-upgrade-missing-{}", std::process::id()));
        let ctx = context(&base);
        let error = run(
            &ctx,
            &UpgradeRequest {
                manifest: Some(base.join("nope.json").display().to_string()),
                ..UpgradeRequest::default()
            },
        )
        .expect_err("a missing manifest fails");
        assert!(matches!(error, SetupError::Io { .. }));
    }

    /// Serve `(path, body)` pairs over HTTP, one request each, on a loopback
    /// port. The bodies are built after binding so they can name the port.
    fn http_server(
        bodies: impl FnOnce(u16) -> Vec<(String, Vec<u8>)>,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let responses = bodies(port);
        let handle = std::thread::spawn(move || {
            for _ in 0..responses.len() {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                }
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let (status, body) = match responses.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => ("200 OK", body.clone()),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (port, handle)
    }

    #[test]
    fn a_network_upgrade_downloads_the_manifest_and_the_asset_without_curl() {
        let base = std::env::temp_dir().join(format!("ukma-upgrade-net-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("bin")).expect("temp dir");
        let ctx = context(&base);
        let target = base.join("bin").join(SetupContext::binary_name());
        fs::write(&target, b"old-binary").expect("seed");

        let asset = b"new-binary-from-the-network".to_vec();
        let digest: String = Sha256::digest(&asset)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let (port, server) = http_server(|port| {
            let manifest = serde_json::json!({
                "version": "99.0.0",
                "assets": {
                    target_triple().expect("triple"): {
                        "url": format!("http://127.0.0.1:{port}/ukma-asset"),
                        "sha256": digest,
                    }
                }
            })
            .to_string();
            vec![
                ("/latest.json".to_string(), manifest.into_bytes()),
                ("/ukma-asset".to_string(), asset.clone()),
            ]
        });

        let report = run(
            &ctx,
            &UpgradeRequest {
                manifest: Some(format!("http://127.0.0.1:{port}/latest.json")),
                ..UpgradeRequest::default()
            },
        )
        .expect("the upgrade downloads and applies");
        assert!(report.applied, "{}", report.detail);
        assert_eq!(report.available.as_deref(), Some("99.0.0"));
        assert_eq!(fs::read(&target).expect("read"), asset);
        let _ = server.join();

        fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn an_unreachable_manifest_is_a_network_error_not_a_missing_curl() {
        let base =
            std::env::temp_dir().join(format!("ukma-upgrade-netdown-{}", std::process::id()));
        let ctx = context(&base);
        // Port 1 on loopback refuses connections immediately on every platform.
        let error = run(
            &ctx,
            &UpgradeRequest {
                check: true,
                manifest: Some("http://127.0.0.1:1/latest.json".to_string()),
                ..UpgradeRequest::default()
            },
        )
        .expect_err("an unreachable manifest fails");
        let text = error.to_string();
        assert!(matches!(error, SetupError::Network(_)), "{text}");
        assert!(!text.contains("curl"), "{text}");
    }
}
