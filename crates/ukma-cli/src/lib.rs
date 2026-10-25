//! ukma — command-line client over the public API (§13, §39.2, P8-WP4).
//!
//! The CLI is a thin projection of the registry: it lists tools and routes from
//! [`ukma_mcp::manifest`], and it dispatches through the same gateway the server
//! uses. It never re-declares an operation, a scope or an error code, and it never
//! talks to a database directly (ADR-021).
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md` §13, §39.2.

use std::collections::BTreeMap;

use serde_json::Value;
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ScopeRef};
use ukma_mcp::{manifest, rest, Gateway};

/// Everything that can go wrong while running a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// The command line itself was wrong.
    Usage(String),
    /// The operation is not in the registry.
    UnknownOperation(String),
    /// The JSON body could not be parsed.
    BadJson(String),
    /// The gateway answered with an error envelope.
    Api { code: String, message: String },
    /// No handler answered for the operation.
    Unhandled(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(detail) => write!(f, "usage: {detail}"),
            CliError::UnknownOperation(operation) => {
                write!(f, "unknown operation `{operation}`")
            }
            CliError::BadJson(detail) => write!(f, "body is not JSON: {detail}"),
            CliError::Api { code, message } => write!(f, "{code}: {message}"),
            CliError::Unhandled(operation) => write!(f, "no handler for `{operation}`"),
        }
    }
}

impl std::error::Error for CliError {}

/// Exit code for a failure.
pub fn exit_code(error: &CliError) -> i32 {
    match error {
        CliError::Usage(_) => 2,
        CliError::UnknownOperation(_) => 2,
        CliError::BadJson(_) => 2,
        CliError::Unhandled(_) => 3,
        CliError::Api { .. } => 1,
    }
}

/// Options that shape the request context.
#[derive(Debug, Clone)]
pub struct Options {
    /// Tenant id.
    pub tenant_id: String,
    /// Project id.
    pub project_id: String,
    /// Authenticated principal.
    pub principal_id: String,
    /// Scopes the caller expects.
    pub scopes: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tenant_id: "tenant-dev".into(),
            project_id: "project-dev".into(),
            principal_id: "principal-dev".into(),
            scopes: Vec::new(),
        }
    }
}

impl Options {
    /// Request context for one call, with the operation's scopes as expectations.
    pub fn context(&self, operation: &str) -> RequestContext {
        let scopes = if self.scopes.is_empty() {
            manifest::tool_for_operation(operation)
                .map(|tool| {
                    tool.required_scopes
                        .iter()
                        .map(|scope| (*scope).to_string())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.scopes.clone()
        };
        RequestContext {
            request_id: format!("cli-{operation}"),
            trace_id: "trace-cli".into(),
            scope: ScopeRef {
                tenant_id: self.tenant_id.clone(),
                project_id: self.project_id.clone(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: self.principal_id.clone(),
            scopes,
            deadline_at: None,
            consistency: Consistency::Eventual,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: Some(format!("cli-{operation}")),
            expected_etag: Some("\"cli-etag\"".into()),
            delegation_token: None,
        }
    }
}

/// Usage text.
pub fn usage() -> String {
    [
        "ukma — UKMA command-line client (spec 1.2.3)",
        "",
        "  ukma health                              health payload",
        "  ukma capabilities                        system.capabilities",
        "  ukma tools [--domain <domain>]           registered MCP tools",
        "  ukma routes                              REST routes",
        "  ukma describe <operation>                one operation from the registry",
        "  ukma manifest                            tool-manifest.json",
        "  ukma call <operation> <json> [options]   dispatch one operation",
        "",
        "options: --tenant <id> --project <id> --principal <id> --scope <scope>",
    ]
    .join("\n")
}

/// Run one command line and return the text to print.
pub fn run(gateway: &Gateway, args: &[String]) -> Result<String, CliError> {
    let mut positional: Vec<String> = Vec::new();
    let mut options = Options::default();
    let mut domain: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--tenant" => {
                options.tenant_id = take(args, &mut index, "--tenant")?;
            }
            "--project" => {
                options.project_id = take(args, &mut index, "--project")?;
            }
            "--principal" => {
                options.principal_id = take(args, &mut index, "--principal")?;
            }
            "--scope" => {
                options.scopes.push(take(args, &mut index, "--scope")?);
            }
            "--domain" => {
                domain = Some(take(args, &mut index, "--domain")?);
            }
            "--help" | "-h" => return Ok(usage()),
            other => positional.push(other.to_string()),
        }
        index += 1;
    }

    let Some(command) = positional.first().cloned() else {
        return Ok(usage());
    };
    match command.as_str() {
        "health" => Ok(rest::health_json()),
        "manifest" => Ok(manifest::manifest_json()),
        "capabilities" => Ok(dispatch(
            gateway,
            "system.capabilities",
            Value::Null,
            &options,
        )?),
        "tools" => Ok(tools(domain.as_deref())),
        "routes" => Ok(routes()),
        "describe" => {
            let operation = positional
                .get(1)
                .cloned()
                .ok_or_else(|| CliError::Usage("describe needs an operation id".to_string()))?;
            if manifest::tool_for_operation(&operation).is_none() {
                return Err(CliError::UnknownOperation(operation));
            }
            Ok(dispatch(
                gateway,
                "system.get_operation",
                serde_json::json!({ "operation_id": operation }),
                &options,
            )?)
        }
        "call" => {
            let operation = positional
                .get(1)
                .cloned()
                .ok_or_else(|| CliError::Usage("call needs an operation id".to_string()))?;
            let body = positional
                .get(2)
                .cloned()
                .unwrap_or_else(|| "{}".to_string());
            let input: Value = serde_json::from_str(&body)
                .map_err(|error| CliError::BadJson(error.to_string()))?;
            dispatch(gateway, &operation, input, &options)
        }
        other => Err(CliError::Usage(format!(
            "unknown command `{other}`; try ukma --help"
        ))),
    }
}

fn take(args: &[String], index: &mut usize, flag: &str) -> Result<String, CliError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))
}

fn tools(domain: Option<&str>) -> String {
    let mut by_domain: BTreeMap<&str, usize> = BTreeMap::new();
    let mut lines = Vec::new();
    for tool in manifest::tools() {
        *by_domain.entry(tool.domain).or_default() += 1;
        if let Some(wanted) = domain {
            if tool.domain != wanted {
                continue;
            }
        }
        lines.push(format!(
            "{:<44} {:<32} {}",
            tool.name, tool.operation_id, tool.rest
        ));
    }
    let summary: Vec<String> = by_domain
        .iter()
        .map(|(domain, count)| format!("{domain}={count}"))
        .collect();
    lines.push(format!(
        "total={} ({})",
        manifest::tools().len(),
        summary.join(", ")
    ));
    lines.join("\n")
}

fn routes() -> String {
    let mut lines: Vec<String> = manifest::tools()
        .into_iter()
        .map(|tool| format!("{:<48} {}", tool.rest, tool.operation_id))
        .collect();
    lines.push(format!("{:<48} {}", "GET /api/v1/health", "system.health"));
    lines.push(format!(
        "{:<48} {}",
        "GET /api/v1/jobs/{job_id}", "system.get_job"
    ));
    lines.join("\n")
}

fn dispatch(
    gateway: &Gateway,
    operation: &str,
    input: Value,
    options: &Options,
) -> Result<String, CliError> {
    if manifest::tool_for_operation(operation).is_none() {
        return Err(CliError::UnknownOperation(operation.to_string()));
    }
    if gateway.unhandled().contains(&operation) {
        return Err(CliError::Unhandled(operation.to_string()));
    }
    let response = gateway.dispatch(RequestEnvelope {
        api_version: "1.2.3".into(),
        operation: operation.to_string(),
        context: options.context(operation),
        input,
    });
    match response.error {
        Some(error) => Err(CliError::Api {
            code: error.code,
            message: error.safe_message,
        }),
        None => Ok(serde_json::to_string_pretty(&response).unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_codes_separate_usage_from_api_failures() {
        assert_eq!(exit_code(&CliError::Usage("x".into())), 2);
        assert_eq!(
            exit_code(&CliError::Api {
                code: "NOT_FOUND".into(),
                message: "x".into()
            }),
            1
        );
        assert_eq!(exit_code(&CliError::Unhandled("x".into())), 3);
    }

    #[test]
    fn options_default_to_the_operation_scopes() {
        let context = Options::default().context("memory.store_episode");
        assert_eq!(context.scopes, vec!["memory:write".to_string()]);
        assert_eq!(context.scope.tenant_id, "tenant-dev");
    }
}
