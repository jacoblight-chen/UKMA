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

/// The embedding port plus selected profile, and the effective backend mode
/// (CodeGraph `--embedding-model` parity). Factored out to keep
/// `ParsedArgs::embedding_backend`'s signature clippy `type_complexity`-clean.
pub type EmbeddingBackend = (
    Option<(std::sync::Arc<dyn ukma_mcp::EmbeddingPort>, String)>,
    ukma_mcp::BackendMode,
);

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

/// One parsed command line.
#[derive(Debug, Clone)]
pub struct Parsed {
    /// Positional commands (health, call, ...).
    pub positional: Vec<String>,
    /// Context options.
    pub options: Options,
    /// The tool to run by name or operation id (`--run-tool`).
    pub run_tool: Option<String>,
    /// The JSON arguments for `--run-tool`.
    pub tool_args: Option<String>,
    /// Whether to serve the MCP stdio protocol instead of printing.
    pub mcp: bool,
    /// The directory to index as a `@cwd` revision (`--index-cwd`).
    pub index_cwd: Option<String>,
    /// Additional workspace roots to index and merge (`--workspace`, repeatable).
    pub workspaces: Vec<String>,
    /// Path globs to skip while indexing (`--exclude`, repeatable).
    pub excludes: Vec<String>,
    /// Cap on the number of files to index (`--max-files`).
    pub max_files: Option<usize>,
    /// The tool-surface profile (`--profile`): `all`, `core`, `graph`,
    /// `memory`, `security`. Filters the MCP tools exposed by `--mcp`.
    pub profile: Option<String>,
    /// Re-index on change by polling mtimes (`--watch`).
    pub watch: bool,
    /// Enable online OSV dependency lookups (`--online`). A network operation;
    /// the offline manifest is the fallback when the build has no `osv`
    /// feature or the network is unavailable.
    pub online: bool,
    /// Split camelCase identifiers (`--split-identifiers` / `--no-split-identifiers`).
    pub split_identifiers: Option<bool>,
    /// Embed the symbol body (`--full-body-embedding` / `--no-full-body-embedding`).
    pub full_body: Option<bool>,
    /// Structural-only ranking (`--graph-only`).
    pub graph_only: bool,
    /// The selectable embedding model (`--embedding-model`): `bge-small`,
    /// `jina-code-v2`, `granite-97m`, `static`.
    pub embedding_model: Option<String>,
    /// The domain filter for `tools`.
    pub domain: Option<String>,
    /// Whether the caller asked for the usage text.
    pub help: bool,
}

/// Parse the command line.
pub fn parse(args: &[String]) -> Result<Parsed, CliError> {
    let mut parsed = Parsed {
        positional: Vec::new(),
        options: Options::default(),
        run_tool: None,
        tool_args: None,
        mcp: false,
        index_cwd: None,
        workspaces: Vec::new(),
        excludes: Vec::new(),
        max_files: None,
        profile: None,
        watch: false,
        online: false,
        split_identifiers: None,
        full_body: None,
        graph_only: false,
        embedding_model: None,
        domain: None,
        help: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--tenant" => {
                parsed.options.tenant_id = take(args, &mut index, "--tenant")?;
            }
            "--project" => {
                parsed.options.project_id = take(args, &mut index, "--project")?;
            }
            "--principal" => {
                parsed.options.principal_id = take(args, &mut index, "--principal")?;
            }
            "--scope" => {
                parsed
                    .options
                    .scopes
                    .push(take(args, &mut index, "--scope")?);
            }
            "--run-tool" => {
                let tool = take(args, &mut index, "--run-tool")?;
                if parsed.run_tool.is_some() {
                    return Err(CliError::Usage("only one --run-tool is allowed".into()));
                }
                parsed.run_tool = Some(tool);
            }
            "--tool-args" => {
                let tool_args = take(args, &mut index, "--tool-args")?;
                if parsed.tool_args.is_some() {
                    return Err(CliError::Usage("only one --tool-args is allowed".into()));
                }
                parsed.tool_args = Some(tool_args);
            }
            "--mcp" => {
                parsed.mcp = true;
            }
            "--index-cwd" => {
                let path = take(args, &mut index, "--index-cwd")?;
                if parsed.index_cwd.is_some() {
                    return Err(CliError::Usage("only one --index-cwd is allowed".into()));
                }
                parsed.index_cwd = Some(path);
            }
            "--domain" => {
                let domain = take(args, &mut index, "--domain")?;
                if parsed.domain.is_some() {
                    return Err(CliError::Usage("only one --domain is allowed".into()));
                }
                parsed.domain = Some(domain);
            }
            "--workspace" => {
                parsed
                    .workspaces
                    .push(take(args, &mut index, "--workspace")?);
            }
            "--exclude" => {
                parsed.excludes.push(take(args, &mut index, "--exclude")?);
            }
            "--max-files" => {
                let raw = take(args, &mut index, "--max-files")?;
                let max_files = raw.parse::<usize>().map_err(|_| {
                    CliError::Usage("--max-files must be a non-negative integer".into())
                })?;
                if parsed.max_files.is_some() {
                    return Err(CliError::Usage("only one --max-files is allowed".into()));
                }
                parsed.max_files = Some(max_files);
            }
            "--profile" => {
                let profile = take(args, &mut index, "--profile")?;
                if !ukma_mcp::ToolProfile::NAMES.contains(&profile.as_str()) {
                    return Err(CliError::Usage(
                        "--profile must be one of all, core, graph, memory, security".into(),
                    ));
                }
                if parsed.profile.is_some() {
                    return Err(CliError::Usage("only one --profile is allowed".into()));
                }
                parsed.profile = Some(profile);
            }
            "--watch" => {
                parsed.watch = true;
            }
            "--online" => {
                parsed.online = true;
            }
            "--split-identifiers" => {
                parsed.split_identifiers = Some(true);
            }
            "--no-split-identifiers" => {
                parsed.split_identifiers = Some(false);
            }
            "--full-body-embedding" => {
                parsed.full_body = Some(true);
            }
            "--no-full-body-embedding" => {
                parsed.full_body = Some(false);
            }
            "--graph-only" => {
                parsed.graph_only = true;
            }
            "--embedding-model" => {
                let model = take(args, &mut index, "--embedding-model")?;
                if ukma_mcp::EmbeddingModel::from_name(&model).is_none() {
                    return Err(CliError::Usage(
                        "--embedding-model must be one of bge-small, jina-code-v2, granite-97m, static"
                            .into(),
                    ));
                }
                if parsed.embedding_model.is_some() {
                    return Err(CliError::Usage(
                        "only one --embedding-model is allowed".into(),
                    ));
                }
                parsed.embedding_model = Some(model);
            }
            "--help" | "-h" => {
                parsed.help = true;
            }
            other => parsed.positional.push(other.to_string()),
        }
        index += 1;
    }
    Ok(parsed)
}

impl Parsed {
    /// The scanner options implied by the index flags: the explicit
    /// `--exclude` globs (the built-in build/binary/secret exclusion set is
    /// always applied by the scanner) plus the `--max-files` cap.
    pub fn index_options(&self) -> ukma_mcp::index::IndexOptions {
        ukma_mcp::index::IndexOptions {
            excludes: self.excludes.clone(),
            max_files: self.max_files,
        }
    }

    /// The tool-surface profile implied by `--profile` (default: `all`).
    pub fn tool_profile(&self) -> ukma_mcp::ToolProfile {
        self.profile
            .as_deref()
            .and_then(ukma_mcp::ToolProfile::from_name)
            .unwrap_or_default()
    }

    /// The search / embedding policy implied by the embedding flags
    /// (defaults: split `true`, body `false`, graph-only `false`).
    pub fn search_policy(&self) -> ukma_mcp::SearchPolicy {
        let mut policy = ukma_mcp::SearchPolicy::default();
        if let Some(v) = self.split_identifiers {
            policy.split_identifiers = v;
        }
        if let Some(v) = self.full_body {
            policy.full_body = v;
        }
        policy.graph_only = self.graph_only;
        policy
    }

    /// The effective mode after the RAM gate (CodeGraph
    /// `CODEGRAPH_SKIP_MEMORY_CHECK` parity). Only an ONNX model whose gate
    /// fails degrades to `GraphOnly`; `static` and the default never do.
    pub fn backend_mode(&self) -> ukma_mcp::BackendMode {
        if let Some(model) = self.embedding_model.as_deref() {
            if let Some(m) = ukma_mcp::EmbeddingModel::from_name(model) {
                if m.requires_onnx() {
                    let probe = ukma_mcp::SystemMemoryProbe::new();
                    let available = ukma_mcp::MemoryProbe::available_bytes(&probe);
                    if !ukma_mcp::model_gate_passes(available) {
                        return ukma_mcp::BackendMode::GraphOnly;
                    }
                }
            }
        }
        ukma_mcp::BackendMode::WithModel
    }

    /// The embedding port + selected profile and the effective mode
    /// (CodeGraph `--embedding-model` parity). `None` keeps the static
    /// channel; an unavailable ONNX profile degrades the semantic channel to
    /// the static fallback at query time.
    pub fn embedding_backend(&self) -> EmbeddingBackend {
        if let Some(model) = self.embedding_model.as_deref() {
            if let Some(m) = ukma_mcp::EmbeddingModel::from_name(model) {
                // Real ONNX Runtime (feature `onnx`): use the provisioned model
                // when `UKMA_ONNX_MODEL_PATH` is set and the model loads.
                #[cfg(feature = "onnx")]
                if let Some(path) = std::env::var("UKMA_ONNX_MODEL_PATH")
                    .ok()
                    .filter(|s| !s.is_empty())
                {
                    if let Ok(real) =
                        ukma_mcp::RealOnnxEmbedder::load(&path, m.profile_id(), m.dimension())
                    {
                        return (
                            Some((std::sync::Arc::new(real), m.profile_id().to_string())),
                            self.backend_mode(),
                        );
                    }
                }
                // Fallback: deterministic/static embedder; the ONNX models degrade
                // to the static channel when the runtime is absent.
                let embedder: std::sync::Arc<dyn ukma_mcp::EmbeddingPort> =
                    std::sync::Arc::new(ukma_mcp::OnnxEmbedder::new());
                return (
                    Some((embedder, m.profile_id().to_string())),
                    self.backend_mode(),
                );
            }
        }
        (None, self.backend_mode())
    }

    /// The OSV online vulnerability source when `--online` is set and the
    /// `osv` feature is enabled; `None` otherwise (offline manifest).
    #[cfg(feature = "osv")]
    pub fn vulnerability_source(
        &self,
    ) -> Option<std::sync::Arc<dyn ukma_mcp::VulnerabilitySource>> {
        if self.online {
            Some(std::sync::Arc::new(
                ukma_mcp::OsVulnerabilitySource::default(),
            ))
        } else {
            None
        }
    }

    /// Without the `osv` feature, online lookups are unavailable; always
    /// offline (the manifest is reported).
    #[cfg(not(feature = "osv"))]
    pub fn vulnerability_source(
        &self,
    ) -> Option<std::sync::Arc<dyn ukma_mcp::VulnerabilitySource>> {
        let _ = self.online;
        None
    }
}

/// Resolve a `--run-tool` name: either an MCP tool name
/// (`ukma_code_find_symbols_v1`) or a canonical operation id
/// (`code.find_symbols`).
pub fn resolve_tool(name: &str) -> Option<String> {
    if manifest::tool_by_name(name).is_some() {
        manifest::tool_by_name(name).map(|tool| tool.operation_id.to_string())
    } else {
        manifest::tool_for_operation(name).map(|tool| tool.operation_id.to_string())
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
        "  ukma --run-tool <name> [--tool-args json]  run one tool and exit",
        "  ukma --mcp [--index-cwd <path>]          serve the MCP stdio protocol",
        "",
        "options: --tenant <id> --project <id> --principal <id> --scope <scope>",
        "",
        "indexing (CodeGraph-style):",
        "  --index-cwd <path>        index the working tree as revision `@cwd`",
        "  --workspace <path>        index an extra root (repeatable, merged)",
        "  --exclude <glob>          skip matching paths (repeatable)",
        "  --max-files <n>           cap on files indexed",
        "  --watch                  re-index on change (mtime polling)",
        "  --profile <all|core|graph|memory|security>",
        "                            filter the MCP tool surface (with --mcp)",
        "  --split-identifiers / --no-split-identifiers",
        "                            split camelCase ids (default: split)",
        "  --full-body-embedding / --no-full-body-embedding",
        "                            embed the symbol body (default: off)",
        "  --graph-only              structural (keyword-only) ranking",
        "  --embedding-model <bge-small|jina-code-v2|granite-97m|static>",
        "                            select the embedding model (default: static)",
        "                            (ONNX models degrade to the static channel",
        "                             when the runtime is absent; CODEGRAPH_SKIP_",
        "                             MEMORY_CHECK bypasses the ~1.5GB RAM gate)",
        "",
        "With --index-cwd/--workspace, the scanned tree becomes revision `@cwd`:",
        "use \"commit_sha\": \"@cwd\" in a --tool-args revision.",
    ]
    .join("\n")
}

/// Run one command line and return the text to print.
///
/// `cwd_index` is the indexed working directory, when `--index-cwd` was
/// given (the gateway must have been built with the same index).
pub fn run(
    gateway: &Gateway,
    parsed: &Parsed,
    cwd_index: Option<&ukma_mcp::CwdIndex>,
) -> Result<String, CliError> {
    let positional = parsed.positional.clone();
    let options = parsed.options.clone();
    let domain = parsed.domain.clone();

    // --run-tool: run one tool and exit (CodeGraph `--run-tool` style).
    if let Some(name) = &parsed.run_tool {
        let operation = resolve_tool(name).ok_or_else(|| {
            CliError::UnknownOperation(format!(
                "`{name}` is neither a tool name nor an operation id"
            ))
        })?;
        let body = parsed.tool_args.clone().unwrap_or_else(|| "{}".to_string());
        let mut input: Value =
            serde_json::from_str(&body).map_err(|error| CliError::BadJson(error.to_string()))?;
        if let Some(index) = cwd_index {
            ukma_mcp::resolve_cwd_alias(&mut input, index);
        }
        return dispatch(gateway, &operation, input, &options);
    }

    if parsed.help {
        return Ok(usage());
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

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

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

    #[test]
    fn parse_accepts_the_new_flags() {
        let parsed = parse(&args(&[
            "--run-tool",
            "system.capabilities",
            "--tool-args",
            "{}",
            "--mcp",
            "--index-cwd",
            ".",
            "--domain",
            "memory",
        ]))
        .expect("the flags are valid");
        assert_eq!(parsed.run_tool.as_deref(), Some("system.capabilities"));
        assert_eq!(parsed.tool_args.as_deref(), Some("{}"));
        assert!(parsed.mcp);
        assert_eq!(parsed.index_cwd.as_deref(), Some("."));
        assert_eq!(parsed.domain.as_deref(), Some("memory"));
    }

    #[test]
    fn parse_rejects_a_duplicate_run_tool() {
        let error = parse(&args(&["--run-tool", "a", "--run-tool", "b"]))
            .expect_err("duplicates are a usage error");
        assert_eq!(exit_code(&error), 2);
    }

    #[test]
    fn parse_rejects_a_flag_without_a_value() {
        let error = parse(&args(&["--tool-args"])).expect_err("a flag needs a value");
        assert!(matches!(error, CliError::Usage(_)));
    }

    #[test]
    fn parse_accepts_the_indexing_flags() {
        let parsed = parse(&args(&[
            "--index-cwd",
            ".",
            "--workspace",
            "other",
            "--workspace",
            "third",
            "--profile",
            "core",
            "--exclude",
            "**/*.md",
            "--exclude",
            "vendor/**",
            "--max-files",
            "100",
            "--watch",
        ]))
        .expect("the indexing flags are valid");
        assert_eq!(parsed.index_cwd.as_deref(), Some("."));
        assert_eq!(
            parsed.workspaces,
            vec!["other".to_string(), "third".to_string()]
        );
        assert_eq!(parsed.profile.as_deref(), Some("core"));
        assert_eq!(
            parsed.excludes,
            vec!["**/*.md".to_string(), "vendor/**".to_string()]
        );
        assert_eq!(parsed.max_files, Some(100));
        assert!(parsed.watch);
    }

    #[test]
    fn parse_rejects_an_unknown_profile() {
        let error = parse(&args(&["--profile", "bogus"]))
            .expect_err("only the five profile names are valid");
        assert_eq!(exit_code(&error), 2);
    }

    #[test]
    fn parse_accepts_the_security_profile() {
        let parsed = parse(&args(&["--profile", "security"])).expect("security is valid");
        assert_eq!(parsed.profile.as_deref(), Some("security"));
        assert!(
            matches!(parsed.tool_profile(), ukma_mcp::ToolProfile::Security),
            "the parsed profile maps to ToolProfile::Security"
        );
    }

    #[test]
    fn parse_accepts_the_embedding_flags() {
        let parsed = parse(&args(&[
            "--no-split-identifiers",
            "--full-body-embedding",
            "--graph-only",
        ]))
        .expect("embedding flags are valid");
        assert_eq!(parsed.split_identifiers, Some(false));
        assert_eq!(parsed.full_body, Some(true));
        assert!(parsed.graph_only);
        let policy = parsed.search_policy();
        assert!(!policy.split_identifiers);
        assert!(policy.full_body);
        assert!(policy.graph_only);
    }

    #[test]
    fn parse_accepts_the_embedding_model() {
        let parsed =
            parse(&args(&["--embedding-model", "static"])).expect("static is a valid model");
        assert_eq!(parsed.embedding_model.as_deref(), Some("static"));
        // `static` never requires the ONNX runtime, so the mode is `WithModel`.
        assert!(
            matches!(parsed.backend_mode(), ukma_mcp::BackendMode::WithModel),
            "static degrades to no model (WithModel)"
        );
        let (embedder, mode) = parsed.embedding_backend();
        assert!(embedder.is_some(), "static wires an embedder");
        assert!(
            matches!(mode, ukma_mcp::BackendMode::WithModel),
            "the backend mode is WithModel for static"
        );
    }

    #[test]
    fn parse_rejects_an_unknown_embedding_model() {
        let error = parse(&args(&["--embedding-model", "bogus"]))
            .expect_err("only the four model names are valid");
        assert_eq!(exit_code(&error), 2);
    }

    #[test]
    fn the_search_policy_defaults_preserve_current_behaviour() {
        let parsed = parse(&args(&[])).expect("no flags");
        let policy = parsed.search_policy();
        assert!(policy.split_identifiers, "default splits identifiers");
        assert!(!policy.full_body, "default does not embed the body");
        assert!(!policy.graph_only, "default is not graph-only");
    }

    #[test]
    fn parse_rejects_a_non_integer_max_files() {
        let error = parse(&args(&["--max-files", "abc"])).expect_err("the cap must be an integer");
        assert_eq!(exit_code(&error), 2);
    }

    #[test]
    fn the_profile_no_longer_shapes_excludes() {
        // `--profile` is a tool-surface filter, not an exclude preset: the
        // scanner options now carry only the explicit `--exclude` globs.
        let parsed = parse(&args(&["--profile", "graph", "--exclude", "docs/**"]))
            .expect("profile + exclude");
        let options = parsed.index_options();
        assert_eq!(
            options.excludes,
            vec!["docs/**".to_string()],
            "only the explicit --exclude is carried (no preset globs)"
        );
        assert!(
            matches!(parsed.tool_profile(), ukma_mcp::ToolProfile::Graph),
            "the profile is exposed separately as a tool-surface filter"
        );
    }

    #[test]
    fn resolve_tool_accepts_tool_names_and_operation_ids() {
        assert_eq!(
            resolve_tool("ukma_system_capabilities_v1").as_deref(),
            Some("system.capabilities")
        );
        assert_eq!(
            resolve_tool("memory.search").as_deref(),
            Some("memory.search")
        );
    }

    #[test]
    fn resolve_tool_rejects_unknown_names() {
        assert_eq!(resolve_tool("ukma_nope_nope_v1"), None);
        assert_eq!(resolve_tool("nope.nope"), None);
    }

    #[test]
    fn run_tool_runs_one_tool_and_prints_the_envelope() {
        let gateway = ukma_mcp::default_gateway();
        let parsed = parse(&args(&[
            "--run-tool",
            "ukma_system_capabilities_v1",
            "--tool-args",
            "null",
        ]))
        .expect("a valid command line");
        let output = run(&gateway, &parsed, None).expect("the tool runs");
        assert!(output.contains("capabilities"), "the envelope is printed");
    }

    #[test]
    fn run_tool_with_an_unknown_name_is_an_error() {
        let gateway = ukma_mcp::default_gateway();
        let parsed = parse(&args(&["--run-tool", "ukma_nope_nope_v1"])).unwrap();
        let error = run(&gateway, &parsed, None).expect_err("unknown tool");
        assert_eq!(exit_code(&error), 2);
    }

    #[test]
    fn run_tool_with_bad_json_is_a_bad_json_error() {
        let gateway = ukma_mcp::default_gateway();
        let parsed = parse(&args(&[
            "--run-tool",
            "system.capabilities",
            "--tool-args",
            "{not json",
        ]))
        .unwrap();
        let error = run(&gateway, &parsed, None).expect_err("bad json");
        assert_eq!(exit_code(&error), 2);
        assert!(matches!(error, CliError::BadJson(_)));
    }

    #[test]
    fn run_tool_with_cwd_index_resolves_the_alias_end_to_end() {
        use ukma_mcp::gateway_with_indexed_cwd;

        let root = std::env::temp_dir().join(format!("ukma-cli-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create the tree");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn ukma_hello() -> u32 { 1 }\n",
        )
        .expect("write the source");

        let (gateway, index) = gateway_with_indexed_cwd(&root).expect("the tree indexes");
        assert_eq!(index.commit_sha.len(), 40, "a 40-char hex revision id");
        assert!(index.files >= 1, "the source file was indexed");

        // Build the index for the @cwd revision.
        let analyze = parse(&args(&[
            "--run-tool",
            "code.analyze_repository",
            "--tool-args",
            r#"{"revision":{"repository_id":"repo-1","vcs":"git","commit_sha":"@cwd","tree_sha":null,"dirty_policy":"snapshot"},"parser_profile":"default","include_globs":[],"exclude_globs":[],"asynchronous":true}"#,
        ]))
        .expect("a valid analyze line");
        let output = run(&gateway, &analyze, Some(&index))
            .expect("the analysis runs through the @cwd alias");
        assert!(output.contains("job_id"), "a job handle comes back");

        // Query the symbols through the bare-string @cwd alias.
        let search = parse(&args(&[
            "--run-tool",
            "ukma_code_find_symbols_v1",
            "--tool-args",
            r#"{"repository_id":"repo-1","revision":"@cwd","query":"ukma_hello","kinds":[],"path_glob":null,"language":null,"page":{"cursor":null,"limit":10}}"#,
        ]))
        .expect("a valid search line");
        let output = run(&gateway, &search, Some(&index)).expect("the symbol search runs");
        assert!(
            output.contains("ukma_hello"),
            "the indexed symbol is found through @cwd"
        );

        std::fs::remove_dir_all(&root).expect("clean up");
    }
}
