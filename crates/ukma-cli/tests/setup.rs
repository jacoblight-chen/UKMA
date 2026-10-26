//! Filesystem-level tests for the setup surface (`install`, `uninstall`,
//! `init`, `uninit`).
//!
//! Every test points a [`SetupContext`] at its own scratch directory, so no
//! real agent configuration, home directory or project is touched, and the
//! tests are safe to run in parallel.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use ukma_cli::setup::cache;
use ukma_cli::setup::config;
use ukma_cli::setup::install::{self, Action, InstallRequest, UninstallRequest};
use ukma_cli::setup::project;
use ukma_cli::setup::snapshot::PendingKind;
use ukma_cli::setup::{SetupContext, SetupError};
use ukma_mcp::index::IndexOptions;

/// A scratch directory tree with a home, a project and a binary path.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("ukma-setup-{tag}-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).expect("scratch home");
        fs::create_dir_all(root.join("project")).expect("scratch project");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn binary(&self) -> PathBuf {
        self.root.join("bin").join(SetupContext::binary_name())
    }

    fn context(&self) -> SetupContext {
        SetupContext {
            home: self.home(),
            cwd: self.project(),
            binary: self.binary(),
        }
    }

    fn read(&self, path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|error| {
            panic!("cannot read {}: {error}", path.display());
        })
    }

    fn json(&self, path: &Path) -> Value {
        serde_json::from_str(&self.read(path)).unwrap_or_else(|error| {
            panic!("{} is not JSON: {error}", path.display());
        })
    }

    fn touch_dir(&self, relative: &str) {
        fs::create_dir_all(self.home().join(relative)).expect("scratch dir");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn targets(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| id.to_string()).collect()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| arg.to_string()).collect()
}

// ---------------------------------------------------------------- install ---

#[test]
fn install_wires_every_detected_agent_and_is_idempotent() {
    let scratch = Scratch::new("install-detected");
    scratch.touch_dir(".claude");
    scratch.touch_dir(".cursor");
    let ctx = scratch.context();

    let report = install::install(&ctx, &InstallRequest::default()).expect("install");
    assert_eq!(report.count(Action::Configured), 2, "{}", report.text());
    assert_eq!(report.count(Action::Skipped), 0);

    let claude = scratch.json(&scratch.home().join(".claude.json"));
    assert_eq!(
        claude["mcpServers"]["ukma"]["command"],
        json!(ctx.binary.display().to_string())
    );
    assert_eq!(
        claude["mcpServers"]["ukma"]["args"],
        json!(["--mcp", "--index-cwd", "."])
    );
    assert!(
        scratch.home().join(".claude/UKMA.md").is_file(),
        "the rules file is written"
    );
    let cursor = scratch.json(&scratch.home().join(".cursor/mcp.json"));
    assert_eq!(
        cursor["mcpServers"]["ukma"]["command"],
        json!(ctx.binary.display().to_string())
    );

    let again = install::install(&ctx, &InstallRequest::default()).expect("install twice");
    assert_eq!(again.count(Action::Unchanged), 2, "{}", again.text());
    assert_eq!(again.count(Action::Configured), 0);
}

#[test]
fn install_wires_hermes_into_its_yaml_config_and_uninstall_takes_it_out() {
    let scratch = Scratch::new("install-hermes");
    scratch.touch_dir(".hermes");
    let ctx = scratch.context();
    let config = scratch.home().join(".hermes/config.yaml");
    fs::write(
        &config,
        "model: hermes-3\nmcp_servers:\n  other:\n    command: other\n",
    )
    .expect("seed");

    let report = install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["hermes"]),
            ..InstallRequest::default()
        },
    )
    .expect("install");
    assert_eq!(report.count(Action::Updated), 1, "{}", report.text());
    let text = scratch.read(&config);
    assert!(text.starts_with("model: hermes-3\n"), "{text}");
    assert!(text.contains("  other:\n    command: other\n"), "{text}");
    assert!(text.contains("  ukma:\n"), "{text}");
    assert!(text.contains("      - '--mcp'\n"), "{text}");

    // Idempotent: a second run changes nothing.
    let again = install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["hermes"]),
            ..InstallRequest::default()
        },
    )
    .expect("install twice");
    assert_eq!(again.count(Action::Unchanged), 1, "{}", again.text());

    let removed = install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["hermes"]),
            yes: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");
    assert_eq!(removed.count(Action::Removed), 1, "{}", removed.text());
    assert_eq!(
        scratch.read(&config),
        "model: hermes-3\nmcp_servers:\n  other:\n    command: other\n"
    );
}

#[test]
fn install_wires_antigravity_and_uninstall_sweeps_both_config_paths() {
    let scratch = Scratch::new("install-antigravity");
    // A migrated Antigravity: the unified path is preferred, and an entry left
    // at the legacy path by an older install must still be swept.
    std::fs::create_dir_all(scratch.home().join(".gemini/config")).expect("config dir");
    fs::write(scratch.home().join(".gemini/config/.migrated"), b"").expect("marker");
    let unified = scratch.home().join(".gemini/config/mcp_config.json");
    let legacy = scratch.home().join(".gemini/antigravity/mcp_config.json");
    std::fs::create_dir_all(legacy.parent().expect("legacy dir")).expect("legacy");
    fs::write(
        &legacy,
        r#"{"mcpServers":{"ukma":{"command":"old-ukma","args":["--mcp"]},"keep":{"command":"k"}}}"#,
    )
    .expect("seed legacy");
    let ctx = scratch.context();

    let report = install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["antigravity"]),
            ..InstallRequest::default()
        },
    )
    .expect("install");
    assert_eq!(report.count(Action::Configured), 1, "{}", report.text());
    let entry = scratch.json(&unified);
    assert_eq!(
        entry["mcpServers"]["ukma"]["command"],
        json!(ctx.binary.display().to_string())
    );
    assert!(
        entry["mcpServers"]["ukma"].get("type").is_none(),
        "Antigravity rejects a `type` field: {entry}"
    );

    let removed = install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["antigravity"]),
            yes: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");
    assert_eq!(removed.count(Action::Removed), 1, "{}", removed.text());
    assert!(
        scratch.json(&unified)["mcpServers"].get("ukma").is_none(),
        "the preferred path is cleaned"
    );
    let legacy_after = scratch.json(&legacy);
    assert!(
        legacy_after["mcpServers"].get("ukma").is_none(),
        "the legacy path is swept too: {legacy_after}"
    );
    assert_eq!(
        legacy_after["mcpServers"]["keep"]["command"],
        json!("k"),
        "an unrelated server survives"
    );
}

#[test]
fn install_preserves_every_unrelated_key() {
    let scratch = Scratch::new("install-merge");
    scratch.touch_dir(".claude");
    let path = scratch.home().join(".claude.json");
    fs::write(
        &path,
        r#"{"numStartups":7,"mcpServers":{"other":{"command":"other-bin"}}}"#,
    )
    .expect("seed");

    let report = install::install(
        &scratch.context(),
        &InstallRequest {
            targets: targets(&["claude-code"]),
            ..InstallRequest::default()
        },
    )
    .expect("install");

    assert_eq!(report.count(Action::Updated), 1, "{}", report.text());
    let document = scratch.json(&path);
    assert_eq!(document["numStartups"], json!(7));
    assert_eq!(
        document["mcpServers"]["other"]["command"],
        json!("other-bin")
    );
    assert!(document["mcpServers"]["ukma"]["command"].is_string());
}

#[test]
fn install_reports_nothing_detected_and_rejects_unknown_targets() {
    let scratch = Scratch::new("install-empty");
    let ctx = scratch.context();

    let report = install::install(&ctx, &InstallRequest::default()).expect("install");
    assert!(report.actions.is_empty());
    assert!(report.text().contains("no agents selected"));

    let error = install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["nope"]),
            ..InstallRequest::default()
        },
    )
    .expect_err("an unknown target is an error");
    assert!(matches!(error, SetupError::UnknownTarget(_)));

    let unsupported = install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["jetbrains"]),
            ..InstallRequest::default()
        },
    )
    .expect("an unsupported agent is reported, not fatal");
    assert_eq!(unsupported.count(Action::Skipped), 1);
    assert!(unsupported.actions[0].detail.contains("REST client"));
}

#[test]
fn install_dry_run_writes_nothing() {
    let scratch = Scratch::new("install-dry");
    scratch.touch_dir(".claude");
    let ctx = scratch.context();

    let report = install::install(
        &ctx,
        &InstallRequest {
            dry_run: true,
            ..InstallRequest::default()
        },
    )
    .expect("dry run");

    assert_eq!(report.count(Action::Planned), 1, "{}", report.text());
    assert!(!scratch.home().join(".claude.json").exists());
    assert!(!scratch.home().join(".claude/UKMA.md").exists());
}

#[test]
fn install_can_skip_the_rules_file_and_customise_the_index_root() {
    let scratch = Scratch::new("install-rules");
    scratch.touch_dir(".claude");
    let ctx = scratch.context();

    install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["claude-code"]),
            rules: false,
            index_root: "crates".into(),
            ..InstallRequest::default()
        },
    )
    .expect("install");

    assert!(!scratch.home().join(".claude/UKMA.md").exists());
    let document = scratch.json(&scratch.home().join(".claude.json"));
    assert_eq!(
        document["mcpServers"]["ukma"]["args"],
        json!(["--mcp", "--index-cwd", "crates"])
    );
}

#[test]
fn install_writes_one_managed_toml_block_for_codex() {
    let scratch = Scratch::new("install-codex");
    scratch.touch_dir(".codex");
    let path = scratch.home().join(".codex/config.toml");
    fs::write(&path, "model = 'gpt-5'\n").expect("seed");
    let ctx = scratch.context();

    install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["codex"]),
            ..InstallRequest::default()
        },
    )
    .expect("install");
    let first = scratch.read(&path);
    assert!(first.contains("model = 'gpt-5'"), "the existing keys stay");
    assert!(first.contains("[mcp_servers.ukma]"));
    assert!(first.contains(&ctx.binary.display().to_string()));
    assert_eq!(first.matches(config::TOML_BEGIN).count(), 1);

    let again = install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["codex"]),
            ..InstallRequest::default()
        },
    )
    .expect("install twice");
    assert_eq!(again.count(Action::Unchanged), 1, "{}", again.text());
    assert_eq!(scratch.read(&path), first, "the file is byte-identical");
}

#[test]
fn install_writes_the_project_scoped_copilot_configuration() {
    let scratch = Scratch::new("install-copilot");
    let ctx = scratch.context();

    install::install(
        &ctx,
        &InstallRequest {
            targets: targets(&["copilot-vscode"]),
            ..InstallRequest::default()
        },
    )
    .expect("install");

    let document = scratch.json(&scratch.project().join(".vscode/mcp.json"));
    assert_eq!(document["servers"]["ukma"]["type"], json!("stdio"));
    assert!(document["servers"]["ukma"]["command"].is_string());

    let rules = scratch.read(
        &scratch
            .project()
            .join(".github/instructions/ukma.instructions.md"),
    );
    assert!(
        rules.starts_with("---\napplyTo:"),
        "VS Code needs frontmatter"
    );
    assert!(rules.contains("ukma_code_get_symbol"));
}

#[test]
fn install_refuses_a_configuration_that_is_not_an_object_container() {
    let scratch = Scratch::new("install-bad-config");
    scratch.touch_dir(".claude");
    fs::write(
        scratch.home().join(".claude.json"),
        r#"{"mcpServers":"nope"}"#,
    )
    .expect("seed");

    let error = install::install(
        &scratch.context(),
        &InstallRequest {
            targets: targets(&["claude-code"]),
            ..InstallRequest::default()
        },
    )
    .expect_err("a container of the wrong type is refused");
    assert!(matches!(error, SetupError::Config { .. }));
}

#[test]
fn install_refuses_an_unparsable_configuration() {
    let scratch = Scratch::new("install-broken-json");
    scratch.touch_dir(".claude");
    fs::write(scratch.home().join(".claude.json"), "{not json").expect("seed");

    let error = install::install(
        &scratch.context(),
        &InstallRequest {
            targets: targets(&["claude-code"]),
            ..InstallRequest::default()
        },
    )
    .expect_err("invalid JSON is refused");
    assert!(matches!(error, SetupError::Config { .. }));
}

// -------------------------------------------------------------- uninstall ---

#[test]
fn uninstall_requires_confirmation() {
    let scratch = Scratch::new("uninstall-guard");
    scratch.touch_dir(".claude");
    let error = install::uninstall(&scratch.context(), &UninstallRequest::default())
        .expect_err("an unconfirmed uninstall is refused");
    match error {
        SetupError::ConfirmationRequired(detail) => assert!(detail.contains("--yes")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn uninstall_removes_only_the_ukma_wiring() {
    let scratch = Scratch::new("uninstall-roundtrip");
    scratch.touch_dir(".claude");
    let path = scratch.home().join(".claude.json");
    fs::write(
        &path,
        r#"{"numStartups":7,"mcpServers":{"other":{"command":"other-bin"}}}"#,
    )
    .expect("seed");
    let ctx = scratch.context();
    let install_request = InstallRequest {
        targets: targets(&["claude-code"]),
        ..InstallRequest::default()
    };
    install::install(&ctx, &install_request).expect("install");

    let report = install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["claude-code"]),
            yes: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");
    assert_eq!(report.count(Action::Removed), 1, "{}", report.text());

    let document = scratch.json(&path);
    assert_eq!(document["numStartups"], json!(7));
    assert_eq!(
        document["mcpServers"]["other"]["command"],
        json!("other-bin")
    );
    assert!(
        document["mcpServers"].get("ukma").is_none(),
        "only the ukma entry is removed"
    );
    assert!(!scratch.home().join(".claude/UKMA.md").exists());

    let again = install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["claude-code"]),
            yes: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall twice");
    assert_eq!(again.count(Action::Unchanged), 1, "{}", again.text());
}

#[test]
fn uninstall_drops_the_container_when_ukma_was_its_only_entry() {
    let scratch = Scratch::new("uninstall-container");
    scratch.touch_dir(".claude");
    let ctx = scratch.context();
    let request = InstallRequest {
        targets: targets(&["claude-code"]),
        ..InstallRequest::default()
    };
    install::install(&ctx, &request).expect("install");
    install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["claude-code"]),
            yes: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");

    let document = scratch.json(&scratch.home().join(".claude.json"));
    assert!(
        document.get("mcpServers").is_none(),
        "an emptied container is removed: {document}"
    );
}

#[test]
fn uninstall_never_deletes_a_user_modified_rules_file() {
    let scratch = Scratch::new("uninstall-rules");
    scratch.touch_dir(".claude");
    let ctx = scratch.context();
    let request = InstallRequest {
        targets: targets(&["claude-code"]),
        ..InstallRequest::default()
    };
    install::install(&ctx, &request).expect("install");
    let rules = scratch.home().join(".claude/UKMA.md");
    let mut text = scratch.read(&rules);
    text.push_str("\nmy own note\n");
    fs::write(&rules, &text).expect("edit");

    let report = install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["claude-code"]),
            yes: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");
    assert!(rules.is_file(), "the edited rules file is kept");
    assert!(
        report.actions[0].detail.contains("modified"),
        "{}",
        report.actions[0].detail
    );
}

#[test]
fn uninstall_can_remove_the_cli_and_keeps_it_when_asked() {
    let scratch = Scratch::new("uninstall-cli");
    scratch.touch_dir(".claude");
    let ctx = scratch.context();
    fs::create_dir_all(ctx.binary.parent().expect("parent")).expect("bin dir");
    fs::write(&ctx.binary, b"binary").expect("seed binary");

    install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["claude-code"]),
            yes: true,
            keep_cli: true,
            remove_cli: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");
    assert!(ctx.binary.is_file(), "--keep-cli wins over --remove-cli");

    let report = install::uninstall(
        &ctx,
        &UninstallRequest {
            targets: targets(&["claude-code"]),
            yes: true,
            remove_cli: true,
            ..UninstallRequest::default()
        },
    )
    .expect("uninstall");
    assert!(!ctx.binary.exists(), "--remove-cli deletes the binary");
    assert_eq!(report.count(Action::Removed), 1, "{}", report.text());
}

#[test]
fn uninstall_dry_run_changes_nothing() {
    let scratch = Scratch::new("uninstall-dry");
    scratch.touch_dir(".claude");
    let ctx = scratch.context();
    install::install(&ctx, &InstallRequest::default()).expect("install");
    let before = scratch.read(&scratch.home().join(".claude.json"));

    let report = install::uninstall(
        &ctx,
        &UninstallRequest {
            dry_run: true,
            ..UninstallRequest::default()
        },
    )
    .expect("dry run");
    assert!(report.count(Action::Planned) >= 1, "{}", report.text());
    assert_eq!(scratch.read(&scratch.home().join(".claude.json")), before);
}

// -------------------------------------------------------------------- init ---

#[test]
fn init_writes_the_manifest_and_the_index_summary() {
    let scratch = Scratch::new("init");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");

    let report = project::init(&scratch.project(), &[], &IndexOptions::default()).expect("init");
    assert!(report.created);
    assert_eq!(report.files, 1);
    assert_eq!(report.revision.len(), 40, "a content-addressed revision");
    assert!(report.revision.chars().all(|c| c.is_ascii_hexdigit()));

    let manifest = scratch.json(&scratch.project().join(".ukma/project.json"));
    assert_eq!(manifest["format"], json!("ukma.project"));
    assert_eq!(manifest["schema_version"], json!(project::SCHEMA_VERSION));
    assert_eq!(manifest["roots"], json!(["."]));
    assert!(manifest["created_at"].is_string());

    let summary = scratch.json(&scratch.project().join(".ukma/last-index.json"));
    assert_eq!(summary["revision"], json!(report.revision));
    assert_eq!(summary["files"], json!(1));
}

#[test]
fn init_refreshes_the_index_and_keeps_the_creation_time() {
    let scratch = Scratch::new("init-refresh");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let first = project::init(&scratch.project(), &[], &IndexOptions::default()).expect("init");
    let created_at = scratch.json(&scratch.project().join(".ukma/project.json"))["created_at"]
        .as_str()
        .expect("created_at")
        .to_string();

    fs::write(scratch.project().join("lib.rs"), "pub fn helper() {}\n").expect("seed");
    let second = project::init(&scratch.project(), &[], &IndexOptions::default()).expect("re-init");
    assert!(
        !second.created,
        "a re-init refreshes, it does not re-create"
    );
    assert_eq!(second.files, 2);
    assert_ne!(
        second.revision, first.revision,
        "the revision follows the tree"
    );

    let manifest = scratch.json(&scratch.project().join(".ukma/project.json"));
    assert_eq!(manifest["created_at"], json!(created_at));
}

#[test]
fn init_records_the_excludes_and_the_file_cap() {
    let scratch = Scratch::new("init-options");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    fs::write(scratch.project().join("skip.md"), "# skip\n").expect("seed");
    let options = IndexOptions {
        excludes: vec!["*.md".into()],
        max_files: Some(5),
    };

    let report = project::init(&scratch.project(), &[], &options).expect("init");
    assert_eq!(report.files, 1, "the excluded file is not indexed");
    let manifest = scratch.json(&scratch.project().join(".ukma/project.json"));
    assert_eq!(manifest["excludes"], json!(["*.md"]));
    assert_eq!(manifest["max_files"], json!(5));

    let discovered = project::discover(&scratch.project())
        .expect("discovery")
        .expect("a project manifest");
    let merged = discovered.index_options(&IndexOptions::default());
    assert_eq!(merged.excludes, vec!["*.md".to_string()]);
    assert_eq!(merged.max_files, Some(5));

    let explicit = IndexOptions {
        excludes: vec!["target/**".into()],
        max_files: Some(1),
    };
    let merged = discovered.index_options(&explicit);
    assert_eq!(
        merged.excludes,
        vec!["target/**".to_string(), "*.md".to_string()],
        "the manifest excludes are added"
    );
    assert_eq!(merged.max_files, Some(1), "the command line wins");
}

#[test]
fn init_refuses_a_path_that_is_not_a_directory() {
    let scratch = Scratch::new("init-missing");
    let error = project::init(&scratch.root.join("nope"), &[], &IndexOptions::default())
        .expect_err("a missing directory is refused");
    assert!(matches!(error, SetupError::Missing(_)));
}

#[test]
fn uninit_removes_the_project_directory_once() {
    let scratch = Scratch::new("uninit");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    project::init(&scratch.project(), &[], &IndexOptions::default()).expect("init");
    assert!(scratch.project().join(".ukma").is_dir());

    let report = project::uninit(&scratch.project()).expect("uninit");
    assert!(report.removed);
    assert!(!scratch.project().join(".ukma").exists());
    assert!(
        scratch.project().join("main.rs").is_file(),
        "the sources are untouched"
    );

    let again = project::uninit(&scratch.project()).expect("uninit twice");
    assert!(!again.removed);
}

#[test]
fn discover_validates_the_manifest() {
    let scratch = Scratch::new("discover");
    assert_eq!(
        project::discover(&scratch.project()).expect("no manifest"),
        None
    );

    project::init(&scratch.project(), &[], &IndexOptions::default()).expect("init");
    let discovered = project::discover(&scratch.project())
        .expect("discovery")
        .expect("a manifest");
    assert_eq!(discovered.manifest.format, project::FORMAT);
    assert_eq!(discovered.roots(), vec![scratch.project()]);
}

// ------------------------------------------------------------------- cache ---

#[test]
fn init_persists_the_snapshot_and_a_reload_reads_nothing() {
    let scratch = Scratch::new("cache-init");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let options = IndexOptions::default();

    let report = project::init(&scratch.project(), &[], &options).expect("init");
    assert!(report.cached, "a single-root init fills .ukma/index.db");
    assert_eq!(report.files, 1);
    assert_eq!(report.read, 1, "the first scan reads the file");
    assert!(scratch.project().join(".ukma/index.db").is_file());

    let snapshot = cache::load(&scratch.project(), &options).expect("load");
    assert_eq!(snapshot.revision, report.revision);
    assert_eq!(snapshot.report.read, 0, "the reload reuses the snapshot");
    assert_eq!(snapshot.report.unchanged, 1);

    // The cached revision is the same id a fresh scan produces.
    let root = scratch.project();
    let roots = [root.as_path()];
    let scan = ukma_mcp::index::index_multi(&roots, &options).expect("scan");
    assert_eq!(snapshot.revision, scan.index.commit_sha);
}

#[test]
fn a_multi_root_init_caches_every_root_together() {
    let scratch = Scratch::new("cache-multi");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let extra = scratch.root.join("extra");
    fs::create_dir_all(&extra).expect("extra root");
    fs::write(extra.join("lib.rs"), "pub fn helper() {}\n").expect("seed the second root");

    let report = project::init(
        &scratch.project(),
        &[extra.display().to_string()],
        &IndexOptions::default(),
    )
    .expect("init");
    assert!(report.cached, "a multi-root project is cached too");
    assert!(scratch.project().join(".ukma/index.db").exists());
    assert_eq!(report.files, 2, "one file from each root");

    // The cached revision must equal a fresh multi-root scan of the same trees.
    let root = scratch.project();
    let roots_vec = [root.clone(), extra.clone()];
    let roots: Vec<&Path> = roots_vec.iter().map(PathBuf::as_path).collect();
    let snapshot = cache::load_roots(&roots, &IndexOptions::default()).expect("load");
    assert_eq!(snapshot.report.read, 0, "both rows come from the cache");
    let scan = ukma_mcp::index::index_multi(&roots, &IndexOptions::default()).expect("scan");
    assert_eq!(snapshot.revision, scan.index.commit_sha);

    // A change in the second root is picked up incrementally.
    fs::write(extra.join("lib.rs"), "pub fn helper() -> u8 { 1 }\n").expect("edit");
    let again = cache::load_roots(&roots, &IndexOptions::default()).expect("reload");
    assert_eq!(
        again.report.read, 1,
        "only the edited file in the second root"
    );
    assert_eq!(again.report.modified, 1);
    assert_eq!(
        again.revision,
        ukma_mcp::index::index_multi(&roots, &IndexOptions::default())
            .expect("scan")
            .index
            .commit_sha
    );
}

#[test]
fn a_multi_root_snapshot_reports_the_second_root_as_pending() {
    let scratch = Scratch::new("cache-multi-pending");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let extra = scratch.root.join("extra");
    fs::create_dir_all(&extra).expect("extra root");
    fs::write(extra.join("lib.rs"), "pub fn helper() {}\n").expect("seed");

    project::init(
        &scratch.project(),
        &[extra.display().to_string()],
        &IndexOptions::default(),
    )
    .expect("init");
    let root = scratch.project();
    let roots_vec = [root.clone(), extra.clone()];
    let roots: Vec<&Path> = roots_vec.iter().map(PathBuf::as_path).collect();
    let options = IndexOptions::default();
    let snapshot = cache::load_roots(&roots, &options).expect("load");

    // The probe resolves each path through the root it came from, so a file in
    // the second root is stat-ed in the second root.
    let probe = cache::pending_probe(&roots, Some(&snapshot), &options).expect("a probe");
    let response = serde_json::json!({ "data": { "path": "lib.rs" } });
    assert!(probe.pending_for(&response).is_empty(), "fresh");
    fs::write(extra.join("lib.rs"), "pub fn helper() -> u8 { 2 }\n").expect("edit");
    let pending = probe.pending_for(&response);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert!(pending[0].reason.contains("pending sync"));
}

#[test]
fn the_cached_gateway_resolves_the_cwd_alias() {
    let scratch = Scratch::new("cache-gateway");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let options = IndexOptions::default();
    let report = project::init(&scratch.project(), &[], &options).expect("init");

    let snapshot = cache::load(&scratch.project(), &options).expect("load");
    let files = cache::source_files(&snapshot.files);
    let parse_cache: std::sync::Arc<dyn storage_ports::snapshot::ParseCache> =
        snapshot.store.clone();
    let cached_gateway = cache::gateway(
        &snapshot.revision,
        &files,
        ukma_mcp::SearchPolicy::default(),
        None,
        ukma_mcp::BackendMode::WithModel,
        None,
        Some(parse_cache),
    )
    .expect("build the gateway from the cache");
    let index = ukma_mcp::CwdIndex {
        commit_sha: snapshot.revision.clone(),
        files: snapshot.files.len(),
    };
    assert_eq!(index.commit_sha, report.revision);

    let request = ukma_cli::parse(&args(&[
        "call",
        "code.analyze_repository",
        r#"{"revision":{"repository_id":"p","vcs":"git","commit_sha":"@cwd","tree_sha":null,"dirty_policy":"snapshot"},"parser_profile":"default","include_globs":[],"exclude_globs":[],"asynchronous":true}"#,
    ]))
    .expect("parse");
    let output = ukma_cli::run(&cached_gateway.gateway, &request, Some(&index))
        .expect("the cached gateway answers");
    assert!(output.contains("\"status\": \"ok\""), "{output}");

    // The parse cache is wired into the same gateway: parsing the unchanged
    // file again is answered from `.ukma/index.db`, not from the parser.
    use code_intelligence::ParserPort;
    let parser = cached_gateway
        .parser
        .clone()
        .expect("the parse cache is wired");
    let source = "fn main() {}\n";
    // Warm: this may parse (it fills the cache) or reuse an entry the dispatch
    // already wrote; either way the next parses must not run the parser.
    let _ = parser.parse("main.rs", "rust", source).expect("warm");
    assert_eq!(
        parser.write_errors(),
        0,
        "the cache write failed: {:?}",
        parser.last_write_error()
    );
    let hits_before = parser.hits();
    let misses_before = parser.misses();
    let first = parser.parse("main.rs", "rust", source).expect("parse");
    let second = parser.parse("main.rs", "rust", source).expect("parse");
    assert_eq!(
        parser.misses(),
        misses_before,
        "unchanged content is never parsed again"
    );
    assert_eq!(
        parser.hits(),
        hits_before + 2,
        "both parses were served from the cache"
    );
    assert_eq!(first, second);
    let entries = storage_ports::snapshot::ParseCache::len(snapshot.store.as_ref())
        .expect("the parse cache holds the file");
    assert!(entries >= 1, "the parse of main.rs was persisted");
}

#[test]
fn status_reports_a_missing_cache_then_the_drift() {
    let scratch = Scratch::new("cache-status");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let root = scratch.project();
    let options = IndexOptions::default();

    let report = ukma_cli::setup::snapshot::status(&root, &options, None).expect("status");
    assert_eq!(report.store, None);
    assert_eq!(report.on_disk, 1);
    assert!(!report.is_current());

    project::init(&root, &[], &options).expect("init");
    let clean = cache::load(&root, &options).expect("load");
    assert_eq!(clean.report.read, 0);

    fs::write(root.join("main.rs"), "fn main() { main(); }\n").expect("edit");
    let store = cache::open_store(&root).expect("store");
    let drifted = ukma_cli::setup::snapshot::status(&root, &options, Some(&store)).expect("status");
    assert!(!drifted.is_current());
    assert_eq!(drifted.pending.len(), 1);
    assert_eq!(drifted.pending[0].path, "main.rs");
    assert_eq!(drifted.pending[0].kind, PendingKind::Modified);
}

#[test]
fn init_heals_an_unusable_snapshot_cache() {
    let scratch = Scratch::new("cache-heal");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let options = IndexOptions::default();
    let first = project::init(&scratch.project(), &[], &options).expect("init");

    let db = scratch.project().join(".ukma/index.db");
    fs::write(&db, b"not a database").expect("corrupt the cache");

    let healed = project::init(&scratch.project(), &[], &options).expect("init heals the cache");
    assert_eq!(healed.revision, first.revision, "the tree did not change");
    assert!(healed.cached);
}

#[test]
fn sync_force_rebuilds_an_unusable_snapshot_cache() {
    let scratch = Scratch::new("cache-force");
    fs::write(scratch.project().join("main.rs"), "fn main() {}\n").expect("seed");
    let root = scratch.project();
    let options = IndexOptions::default();
    project::init(&root, &[], &options).expect("init");
    fs::write(root.join(".ukma/index.db"), b"not a database").expect("corrupt");

    // The non-forced path reports the corruption.
    let error = cache::open_store(&root).expect_err("a corrupt cache is refused");
    assert!(matches!(error, SetupError::Cache { .. }));

    // ...and the forced path rebuilds it.
    let store = cache::recreate(&root).expect("recreate");
    let report = ukma_cli::setup::snapshot::sync(&root, &options, &store).expect("sync");
    assert!(report.full);
    assert_eq!(report.files, 1);
}

#[test]
fn the_pending_probe_reports_a_file_edited_after_the_snapshot() {
    let scratch = Scratch::new("cache-pending");
    let source = scratch.project().join("src");
    fs::create_dir_all(&source).expect("mkdir");
    fs::write(source.join("a.rs"), "fn a() {}\n").expect("seed");
    fs::write(scratch.project().join("b.rs"), "fn b() {}\n").expect("seed");
    let root = scratch.project();
    let options = IndexOptions::default();
    project::init(&root, &[], &options).expect("init");

    let snapshot = cache::load(&root, &options).expect("load");
    let probe = cache::pending_probe(&[&root], Some(&snapshot), &options).expect("a probe");

    // Nothing changed yet: a response naming both files is fresh.
    let response = serde_json::json!({
        "data": { "symbols": [
            { "definition": { "path": "src/a.rs", "line": 1 } },
            { "definition": { "path": "b.rs", "line": 1 } }
        ]}
    });
    assert!(probe.pending_for(&response).is_empty());

    // One edit makes exactly that file pending.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(source.join("a.rs"), "fn a() { a(); }\n").expect("edit");
    let pending = probe.pending_for(&response);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].path, "src/a.rs");
    let banner = ukma_mcp::banner(&pending).expect("a banner");
    assert!(banner.contains("src/a.rs"), "{banner}");

    // A response that does not mention the edited file stays clean.
    let unrelated = serde_json::json!({ "data": { "path": "b.rs" } });
    assert!(probe.pending_for(&unrelated).is_empty());
}

#[test]
fn the_pending_probe_falls_back_to_a_stat_walk_without_a_snapshot() {
    let scratch = Scratch::new("cache-pending-scan");
    fs::write(scratch.project().join("a.rs"), "fn a() {}\n").expect("seed");
    let root = scratch.project();
    let options = IndexOptions::default();

    let probe = cache::pending_probe(&[&root], None, &options).expect("a probe from the walk");
    let response = serde_json::json!({ "path": "a.rs" });
    assert!(probe.pending_for(&response).is_empty(), "fresh tree");

    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(root.join("a.rs"), "fn a() { a(); }\n").expect("edit");
    let pending = probe.pending_for(&response);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, "a.rs");
}

#[test]
fn an_empty_tree_has_no_pending_probe() {
    let scratch = Scratch::new("cache-pending-empty");
    let root = scratch.project();
    assert!(cache::pending_probe(&[&root], None, &IndexOptions::default()).is_none());
}

#[test]
fn discover_rejects_a_newer_schema_and_a_foreign_format() {
    let scratch = Scratch::new("discover-version");
    fs::create_dir_all(scratch.project().join(".ukma")).expect("mkdir");
    let path = scratch.project().join(".ukma/project.json");

    fs::write(
        &path,
        format!(
            r#"{{"format":"ukma.project","schema_version":{},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#,
            project::SCHEMA_VERSION + 1
        ),
    )
    .expect("seed");
    let error = project::discover(&scratch.project()).expect_err("a newer schema is refused");
    match error {
        SetupError::Config { detail, .. } => assert!(detail.contains("schema_version")),
        other => panic!("unexpected {other:?}"),
    }

    fs::write(
        &path,
        r#"{"format":"other.tool","schema_version":1,"created_at":"x","updated_at":"x"}"#,
    )
    .expect("seed");
    let error = project::discover(&scratch.project()).expect_err("a foreign format is refused");
    assert!(matches!(error, SetupError::Config { .. }));

    fs::write(&path, "{not json").expect("seed");
    let error = project::discover(&scratch.project()).expect_err("invalid JSON is refused");
    assert!(matches!(error, SetupError::Config { .. }));
}
