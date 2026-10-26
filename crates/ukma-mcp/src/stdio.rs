//! MCP stdio surface (MCP 2024-11-05, 禮31.2): newline-delimited JSON-RPC 2.0
//! over stdin/stdout.
//!
//! This is the `--mcp` mode of the `ukma` binary — the same gateway the HTTP
//! server and the CLI use, on a new transport. Nothing here re-declares an
//! operation, a scope or an error code: `tools/list` is projected from the
//! manifest (itself a projection of the registry) and `tools/call` dispatches
//! through the gateway, so the 37-code closed set stays enforced in exactly one
//! place. The host's authentication decision is the gateway's scope resolver
//! (default: `EnvelopeScopes`, the local-trusted profile the CLI uses).

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::gateway::Gateway;
use crate::manifest;
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ScopeRef};

/// The protocol version this server speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";
/// The server name reported to the host.
pub const SERVER_NAME: &str = "ukma";
/// The envelope version the server speaks.
pub const API_VERSION: &str = "1.2.3";

/// A stdio session bound to one gateway.
pub struct StdioSession<'a> {
    gateway: &'a Gateway,
    profile: manifest::ToolProfile,
    probe: Option<std::sync::Arc<dyn crate::pending::PendingProbe>>,
    remote: Option<std::sync::Arc<dyn DaemonCall>>,
}

/// Delegates one tool call to a running project daemon.
///
/// The session keeps its own gateway and falls back to it whenever the daemon
/// is unreachable, so delegation can only make things faster, never unavailable.
pub trait DaemonCall: Send + Sync {
    /// Run one envelope and return its response.
    fn call(
        &self,
        envelope: &ukma_contracts::RequestEnvelope<serde_json::Value>,
    ) -> Result<ukma_contracts::ResponseEnvelope<serde_json::Value>, String>;
}

/// Everything a session needs beyond the gateway itself.
#[derive(Default)]
pub struct SessionOptions {
    /// The exposed tool surface.
    pub profile: manifest::ToolProfile,
    /// Staleness disclosure (see [`crate::pending`]).
    pub probe: Option<std::sync::Arc<dyn crate::pending::PendingProbe>>,
    /// Shared daemon, when one is running for this project.
    pub remote: Option<std::sync::Arc<dyn DaemonCall>>,
}

impl<'a> StdioSession<'a> {
    /// Bind a session to a gateway with the full tool surface.
    pub fn new(gateway: &'a Gateway) -> Self {
        Self {
            gateway,
            profile: manifest::ToolProfile::default(),
            probe: None,
            remote: None,
        }
    }

    /// Bind a session to a gateway filtered by a named tool profile
    /// (CodeGraph `--profile` parity).
    pub fn with_profile(gateway: &'a Gateway, profile: manifest::ToolProfile) -> Self {
        Self {
            gateway,
            profile,
            probe: None,
            remote: None,
        }
    }

    /// Bind a session with the full option set.
    pub fn with_options(gateway: &'a Gateway, options: SessionOptions) -> Self {
        Self {
            gateway,
            profile: options.profile,
            probe: options.probe,
            remote: options.remote,
        }
    }

    /// Bind a session that also discloses staleness for the files a response
    /// references (see [`crate::pending`]).
    pub fn with_profile_and_probe(
        gateway: &'a Gateway,
        profile: manifest::ToolProfile,
        probe: Option<std::sync::Arc<dyn crate::pending::PendingProbe>>,
    ) -> Self {
        Self {
            gateway,
            profile,
            probe,
            remote: None,
        }
    }

    /// Handle one stream line (already trimmed). Returns the response to write,
    /// or `None` when the line is a notification (which carries no reply).
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let request: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return Some(self.error(&Value::Null, -32700, "parse error".to_string())),
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => Some(self.ok(&id, Self::initialize_result())),
            "ping" => Some(self.ok(&id, Value::Object(Default::default()))),
            "tools/list" => Some(self.ok(&id, self.tools_list())),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                self.tools_call(&id, &params)
            }
            "resources/list" => Some(self.ok(&id, json!({ "resources": [] }))),
            "prompts/list" => Some(self.ok(&id, json!({ "prompts": [] }))),
            // Notifications carry no response.
            "notifications/initialized" | "notifications/cancel" => None,
            other => Some(self.error(&id, -32601, format!("method not found: `{other}`"))),
        }
    }

    /// The `initialize` result.
    fn initialize_result() -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": API_VERSION }
        })
    }

    /// `tools/list`: the manifest projected onto MCP tool objects, filtered by
    /// the active profile.
    fn tools_list(&self) -> Value {
        let tools: Vec<Value> = self
            .profile
            .tools()
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": format!("{} ({})", tool.operation_id, tool.rest),
                    "inputSchema": {
                        "type": "object",
                        "$id": tool.input_schema
                    }
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    /// `tools/call`: dispatch through the gateway with the local-trusted
    /// context (the same decision the CLI makes).
    fn tools_call(&self, id: &Value, params: &Value) -> Option<String> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        let Some(tool) = manifest::tool_by_name(name) else {
            // Protocol-level failure: the name is not in the registry, so no
            // operation (and no registry error code) applies.
            return Some(self.error(id, -32602, format!("invalid params: unknown tool `{name}`")));
        };
        if !self.profile.contains(tool.operation_id) {
            // The tool exists in the registry but is hidden by the active
            // profile, so it is not callable from this session.
            return Some(self.error(
                id,
                -32602,
                format!("invalid params: tool `{name}` not in active profile"),
            ));
        }
        let context = dev_context("mcp", tool.operation_id, tool.required_scopes);
        let envelope = RequestEnvelope {
            api_version: API_VERSION.into(),
            operation: tool.operation_id.to_string(),
            context,
            input: arguments,
        };
        let response = match &self.remote {
            Some(remote) => match remote.call(&envelope) {
                Ok(response) => response,
                // The daemon is an optimisation: fall back to the local
                // gateway rather than failing the call.
                Err(_) => self.gateway.dispatch(envelope),
            },
            None => self.gateway.dispatch(envelope),
        };
        match response.error {
            Some(error) => Some(self.api_error(id, &error.code, error.safe_message)),
            None => {
                let envelope = serde_json::to_value(&response).unwrap_or(Value::Null);
                let text = serde_json::to_string_pretty(&envelope).unwrap_or_default();
                // Staleness disclosure: an unchanged response shape (one text
                // block) unless a referenced file changed since the snapshot.
                let mut content = Vec::new();
                if let Some(probe) = &self.probe {
                    if let Some(banner) = crate::pending::banner(&probe.pending_for(&envelope)) {
                        content.push(json!({ "type": "text", "text": banner }));
                    }
                }
                content.push(json!({ "type": "text", "text": text }));
                Some(self.ok(id, json!({ "content": content, "isError": false })))
            }
        }
    }

    fn ok(&self, id: &Value, result: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
    }

    fn error(&self, id: &Value, code: i64, message: String) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
        .to_string()
    }

    /// A tool-level failure: the protocol succeeded, the operation did not.
    /// The code comes from the gateway, so it is always in the closed set.
    fn api_error(&self, id: &Value, code: &str, message: String) -> String {
        self.ok(
            id,
            json!({
                "content": [{ "type": "text", "text": format!("{code}: {message}") }],
                "isError": true
            }),
        )
    }
}

/// The local-trusted request context for one call (mirrors the CLI:
/// expected scopes are the operation's required scopes, and the default
/// `EnvelopeScopes` resolver grants exactly those). `transport` labels the
/// ids so MCP and LSP calls are distinguishable in traces.
pub(crate) fn dev_context(
    transport: &str,
    operation: &str,
    required_scopes: &[&str],
) -> RequestContext {
    RequestContext {
        request_id: format!("{transport}-{operation}"),
        trace_id: format!("trace-{transport}"),
        scope: ScopeRef {
            tenant_id: "tenant-dev".into(),
            project_id: "project-dev".into(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "principal-dev".into(),
        scopes: required_scopes
            .iter()
            .map(|scope| (*scope).to_string())
            .collect(),
        deadline_at: None,
        consistency: Consistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: Some(format!("{transport}-{operation}")),
        expected_etag: Some(format!("\"{transport}-etag\"")),
        delegation_token: None,
    }
}

/// Run the stdio loop with the full tool surface. Returns the process exit
/// code.
pub fn run_stdio(gateway: &Gateway) -> i32 {
    run_stdio_profile(gateway, manifest::ToolProfile::default())
}

/// Run the stdio loop with the tool surface filtered by a named profile
/// (CodeGraph `--profile` parity). Returns the process exit code.
pub fn run_stdio_profile(gateway: &Gateway, profile: manifest::ToolProfile) -> i32 {
    run_stdio_profile_with_probe(gateway, profile, None)
}

/// Run the stdio loop with a staleness probe: a `tools/call` response that
/// references a file changed after the index snapshot is prefixed with an
/// explicit warning (see [`crate::pending`]).
pub fn run_stdio_profile_with_probe(
    gateway: &Gateway,
    profile: manifest::ToolProfile,
    probe: Option<std::sync::Arc<dyn crate::pending::PendingProbe>>,
) -> i32 {
    run_stdio_with(
        gateway,
        SessionOptions {
            profile,
            probe,
            remote: None,
        },
    )
}

/// Run the stdio loop with every session option (profile, staleness probe and
/// optional shared daemon).
pub fn run_stdio_with(gateway: &Gateway, options: SessionOptions) -> i32 {
    let session = StdioSession::with_options(gateway, options);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(response) = session.handle_line(line) {
            let mut out = stdout.lock();
            let _ = writeln!(out, "{response}");
            let _ = out.flush();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::StaticScopes;
    use std::sync::Arc;

    /// Run one request line against a fresh default gateway.
    fn default_line(line: &str) -> Value {
        let gateway = crate::default_gateway();
        let session = StdioSession::new(&gateway);
        let response = session
            .handle_line(line)
            .unwrap_or_else(|| panic!("expected a response for `{line}`"));
        serde_json::from_str(&response).expect("a well-formed response")
    }

    fn call(name: &str, arguments: &Value) -> Value {
        default_line(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
        ))
    }

    /// A probe that always reports one pending file (wiring test).
    struct AlwaysPending;

    impl crate::pending::PendingProbe for AlwaysPending {
        fn pending_for(&self, _response: &Value) -> Vec<crate::pending::PendingFile> {
            vec![crate::pending::PendingFile {
                path: "src/a.rs".into(),
                reason: "edited 800ms ago, pending sync".into(),
            }]
        }
    }

    /// A probe that never reports anything.
    struct NeverPending;

    impl crate::pending::PendingProbe for NeverPending {
        fn pending_for(&self, _response: &Value) -> Vec<crate::pending::PendingFile> {
            Vec::new()
        }
    }

    fn call_with_probe(
        probe: Option<std::sync::Arc<dyn crate::pending::PendingProbe>>,
        name: &str,
        arguments: &Value,
    ) -> Value {
        let gateway = crate::handlers::default_gateway();
        let session =
            StdioSession::with_profile_and_probe(&gateway, manifest::ToolProfile::default(), probe);
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
        );
        let response = session.handle_line(&line).expect("a response");
        serde_json::from_str(&response).expect("a well-formed response")
    }

    /// A daemon stub that answers with a marker, proving the session delegated.
    struct MarkingDaemon;

    impl crate::DaemonCall for MarkingDaemon {
        fn call(
            &self,
            envelope: &ukma_contracts::RequestEnvelope<Value>,
        ) -> Result<ukma_contracts::ResponseEnvelope<Value>, String> {
            Ok(ukma_contracts::ResponseEnvelope {
                api_version: "1".into(),
                request_id: envelope.context.request_id.clone(),
                trace_id: envelope.context.trace_id.clone(),
                status: ukma_contracts::ResponseStatus::Ok,
                data: Some(json!({ "served_by": "daemon" })),
                error: None,
                warnings: Vec::new(),
                partial_reasons: Vec::new(),
                snapshot_id: None,
                index_generation: None,
                freshness: ukma_contracts::Freshness::Current,
                observed_revision: None,
            })
        }
    }

    /// A daemon stub that is always unreachable.
    struct DeadDaemon;

    impl crate::DaemonCall for DeadDaemon {
        fn call(
            &self,
            _envelope: &ukma_contracts::RequestEnvelope<Value>,
        ) -> Result<ukma_contracts::ResponseEnvelope<Value>, String> {
            Err("connection refused".into())
        }
    }

    fn session_call_with(remote: Option<std::sync::Arc<dyn crate::DaemonCall>>) -> Value {
        let gateway = crate::handlers::default_gateway();
        let session = StdioSession::with_options(
            &gateway,
            SessionOptions {
                profile: manifest::ToolProfile::default(),
                probe: None,
                remote,
            },
        );
        let response = session
            .handle_line(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ukma_system_capabilities_v1","arguments":null}}"#,
            )
            .expect("a response");
        serde_json::from_str(&response).expect("a well-formed response")
    }

    #[test]
    fn a_running_daemon_serves_the_tool_call() {
        let value = session_call_with(Some(Arc::new(MarkingDaemon)));
        let content = value["result"]["content"]
            .as_array()
            .expect("content blocks");
        assert_eq!(content.len(), 1, "no banner: the envelope alone");
        let text = content[0]["text"].as_str().expect("text");
        assert!(text.contains("\"served_by\": \"daemon\""), "{text}");
    }

    #[test]
    fn an_unreachable_daemon_falls_back_to_the_local_gateway() {
        let value = session_call_with(Some(Arc::new(DeadDaemon)));
        let text = value["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(
            text.contains("capabilities") || text.contains("\"status\":\"ok\""),
            "the local gateway still answered: {text}"
        );
        assert!(!text.contains("connection refused"), "{text}");
        assert_eq!(value["result"]["isError"], false);
    }

    #[test]
    fn a_pending_reference_prepends_a_banner_and_keeps_the_envelope() {
        let value = call_with_probe(
            Some(std::sync::Arc::new(AlwaysPending)),
            "ukma_system_capabilities_v1",
            &json!(null),
        );
        let content = value["result"]["content"]
            .as_array()
            .expect("content blocks");
        assert_eq!(content.len(), 2, "banner + envelope");
        assert_eq!(content[0]["type"], "text");
        let banner = content[0]["text"].as_str().expect("banner text");
        assert!(banner.contains("src/a.rs"), "{banner}");
        assert!(banner.contains("pending sync"), "{banner}");
        assert!(banner.contains("Read them directly"), "{banner}");
        assert_eq!(value["result"]["isError"], false);

        // The envelope block is still exactly the JSON response.
        let envelope: Value =
            serde_json::from_str(content[1]["text"].as_str().expect("envelope")).expect("json");
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["api_version"], API_VERSION);
    }

    #[test]
    fn a_fresh_response_keeps_the_single_block_shape() {
        for probe in [
            None,
            Some(std::sync::Arc::new(NeverPending)
                as std::sync::Arc<dyn crate::pending::PendingProbe>),
        ] {
            let value = call_with_probe(probe, "ukma_system_capabilities_v1", &json!(null));
            let content = value["result"]["content"]
                .as_array()
                .expect("content blocks");
            assert_eq!(content.len(), 1, "no banner when nothing is pending");
            assert!(content[0]["text"].as_str().expect("text").starts_with('{'));
        }
    }

    #[test]
    fn the_probe_never_changes_a_protocol_error() {
        let value = call_with_probe(
            Some(std::sync::Arc::new(AlwaysPending)),
            "ukma_nope_nope_v1",
            &json!({}),
        );
        assert!(value["error"].is_object(), "{value}");
        assert!(value["result"].is_null());
    }

    #[test]
    fn initialize_reports_the_protocol_version_and_server_info() {
        let value = default_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(value["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(value["result"]["serverInfo"]["version"], API_VERSION);
        assert_eq!(
            value["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[test]
    fn tools_list_projects_every_registered_tool_with_a_schema_reference() {
        let value = default_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
        let tools = value["result"]["tools"].as_array().expect("a tool list");
        assert_eq!(
            tools.len(),
            crate::manifest::DOMAIN_OPERATION_COUNT + crate::manifest::SYSTEM_OPERATION_COUNT,
            "all registered tools"
        );
        let mut names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            crate::manifest::DOMAIN_OPERATION_COUNT + crate::manifest::SYSTEM_OPERATION_COUNT,
            "tool names are unique (no drift)"
        );
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(
                tool["inputSchema"]["$id"]
                    .as_str()
                    .unwrap()
                    .contains("schemas.ukma.dev"),
                "the schema reference points at the canonical URI"
            );
        }
    }

    /// Run one request line against a fresh default gateway with a named profile.
    fn profile_line(line: &str, profile: crate::manifest::ToolProfile) -> Value {
        let gateway = crate::default_gateway();
        let session = StdioSession::with_profile(&gateway, profile);
        let response = session
            .handle_line(line)
            .unwrap_or_else(|| panic!("expected a response for `{line}`"));
        serde_json::from_str(&response).expect("a well-formed response")
    }

    fn list_len(profile: crate::manifest::ToolProfile) -> usize {
        let value = profile_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
            profile,
        );
        value["result"]["tools"]
            .as_array()
            .expect("a tool list")
            .len()
    }

    #[test]
    fn the_core_profile_exposes_search_context_tools_only() {
        let value = profile_line(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            crate::manifest::ToolProfile::Core,
        );
        let tools = value["result"]["tools"].as_array().expect("a tool list");
        assert_eq!(tools.len(), 8, "core exposes the 8 search/context tools");
        let names: Vec<String> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"ukma_code_find_symbols_v1".to_string()));
        assert!(names.contains(&"ukma_code_get_ai_context_v1".to_string()));
        assert!(
            !names.contains(&"ukma_code_index_files_v1".to_string()),
            "a graph tool is hidden from the core profile"
        );
    }

    #[test]
    fn the_graph_profile_exposes_the_structural_tools() {
        assert_eq!(
            list_len(crate::manifest::ToolProfile::Graph),
            17,
            "graph exposes the 17 structural tools"
        );
    }

    #[test]
    fn the_memory_profile_exposes_memory_and_document_tools() {
        assert_eq!(
            list_len(crate::manifest::ToolProfile::Memory),
            45,
            "memory exposes all 28 memory + 17 document tools"
        );
    }

    #[test]
    fn the_security_profile_exposes_the_pro_analyzers() {
        assert_eq!(
            list_len(crate::manifest::ToolProfile::Security),
            19,
            "the security profile exposes the 19 CodeGraph Pro analyzers"
        );
    }

    #[test]
    fn a_call_to_a_tool_outside_the_active_profile_is_rejected() {
        // `code.index_files` is a graph tool, hidden from the core profile.
        let value = profile_line(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ukma_code_index_files_v1","arguments":{"paths":["src/main.rs"]}}}"#,
            crate::manifest::ToolProfile::Core,
        );
        assert!(
            value.get("error").is_some(),
            "a hidden tool is rejected with a protocol error"
        );
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not in active profile"));
    }

    #[test]
    fn a_call_to_a_tool_inside_the_active_profile_reaches_the_gateway() {
        // `code.find_symbols` is in the core profile. A missing/empty
        // repository yields a *registry* error, not a *protocol* error: the
        // absence of a protocol error is the signal that the profile filter
        // accepted the tool and dispatched it to the gateway.
        let value = profile_line(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"ukma_code_find_symbols_v1","arguments":{"query":"main"}}}"#,
            crate::manifest::ToolProfile::Core,
        );
        assert!(
            value.get("error").is_none(),
            "a core tool is accepted by the profile filter (no protocol error)"
        );
    }

    #[test]
    fn tools_call_runs_a_system_operation_and_returns_the_envelope() {
        let value = call("ukma_system_capabilities_v1", &Value::Null);
        assert!(value.get("error").is_none(), "no protocol error");
        assert_eq!(value["result"]["isError"], false, "no tool-level error");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        // The text is a pretty-printed envelope, so the version check is
        // whitespace-insensitive.
        let compact = text.replace(char::is_whitespace, "");
        assert!(
            compact.contains("\"api_version\":\"1.2.3\""),
            "the envelope version is reported"
        );
        assert!(text.contains("capabilities"));
    }

    #[test]
    fn tools_call_reports_registry_errors_as_tool_errors() {
        // Empty input for a mutating operation: the gateway rejects it with a
        // code from the closed set, surfaced as a tool-level (not protocol)
        // error so the host can show it to the user.
        let value = call("ukma_memory_store_episode_v1", &Value::Null);
        assert!(
            value.get("error").is_none(),
            "the protocol itself succeeded"
        );
        assert_eq!(value["result"]["isError"], true);
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("INVALID_SCHEMA") || text.starts_with("FORBIDDEN"),
            "the tool error starts with a registry code, got `{text}`"
        );
    }

    #[test]
    fn authorization_denies_when_the_host_grants_nothing() {
        let mut gateway = crate::default_gateway();
        gateway = gateway.with_scope_resolver(Arc::new(StaticScopes::new()));
        let session = StdioSession::new(&gateway);
        let response = session
            .handle_line(
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ukma_system_capabilities_v1","arguments":null}}"#,
            )
            .expect("a request always gets a response");
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["isError"], true);
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("FORBIDDEN"),
            "a principal with no granted scopes is denied, got `{text}`"
        );
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let value = default_line(r#"{"jsonrpc":"2.0","id":4,"method":"bogus/method","params":{}}"#);
        assert_eq!(value["error"]["code"], -32601);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bogus/method"));
    }

    #[test]
    fn malformed_line_is_a_parse_error() {
        let value = default_line("{not json");
        assert_eq!(value["error"]["code"], -32700);
    }

    #[test]
    fn unknown_tool_name_is_an_invalid_params_error() {
        let value = default_line(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ukma_nope_nope_v1","arguments":{}}}"#,
        );
        assert_eq!(value["error"]["code"], -32602);
    }

    #[test]
    fn notifications_carry_no_response() {
        let gateway = crate::default_gateway();
        let session = StdioSession::new(&gateway);
        assert!(session
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    #[test]
    fn ping_replies_with_an_empty_object() {
        let value = default_line(r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#);
        assert!(value["result"]
            .as_object()
            .is_some_and(|object| object.is_empty()));
    }

    #[test]
    fn the_version_check_rejects_a_wrong_api_major() {
        // Version path: the server speaks 1.2.3; a request bound to another
        // major is refused before any handler runs.
        let gateway = crate::default_gateway();
        let response = gateway.dispatch(ukma_contracts::RequestEnvelope {
            api_version: "2.0.0".into(),
            operation: "system.capabilities".into(),
            context: ukma_contracts::RequestContext {
                request_id: "t".into(),
                trace_id: "t".into(),
                scope: ukma_contracts::ScopeRef {
                    tenant_id: "t".into(),
                    project_id: "p".into(),
                    repo_id: None,
                    workspace_id: None,
                    agent_id: None,
                    session_id: None,
                },
                principal_id: "p".into(),
                scopes: vec!["system:read".into()],
                deadline_at: None,
                consistency: ukma_contracts::Consistency::Eventual,
                required_generation: None,
                snapshot_id: None,
                idempotency_key: Some("k".into()),
                expected_etag: Some("\"e\"".into()),
                delegation_token: None,
            },
            input: Value::Null,
        });
        assert_eq!(response.error.as_ref().unwrap().code, "INVALID_SCHEMA");
    }
}
