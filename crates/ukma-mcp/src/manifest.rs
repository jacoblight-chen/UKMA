//! Tool manifest derived from the single operation registry (§36.4, MCP-001).
//!
//! Nothing here is hand-written per operation: every tool is projected from
//! `ukma_contracts::generated::OPERATIONS`, so the manifest cannot drift from the
//! registry. Manifest coverage is *not* a claim that the JSON Schemas are closed
//! (§31.3.1): the schemas remain preliminary.

use std::collections::BTreeSet;

use ukma_contracts::generated::{OperationSpec, OPERATIONS};

use crate::rest;

/// Registered domain operations (code 27 + document 16 + memory 25 + trace 13
/// + pro 27).
pub const DOMAIN_OPERATION_COUNT: usize = 108;
/// Registered system operations (`capabilities`, `get_operation`, `get_job`, `cancel_job`).
pub const SYSTEM_OPERATION_COUNT: usize = 4;
/// Prefix shared by every MCP tool name.
pub const TOOL_PREFIX: &str = "ukma_";

/// One MCP tool projection of an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// MCP tool name, e.g. `ukma_code_get_symbol_v1`.
    pub name: &'static str,
    /// Canonical operation id.
    pub operation_id: &'static str,
    /// Domain segment (`code`, `document`, `memory`, `traceability`, `system`).
    pub domain: &'static str,
    /// Whether this is one of the four system operations.
    pub system: bool,
    /// Request JSON Schema `$id`.
    pub input_schema: &'static str,
    /// Response JSON Schema `$id`.
    pub output_schema: &'static str,
    /// Scopes required by the operation.
    pub required_scopes: &'static [&'static str],
    /// REST route declared by the registry.
    pub rest: &'static str,
    /// Whether the operation needs an idempotency key.
    pub needs_idempotency_key: bool,
    /// Whether the operation needs an `expected_etag`.
    pub needs_expected_etag: bool,
    /// Whether the operation is read-only.
    pub read_only: bool,
}

fn domain_of(operation_id: &'static str) -> &'static str {
    operation_id.split('.').next().unwrap_or("unknown")
}

/// Project one registry entry into a tool.
pub fn tool_of(spec: &'static OperationSpec) -> Tool {
    Tool {
        name: spec.mcp_tool,
        operation_id: spec.operation_id,
        domain: domain_of(spec.operation_id),
        system: domain_of(spec.operation_id) == "system",
        input_schema: spec.request_schema,
        output_schema: spec.response_schema,
        required_scopes: spec.required_scopes,
        rest: spec.server_method,
        needs_idempotency_key: matches!(
            spec.idempotency,
            ukma_contracts::generated::Idempotency::Required
        ),
        needs_expected_etag: matches!(
            spec.concurrency,
            ukma_contracts::generated::Concurrency::ExpectedEtag
        ),
        read_only: matches!(
            spec.state_machine,
            ukma_contracts::generated::StateMachine::ReadOnly
        ),
    }
}

/// Every tool, in registry order.
pub fn tools() -> Vec<Tool> {
    OPERATIONS.iter().map(tool_of).collect()
}

/// Look up a tool by MCP name.
pub fn tool_by_name(name: &str) -> Option<Tool> {
    OPERATIONS
        .iter()
        .find(|spec| spec.mcp_tool == name)
        .map(tool_of)
}

/// Look up a tool by canonical operation id.
pub fn tool_for_operation(operation_id: &str) -> Option<Tool> {
    OPERATIONS
        .iter()
        .find(|spec| spec.operation_id == operation_id)
        .map(tool_of)
}

/// Operation id for an MCP tool name.
pub fn operation_of_tool(name: &str) -> Option<&'static str> {
    OPERATIONS
        .iter()
        .find(|spec| spec.mcp_tool == name)
        .map(|spec| spec.operation_id)
}

/// Search + symbol info + AI context (CodeGraph `core` parity).
const CORE_OPERATIONS: &[&str] = &[
    "code.find_symbols",
    "code.get_symbol",
    "code.get_ai_context",
    "code.get_edit_context",
    "code.get_curated_context",
    "code.search_by_pattern",
    "code.search_by_error",
    "code.find_by_signature",
];

/// Structural / graph / impact / PR context (CodeGraph `graph` parity).
const GRAPH_OPERATIONS: &[&str] = &[
    "code.query_callers",
    "code.query_callees",
    "code.query_dependencies",
    "code.analyze_impact",
    "code.compare_revisions",
    "code.find_circular_deps",
    "code.find_hot_paths",
    "code.find_dead_imports",
    "code.find_entry_points",
    "code.find_related_tests",
    "code.analyze_complexity",
    "code.get_module_summary",
    "code.index_files",
    "code.pr_context",
    "code.analyze_repository",
    "code.get_analysis_job",
    "code.find_references",
];

/// CodeGraph Pro security analyzers (the 17 `pro.security_*` /
/// `pro.scan_security` tools).
const PRO_SECURITY_OPERATIONS: &[&str] = &[
    "pro.scan_security",
    "pro.security_control_flow",
    "pro.security_trace_data_flow",
    "pro.security_generate_sbom",
    "pro.security_audit_deps",
    "pro.security_check_unchecked_returns",
    "pro.security_check_resource_leaks",
    "pro.security_check_misconfig",
    "pro.security_check_input_validation",
    "pro.security_check_error_exposure",
    "pro.security_scan_iac",
    "pro.security_check_licenses",
    "pro.security_check_secrets_entropy",
    "pro.security_detect_injection",
    "pro.security_check_search_path",
    "pro.security_check_crypto",
    "pro.security_export_sarif",
];

/// A named subset of the tool surface (CodeGraph `--profile` parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolProfile {
    /// Every tool (default).
    #[default]
    All,
    /// Search + symbol info + AI context.
    Core,
    /// Callers/callees/deps/impact/traverse/PR context.
    Graph,
    /// Memory + document tools.
    Memory,
    /// Security tools only (the 17 CodeGraph Pro analyzers).
    Security,
}

impl ToolProfile {
    /// Parse a profile name; `None` when unrecognised.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "all" => Some(ToolProfile::All),
            "core" => Some(ToolProfile::Core),
            "graph" => Some(ToolProfile::Graph),
            "memory" => Some(ToolProfile::Memory),
            "security" => Some(ToolProfile::Security),
            _ => None,
        }
    }

    /// The canonical profile names, for usage text.
    pub const NAMES: &'static [&'static str] = &["all", "core", "graph", "memory", "security"];

    /// Whether an operation id belongs to this profile.
    pub fn contains(&self, operation_id: &str) -> bool {
        match self {
            ToolProfile::All => true,
            ToolProfile::Core => CORE_OPERATIONS.contains(&operation_id),
            ToolProfile::Graph => GRAPH_OPERATIONS.contains(&operation_id),
            ToolProfile::Memory => {
                operation_id.starts_with("memory.") || operation_id.starts_with("document.")
            }
            ToolProfile::Security => PRO_SECURITY_OPERATIONS.contains(&operation_id),
        }
    }

    /// The tools visible under this profile, in registry order.
    pub fn tools(&self) -> Vec<Tool> {
        OPERATIONS
            .iter()
            .filter(|spec| self.contains(spec.operation_id))
            .map(tool_of)
            .collect()
    }
}

/// Manifest drift findings; empty means the projection is consistent.
///
/// Checks are structural, not semantic: unique tool names, the `ukma_` prefix, the
/// registered counts, a POST route per domain operation and a tool for every
/// operation.
pub fn drift() -> Vec<String> {
    let mut problems = Vec::new();
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut domain_count = 0usize;
    let mut system_count = 0usize;
    for spec in OPERATIONS {
        let tool = tool_of(spec);
        if !names.insert(tool.name) {
            problems.push(format!("duplicate tool name {}", tool.name));
        }
        if !ids.insert(tool.operation_id) {
            problems.push(format!("duplicate operation id {}", tool.operation_id));
        }
        if !tool.name.starts_with(TOOL_PREFIX) {
            problems.push(format!(
                "{} does not use the {TOOL_PREFIX} prefix",
                tool.name
            ));
        }
        if tool.input_schema.is_empty() || tool.output_schema.is_empty() {
            problems.push(format!("{} has no schema id", tool.operation_id));
        }
        if tool.system {
            system_count += 1;
        } else {
            domain_count += 1;
            if !tool.rest.starts_with("POST /api/v1/") {
                problems.push(format!(
                    "{} declares a non-POST route {}",
                    tool.operation_id, tool.rest
                ));
            }
            if rest::route_of(tool.rest).is_none() {
                problems.push(format!("{} route cannot be parsed", tool.rest));
            }
        }
    }
    if domain_count != DOMAIN_OPERATION_COUNT {
        problems.push(format!(
            "registry has {domain_count} domain operations, expected {DOMAIN_OPERATION_COUNT}"
        ));
    }
    if system_count != SYSTEM_OPERATION_COUNT {
        problems.push(format!(
            "registry has {system_count} system operations, expected {SYSTEM_OPERATION_COUNT}"
        ));
    }
    problems
}

/// Render the manifest as JSON for `tool-manifest.json` consumers.
pub fn manifest_json() -> String {
    let tools: Vec<serde_json::Value> = tools()
        .into_iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "operation_id": tool.operation_id,
                "domain": tool.domain,
                "system": tool.system,
                "description": format!("{} ({})", tool.operation_id, tool.rest),
                "input_schema": tool.input_schema,
                "output_schema": tool.output_schema,
                "required_scopes": tool.required_scopes,
                "rest": tool.rest,
                "idempotency_key_required": tool.needs_idempotency_key,
                "expected_etag_required": tool.needs_expected_etag,
                "read_only": tool.read_only,
            })
        })
        .collect();
    serde_json::json!({
        "spec_version": "1.2.3",
        "api_major": 1,
        "source": "contracts/api/operation-registry.yaml",
        "domain_operations": DOMAIN_OPERATION_COUNT,
        "system_operations": SYSTEM_OPERATION_COUNT,
        "tools": tools,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_matches_the_registry_without_drift() {
        assert_eq!(drift(), Vec::<String>::new());
        assert_eq!(
            tools().len(),
            DOMAIN_OPERATION_COUNT + SYSTEM_OPERATION_COUNT
        );
    }

    #[test]
    fn the_domain_split_is_the_registered_one() {
        let tools = tools();
        let count = |domain: &str| tools.iter().filter(|tool| tool.domain == domain).count();
        assert_eq!(count("code"), 27);
        assert_eq!(count("document"), 16);
        assert_eq!(count("memory"), 25);
        assert_eq!(count("traceability"), 13);
        assert_eq!(count("system"), 4);
    }

    #[test]
    fn tools_are_named_from_the_registry() {
        let tool = tool_by_name("ukma_code_get_symbol_v1").expect("registered");
        assert_eq!(tool.operation_id, "code.get_symbol");
        assert_eq!(tool.rest, "POST /api/v1/code/get_symbol");
        assert!(tool.read_only);
        assert!(!tool.needs_idempotency_key);
        assert_eq!(
            operation_of_tool("ukma_memory_get_budget_v1"),
            Some("memory.get_budget")
        );
        assert!(tool_by_name("ukma_unknown_v1").is_none());
        assert!(tool_for_operation("code.get_symbol").is_some());
    }

    #[test]
    fn idempotency_and_etag_requirements_come_from_the_registry() {
        let store = tool_by_name("ukma_memory_store_episode_v1").unwrap();
        assert!(store.needs_idempotency_key);
        let update = tool_by_name("ukma_memory_update_revision_v1").unwrap();
        assert!(update.needs_expected_etag);
        let read = tool_by_name("ukma_memory_search_v1").unwrap();
        assert!(!read.needs_idempotency_key);
        assert!(!read.needs_expected_etag);
    }

    #[test]
    fn the_manifest_json_lists_every_tool() {
        let json: serde_json::Value = serde_json::from_str(&manifest_json()).unwrap();
        assert_eq!(json["api_major"], 1);
        assert_eq!(json["domain_operations"], DOMAIN_OPERATION_COUNT);
        assert_eq!(json["system_operations"], SYSTEM_OPERATION_COUNT);
        assert_eq!(
            json["tools"].as_array().unwrap().len(),
            DOMAIN_OPERATION_COUNT + SYSTEM_OPERATION_COUNT
        );
        assert_eq!(json["tools"][0]["name"], tools()[0].name);
    }
}
