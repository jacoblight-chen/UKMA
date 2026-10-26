//! CLI contract tests (禮13, 禮39.2, P8-WP4).
//!
//! Every command is run against the real in-memory gateway, so a broken registry
//! projection or a missing handler is caught here rather than by a user.

use ukma_cli::{exit_code, run, CliError, Parsed};
use ukma_contracts::generated::OPERATIONS;
use ukma_mcp::default_gateway;

fn args(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

fn parse(line: &str) -> Parsed {
    ukma_cli::parse(&args(line)).expect("the command line parses")
}

fn output(line: &str) -> String {
    let gateway = default_gateway();
    run(&gateway, &parse(line), None).expect("command succeeds")
}

#[test]
fn health_and_capabilities_are_reported() {
    let health = output("health");
    assert!(health.contains("\"spec_version\":\"1.2.3\""));
    assert!(health.contains(&format!("\"operations\":{}", OPERATIONS.len())));

    let capabilities = output("capabilities");
    assert!(capabilities.contains("\"domain_operations\": 120"));
    assert!(capabilities.contains("\"system_operations\": 5"));
}

#[test]
fn tools_list_every_tool_and_can_be_filtered() {
    let all = output("tools");
    let total_line = all.lines().last().expect("total line");
    assert!(total_line.starts_with("total=125"), "{total_line}");
    assert!(total_line.contains("code=33"));
    assert!(total_line.contains("document=17"));
    assert!(total_line.contains("memory=28"));
    assert!(total_line.contains("pro=29"));
    assert!(total_line.contains("traceability=13"));
    assert!(total_line.contains("system=5"));
    for spec in OPERATIONS {
        assert!(all.contains(spec.mcp_tool), "{} missing", spec.mcp_tool);
    }

    let memory = output("tools --domain memory");
    assert!(memory.lines().count() <= 29);
    assert!(memory.contains("ukma_memory_search_v1"));
    assert!(!memory.contains("ukma_code_get_symbol_v1"));
}

#[test]
fn routes_include_the_two_get_routes() {
    let routes = output("routes");
    assert!(routes.contains("GET /api/v1/health"));
    assert!(routes.contains("GET /api/v1/jobs/{job_id}"));
    for spec in OPERATIONS {
        assert!(
            routes.contains(spec.server_method),
            "{} missing",
            spec.server_method
        );
    }
}

#[test]
fn describe_reads_one_operation_from_the_registry() {
    let described = output("describe memory.store_episode");
    assert!(described.contains("ukma_memory_store_episode_v1"));
    assert!(described.contains("memory:write"));

    let gateway = default_gateway();
    let error = run(&gateway, &parse("describe nope.nope"), None).unwrap_err();
    assert_eq!(error, CliError::UnknownOperation("nope.nope".into()));
    assert_eq!(exit_code(&error), 2);
}

#[test]
fn call_dispatches_and_reports_api_errors() {
    let created = output("call memory.store_episode {\"input\":{\"session_id\":\"s1\",\"agent_id\":\"a1\",\"agent_depth\":0,\"episode_type\":\"conversation_turn\",\"summary\":\"hello\",\"key_facts\":[],\"outcome\":\"success\",\"source_refs\":[],\"importance_score\":0.4,\"language\":\"en\",\"evidence\":[]}}");
    assert!(created.contains("\"status\": \"ok\""));
    assert!(created.contains("mem:tenant-dev:project-dev:"));

    let gateway = default_gateway();
    let error = run(&gateway, &parse("call memory.retrieve {\"memory\":{\"id\":\"nope\",\"kind\":\"episodic\",\"revision\":\"r\"},\"detail\":\"full\",\"max_tokens\":8}"), None)
        .unwrap_err();
    match error {
        CliError::Api { code, .. } => assert_eq!(code, "NOT_FOUND"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_malformed_body_is_a_usage_error() {
    let gateway = default_gateway();
    let error = run(&gateway, &parse("call memory.search not-json"), None).unwrap_err();
    assert!(matches!(error, CliError::BadJson(_)));
    assert_eq!(exit_code(&error), 2);
}

#[test]
fn unknown_commands_and_missing_arguments_are_usage_errors() {
    let gateway = default_gateway();
    assert!(matches!(
        run(&gateway, &parse("nope"), None).unwrap_err(),
        CliError::Usage(_)
    ));
    assert!(matches!(
        run(&gateway, &parse("call"), None).unwrap_err(),
        CliError::Usage(_)
    ));
    // A flag without a value is a parse-time usage error.
    let missing_value = ukma_cli::parse(&args("tools --domain")).unwrap_err();
    assert!(matches!(missing_value, CliError::Usage(_)));
    assert_eq!(exit_code(&missing_value), 2);
    assert!(matches!(
        run(&gateway, &parse("call nope.nope {}"), None).unwrap_err(),
        CliError::UnknownOperation(_)
    ));
}

#[test]
fn the_manifest_command_is_the_gateway_manifest() {
    let manifest: serde_json::Value = serde_json::from_str(&output("manifest")).expect("json");
    assert_eq!(
        manifest["tools"].as_array().unwrap().len(),
        OPERATIONS.len()
    );
    assert_eq!(manifest["api_major"], 1);
}

#[test]
fn the_context_can_be_overridden() {
    let text = output(
        "call system.capabilities --tenant tenant-z --project project-y --scope system:read",
    );
    assert!(text.contains("\"status\": \"ok\""));
    assert_eq!(
        ukma_cli::Options::default()
            .context("memory.retrieve")
            .scopes,
        vec!["memory:read".to_string()]
    );
}

#[test]
fn help_lists_every_command() {
    let help = output("--help");
    for command in [
        "health",
        "capabilities",
        "tools",
        "routes",
        "describe",
        "manifest",
        "call",
        "init",
        "sync",
        "status",
        "uninit",
        "install",
        "uninstall",
        "upgrade",
    ] {
        assert!(help.contains(command), "{command} missing from help");
    }
}

#[test]
fn the_version_flag_reports_the_crate_version() {
    let gateway = default_gateway();
    let text = run(&gateway, &parse("--version"), None).expect("version prints");
    assert_eq!(text, format!("ukma {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn the_setup_flags_parse() {
    let parsed = parse(
        "install --target claude-code --target codex --dry-run --no-rules \
         --home H --cwd C --binary B --index-cwd src",
    );
    assert_eq!(
        parsed.targets,
        vec!["claude-code".to_string(), "codex".to_string()]
    );
    assert!(parsed.dry_run);
    assert_eq!(parsed.rules, Some(false));
    assert_eq!(parsed.home.as_deref(), Some("H"));
    assert_eq!(parsed.cwd.as_deref(), Some("C"));
    assert_eq!(parsed.binary.as_deref(), Some("B"));
    assert_eq!(parsed.index_cwd.as_deref(), Some("src"));

    let upgrade = parse("upgrade --check --from pkg --sha256 abc --dry-run");
    assert!(upgrade.check);
    assert_eq!(upgrade.from.as_deref(), Some("pkg"));
    assert_eq!(upgrade.sha256.as_deref(), Some("abc"));
    assert!(upgrade.dry_run);

    let uninstall = parse("uninstall --yes --remove-cli");
    assert!(uninstall.yes);
    assert!(uninstall.remove_cli);
    assert!(!uninstall.keep_cli);
}

#[test]
fn the_cache_flags_parse() {
    let parsed = parse("status --no-cache --exclude docs/**");
    assert!(parsed.no_cache);
    assert_eq!(parsed.excludes, vec!["docs/**".to_string()]);

    let parsed = parse("sync --force --max-files 5");
    assert!(parsed.force);
    assert_eq!(parsed.max_files, Some(5));
}

#[test]
fn a_setup_failure_has_its_own_exit_code() {
    let gateway = default_gateway();
    // `uninit` is destructive and needs `--yes`.
    let error = run(&gateway, &parse("uninit ."), None).expect_err("unconfirmed uninit");
    assert!(matches!(error, CliError::Setup(_)));
    assert_eq!(exit_code(&error), 4);
}

#[test]
fn install_and_uninstall_run_end_to_end_against_a_scratch_home() {
    let base = std::env::temp_dir().join(format!("ukma-cli-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let project = base.join("project");
    let binary = base.join("bin").join("ukma.exe");
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    std::fs::create_dir_all(&project).expect("project");

    let gateway = default_gateway();
    let install_line = [
        "install".to_string(),
        "--target".into(),
        "claude-code".into(),
        "--home".into(),
        home.display().to_string(),
        "--cwd".into(),
        project.display().to_string(),
        "--binary".into(),
        binary.display().to_string(),
    ];
    let report = run(
        &gateway,
        &ukma_cli::parse(&install_line).expect("parses"),
        None,
    )
    .expect("install prints a report");
    assert!(report.contains("claude-code"), "{report}");
    assert!(report.contains("configured"), "{report}");
    let config = std::fs::read_to_string(home.join(".claude.json")).expect("config");
    assert!(
        config.contains("ukma"),
        "the MCP entry is written: {config}"
    );

    let uninstall_line = [
        "uninstall".to_string(),
        "--target".into(),
        "claude-code".into(),
        "--home".into(),
        home.display().to_string(),
        "--cwd".into(),
        project.display().to_string(),
        "--binary".into(),
        binary.display().to_string(),
        "--yes".into(),
    ];
    let report = run(
        &gateway,
        &ukma_cli::parse(&uninstall_line).expect("parses"),
        None,
    )
    .expect("uninstall prints a report");
    assert!(report.contains("removed"), "{report}");
    let config = std::fs::read_to_string(home.join(".claude.json")).expect("config");
    assert!(!config.contains("ukma"), "the MCP entry is gone: {config}");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn uninstall_removes_the_cli_by_default_and_keeps_it_with_keep_cli() {
    let base = std::env::temp_dir().join(format!("ukma-cli-uninstall-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let project = base.join("project");
    let binary = base.join("bin").join("ukma.exe");
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    std::fs::create_dir_all(project.clone()).expect("project");
    std::fs::create_dir_all(binary.parent().expect("bin")).expect("bin");
    std::fs::write(&binary, b"fake-binary").expect("seed the binary");

    let gateway = default_gateway();
    let base_args = |verb: &str| {
        vec![
            verb.to_string(),
            "--target".into(),
            "claude-code".into(),
            "--home".into(),
            home.display().to_string(),
            "--cwd".into(),
            project.display().to_string(),
            "--binary".into(),
            binary.display().to_string(),
        ]
    };
    run(
        &gateway,
        &ukma_cli::parse(&base_args("install")).expect("parses"),
        None,
    )
    .expect("install");

    // CodeGraph parity: a bare `uninstall --yes` takes the CLI with it.
    let mut line = base_args("uninstall");
    line.push("--yes".into());
    let report = run(&gateway, &ukma_cli::parse(&line).expect("parses"), None)
        .expect("uninstall prints a report");
    assert!(report.contains("cli"), "{report}");
    assert!(
        !binary.exists(),
        "the default uninstall removes the binary: {report}"
    );

    // `--keep-cli` keeps it, which is the opt-out CodeGraph documents.
    std::fs::write(&binary, b"fake-binary").expect("re-seed");
    let mut line = base_args("uninstall");
    line.push("--yes".into());
    line.push("--keep-cli".into());
    run(&gateway, &ukma_cli::parse(&line).expect("parses"), None).expect("uninstall");
    assert!(binary.is_file(), "--keep-cli keeps the binary");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn init_and_uninit_run_end_to_end_against_a_scratch_project() {
    let base = std::env::temp_dir().join(format!("ukma-cli-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("project");
    std::fs::write(base.join("main.rs"), "fn main() {}\n").expect("seed");

    let gateway = default_gateway();
    let line = ["init".to_string(), base.display().to_string()];
    let report = run(&gateway, &ukma_cli::parse(&line).expect("parses"), None).expect("init");
    assert!(report.contains("revision"), "{report}");
    assert!(report.contains("created"), "{report}");
    assert!(base.join(".ukma/project.json").is_file());
    assert!(base.join(".ukma/index.db").is_file());

    let line = [
        "uninit".to_string(),
        base.display().to_string(),
        "--yes".into(),
    ];
    let report = run(&gateway, &ukma_cli::parse(&line).expect("parses"), None).expect("uninit");
    assert!(report.contains("removed"), "{report}");
    assert!(!base.join(".ukma").exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn sync_and_status_run_end_to_end_against_a_scratch_project() {
    let base = std::env::temp_dir().join(format!("ukma-cli-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("project");
    std::fs::write(base.join("main.rs"), "fn main() {}\n").expect("seed");
    let root = base.display().to_string();
    let gateway = default_gateway();

    let output = run(
        &gateway,
        &ukma_cli::parse(&["init".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("init");
    assert!(output.contains("snapshot "), "{output}");
    assert!(output.contains("index.db"), "{output}");
    assert!(base.join(".ukma/index.db").is_file());

    let status = run(
        &gateway,
        &ukma_cli::parse(&["status".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("status");
    assert!(status.contains("up to date"), "{status}");

    std::fs::write(base.join("main.rs"), "fn main() { main(); }\n").expect("edit");
    let status = run(
        &gateway,
        &ukma_cli::parse(&["status".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("status");
    assert!(status.contains("### Pending sync:"), "{status}");
    assert!(status.contains("modified  main.rs"), "{status}");

    let synced = run(
        &gateway,
        &ukma_cli::parse(&["sync".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("sync");
    assert!(synced.contains("1 modified"), "{synced}");
    assert!(synced.contains("1 of 1 files"), "{synced}");

    let again = run(
        &gateway,
        &ukma_cli::parse(&["sync".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("sync");
    assert!(again.contains("nothing to read"), "{again}");

    let forced = run(
        &gateway,
        &ukma_cli::parse(&["sync".to_string(), root.clone(), "--force".into()]).expect("parse"),
        None,
    )
    .expect("forced sync");
    assert!(forced.contains("full rescan"), "{forced}");

    // `--verify` hashes every file, so the report says so.
    let verified = run(
        &gateway,
        &ukma_cli::parse(&["status".to_string(), root.clone(), "--verify".into()]).expect("parse"),
        None,
    )
    .expect("verified status");
    assert!(verified.contains("file(s) hashed by content"), "{verified}");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn daemon_status_reports_a_stopped_daemon_and_rejects_unknown_commands() {
    let base = std::env::temp_dir().join(format!("ukma-cli-daemon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("project");
    std::fs::write(base.join("main.rs"), "fn main() {}\n").expect("seed");
    let root = base.display().to_string();
    let gateway = default_gateway();

    run(
        &gateway,
        &ukma_cli::parse(&["init".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("init");

    let status = run(
        &gateway,
        &ukma_cli::parse(&["daemon".to_string(), "status".into(), root.clone()]).expect("parse"),
        None,
    )
    .expect("status");
    assert!(status.contains("daemon    not running"), "{status}");
    assert!(status.contains("ukma daemon start"), "{status}");

    let stopped = run(
        &gateway,
        &ukma_cli::parse(&["daemon".to_string(), "stop".into(), root.clone()]).expect("parse"),
        None,
    )
    .expect("stop");
    assert!(stopped.contains("not running"), "{stopped}");

    let error = run(
        &gateway,
        &ukma_cli::parse(&["daemon".to_string(), "nope".into(), root.clone()]).expect("parse"),
        None,
    )
    .expect_err("an unknown daemon command is a usage error");
    assert!(
        error.to_string().contains("unknown daemon command"),
        "{error}"
    );

    // `ukma status` names the daemon too, so a stale warm index is visible.
    let report = run(
        &gateway,
        &ukma_cli::parse(&["status".to_string(), root.clone()]).expect("parse"),
        None,
    )
    .expect("status");
    assert!(report.contains("daemon    not running"), "{report}");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn status_without_a_project_reports_no_snapshot() {
    let base = std::env::temp_dir().join(format!("ukma-cli-nostatus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("dir");
    let gateway = default_gateway();
    let status = run(
        &gateway,
        &ukma_cli::parse(&["status".to_string(), base.display().to_string()]).expect("parse"),
        None,
    )
    .expect("status");
    assert!(status.contains("snapshot  none"), "{status}");
    assert!(status.contains("run `ukma init`"), "{status}");
    let _ = std::fs::remove_dir_all(&base);
}
