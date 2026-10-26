//! LSP stdio surface: newline-delimited JSON-RPC 2.0 over stdin/stdout.
//!
//! This is the `--lsp` mode of the `ukma` binary — the same gateway the HTTP
//! server, the CLI and the MCP stdio use, on the editor LSP transport.
//! Nothing here re-declares an operation, a scope or an error code: the
//! custom request set is projected from the manifest (itself a projection of
//! the registry) and every `ukma/<operation_id>` request dispatches through
//! the gateway, so the 37-code closed set stays enforced in exactly one
//! place and the `system.metrics` counters see LSP traffic the same way they
//! see MCP traffic.
//!
//! Lifecycle: `initialize` (capabilities + serverInfo) → `initialized`
//! notification → custom requests → `shutdown` (requests after it are
//! rejected) → `exit` (terminates the session).

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::gateway::Gateway;
use crate::manifest;
use crate::stdio::dev_context;
use ukma_contracts::{Consistency, RequestEnvelope};

/// The LSP specification revision this server speaks.
pub const LSP_SPEC_VERSION: &str = "3.16";
/// The server name reported to the editor host.
pub const SERVER_NAME: &str = "ukma";
/// The envelope version the server speaks.
pub const API_VERSION: &str = "1.2.3";
/// The custom request method prefix.
pub const CUSTOM_REQUEST_PREFIX: &str = "ukma/";

/// An LSP session bound to one gateway.
pub struct LspSession<'a> {
    gateway: &'a Gateway,
    shutdown_received: bool,
    exit_received: bool,
}

impl<'a> LspSession<'a> {
    /// Bind a session to a gateway.
    pub fn new(gateway: &'a Gateway) -> Self {
        Self {
            gateway,
            shutdown_received: false,
            exit_received: false,
        }
    }

    /// Whether the host sent `exit`; the run loop stops after this line.
    pub fn exited(&self) -> bool {
        self.exit_received
    }

    /// Handle one stream line (already trimmed). Returns the response to
    /// write, or `None` when the line is a notification (which carries no
    /// reply).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let request: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return Some(self.error(&Value::Null, -32700, "parse error".to_string())),
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => Some(self.ok(&id, Self::initialize_result())),
            // Notifications carry no response.
            "initialized" => None,
            "shutdown" => {
                self.shutdown_received = true;
                Some(self.ok(&id, Value::Object(Default::default())))
            }
            "exit" => {
                self.exit_received = true;
                None
            }
            other if other.starts_with(CUSTOM_REQUEST_PREFIX) => {
                if self.shutdown_received {
                    return Some(self.error(&id, -32600, "server is shutting down".to_string()));
                }
                let operation_id = &other[CUSTOM_REQUEST_PREFIX.len()..];
                self.custom_request(&id, operation_id, &request)
            }
            other => Some(self.error(&id, -32601, format!("method not found: `{other}`"))),
        }
    }

    /// The `initialize` result: serverInfo plus the custom request set
    /// projected from the manifest, one `ukma/<operation_id>` entry per
    /// registered operation.
    fn initialize_result() -> Value {
        let value_set: Vec<String> = manifest::tools()
            .iter()
            .map(|tool| format!("{}{}", CUSTOM_REQUEST_PREFIX, tool.operation_id))
            .collect();
        json!({
            "serverInfo": { "name": SERVER_NAME, "version": API_VERSION },
            "capabilities": {
                "customRequests": { "valueSet": value_set }
            }
        })
    }

    /// `ukma/<operation_id>`: params = `{ "input": <operation input>,
    /// "context": <optional overrides> }`, dispatched through the gateway
    /// with the local-trusted context (the same decision the CLI and MCP
    /// make). The host may narrow what the envelope expects — including
    /// granting no scopes at all, in which case the gateway answers
    /// `FORBIDDEN` on the shared authorization path.
    fn custom_request(&self, id: &Value, operation_id: &str, request: &Value) -> Option<String> {
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        // Protocol level: the operation must be in the registry; otherwise
        // no operation (and no registry error code) applies.
        let Some(tool) = manifest::tool_for_operation(operation_id) else {
            return Some(self.error(
                id,
                -32602,
                format!("invalid params: unknown operation `{operation_id}`"),
            ));
        };
        // Parameter level: `input` is a required JSON value (it may be `null`
        // or `{}` for empty-payload operations).
        let Some(input) = params.get("input") else {
            return Some(self.error(
                id,
                -32602,
                "invalid params: `input` is required".to_string(),
            ));
        };
        let mut context = dev_context("lsp", tool.operation_id, tool.required_scopes);
        if let Some(overrides) = params.get("context") {
            apply_context_overrides(&mut context, overrides);
        }
        let envelope = RequestEnvelope {
            api_version: API_VERSION.into(),
            operation: tool.operation_id.to_string(),
            context,
            input: input.clone(),
        };
        let response = self.gateway.dispatch(envelope);
        // The protocol succeeded; the operation may not have. The envelope
        // carries the closed-set error code, so the host sees it in-band.
        let envelope = serde_json::to_value(&response).unwrap_or(Value::Null);
        Some(self.ok(id, envelope))
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
}

/// Apply the host's optional context overrides to the local-trusted context.
fn apply_context_overrides(context: &mut ukma_contracts::RequestContext, overrides: &Value) {
    if let Some(tenant_id) = overrides.get("tenant_id").and_then(Value::as_str) {
        context.scope.tenant_id = tenant_id.to_string();
    }
    if let Some(project_id) = overrides.get("project_id").and_then(Value::as_str) {
        context.scope.project_id = project_id.to_string();
    }
    if let Some(principal_id) = overrides.get("principal_id").and_then(Value::as_str) {
        context.principal_id = principal_id.to_string();
    }
    if let Some(scopes) = overrides.get("scopes").and_then(Value::as_array) {
        context.scopes = scopes
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    if let Some(consistency) = overrides.get("consistency").and_then(Value::as_str) {
        context.consistency = match consistency {
            "read-your-writes" | "strong" => Consistency::ReadYourWrites,
            "snapshot" => Consistency::Snapshot,
            "strict-generation" => Consistency::StrictGeneration,
            _ => Consistency::Eventual,
        };
    }
}

/// Run the LSP stdio loop. Returns the process exit code.
pub fn run_lsp(gateway: &Gateway) -> i32 {
    let mut session = LspSession::new(gateway);
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
        if session.exited() {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::default_gateway;

    /// Run one request line against a fresh session.
    fn call(line: &str) -> Option<String> {
        let gateway = default_gateway();
        let mut session = LspSession::new(&gateway);
        session.handle_line(line)
    }

    fn result_of(response: &str) -> Value {
        let value: Value = serde_json::from_str(response).unwrap();
        value["result"].clone()
    }

    fn error_code_of(response: &str) -> i64 {
        let value: Value = serde_json::from_str(response).unwrap();
        value["error"]["code"].as_i64().unwrap()
    }

    #[test]
    fn initialize_reports_server_info_and_the_custom_request_set() {
        let response = call(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap();
        let result = result_of(&response);
        assert_eq!(result["serverInfo"]["name"], "ukma");
        assert_eq!(result["serverInfo"]["version"], API_VERSION);
        let value_set = result["capabilities"]["customRequests"]["valueSet"]
            .as_array()
            .unwrap();
        let names: Vec<String> = value_set
            .iter()
            .map(|entry| entry.as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), manifest::tools().len());
        assert!(names.contains(&"ukma/system.metrics".to_string()));
        assert!(names.contains(&"ukma/code.traverse".to_string()));
        assert!(names.contains(&"ukma/document.remove_source".to_string()));
    }

    #[test]
    fn a_custom_request_dispatches_through_the_gateway_and_is_counted() {
        let gateway = default_gateway();
        let mut session = LspSession::new(&gateway);
        let line =
            r#"{"jsonrpc":"2.0","id":2,"method":"ukma/system.metrics","params":{"input":{}}}"#;
        // The first call observes the counters before itself (self-observation
        // semantics: the in-flight call is recorded after the handler runs).
        let first_line = session.handle_line(line).unwrap();
        let first = result_of(&first_line);
        assert_eq!(first["status"], "ok");
        assert_eq!(first["data"]["total_success"], 0);
        // The second call sees the first one counted.
        let second_line = session.handle_line(line).unwrap();
        let second = result_of(&second_line);
        assert_eq!(second["data"]["total_success"], 1);
        // The same counters MCP sees: the LSP calls are recorded.
        let recorded = gateway
            .metrics()
            .snapshot()
            .into_iter()
            .find(|entry| entry.operation_id == "system.metrics");
        assert_eq!(recorded.map(|entry| entry.success), Some(2));
    }

    #[test]
    fn an_unknown_method_is_a_method_not_found_error() {
        let response = call(r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/foo"}"#).unwrap();
        assert_eq!(error_code_of(&response), -32601);
    }

    #[test]
    fn an_unknown_operation_is_an_invalid_params_error() {
        let response =
            call(r#"{"jsonrpc":"2.0","id":4,"method":"ukma/code.bogus","params":{"input":{}}}"#)
                .unwrap();
        assert_eq!(error_code_of(&response), -32602);
    }

    #[test]
    fn a_missing_input_is_an_invalid_params_error() {
        let response =
            call(r#"{"jsonrpc":"2.0","id":5,"method":"ukma/system.metrics","params":{}}"#).unwrap();
        assert_eq!(error_code_of(&response), -32602);
    }

    #[test]
    fn a_malformed_line_is_a_parse_error() {
        let response = call("this is not json").unwrap();
        assert_eq!(error_code_of(&response), -32700);
    }

    #[test]
    fn notifications_carry_no_response() {
        let response = call(r#"{"jsonrpc":"2.0","method":"initialized"}"#);
        assert!(response.is_none());
    }

    #[test]
    fn shutdown_rejects_requests_until_exit() {
        let gateway = default_gateway();
        let mut session = LspSession::new(&gateway);
        let response = session
            .handle_line(r#"{"jsonrpc":"2.0","id":6,"method":"shutdown"}"#)
            .unwrap();
        assert_eq!(result_of(&response), json!({}));
        let rejected = session
            .handle_line(
                r#"{"jsonrpc":"2.0","id":7,"method":"ukma/system.metrics","params":{"input":{}}}"#,
            )
            .unwrap();
        assert_eq!(error_code_of(&rejected), -32600);
        assert!(session
            .handle_line(r#"{"jsonrpc":"2.0","method":"exit"}"#)
            .is_none());
        assert!(session.exited());
    }

    #[test]
    fn the_host_can_narrow_the_granted_scopes_and_get_forbidden() {
        let response = call(
            r#"{"jsonrpc":"2.0","id":8,"method":"ukma/system.metrics","params":{"input":{},"context":{"scopes":[]}}}"#,
        )
        .unwrap();
        let result = result_of(&response);
        assert_eq!(result["status"], "error");
        assert_eq!(result["error"]["code"], "FORBIDDEN");
    }
}
