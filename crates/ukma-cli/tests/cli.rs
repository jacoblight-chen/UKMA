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
    assert!(capabilities.contains("\"domain_operations\": 108"));
    assert!(capabilities.contains("\"system_operations\": 4"));
}

#[test]
fn tools_list_every_tool_and_can_be_filtered() {
    let all = output("tools");
    let total_line = all.lines().last().expect("total line");
    assert!(total_line.starts_with("total=112"), "{total_line}");
    assert!(total_line.contains("code=27"));
    assert!(total_line.contains("document=16"));
    assert!(total_line.contains("memory=25"));
    assert!(total_line.contains("pro=27"));
    assert!(total_line.contains("traceability=13"));
    assert!(total_line.contains("system=4"));
    for spec in OPERATIONS {
        assert!(all.contains(spec.mcp_tool), "{} missing", spec.mcp_tool);
    }

    let memory = output("tools --domain memory");
    assert!(memory.lines().count() <= 26);
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
    ] {
        assert!(help.contains(command), "{command} missing from help");
    }
}
