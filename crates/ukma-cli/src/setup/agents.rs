//! The agent table: which coding agents UKMA can wire, where their MCP
//! configuration lives, and where their rules file goes.
//!
//! The table is data, not code: adding an agent is one entry. Every path is
//! relative to the [`SetupContext`] home or project directory, so a command can
//! never write outside those two roots.

use std::path::PathBuf;

use serde_json::json;

use super::SetupContext;

/// The rules document used for the agents whose rules file is
/// agent-independent.
const COMMON_RULES: &str = include_str!("../../../../agent-rules/common/UKMA.md");

/// The rule files shipped in `agent-rules/<agent>/UKMA.md`.
const CLAUDE_RULES: &str = include_str!("../../../../agent-rules/claude/UKMA.md");
const CURSOR_RULES: &str = include_str!("../../../../agent-rules/cursor/UKMA.md");
const WINDSURF_RULES: &str = include_str!("../../../../agent-rules/windsurf/UKMA.md");
const CODEX_RULES: &str = include_str!("../../../../agent-rules/codex/UKMA.md");
const CLINE_RULES: &str = include_str!("../../../../agent-rules/cline/UKMA.md");

/// VS Code custom-instruction frontmatter for the Copilot rules file (the
/// `.instructions.md` convention requires it).
const COPILOT_RULES_PREFIX: &str = "---\napplyTo: \"**\"\n---\n\n";

/// How an agent stores its MCP servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// A JSON object `{"<container>": {"ukma": {"command": …, "args": […]}}}`.
    JsonMcpServers,
    /// VS Code `.vscode/mcp.json`: `{"servers": {"ukma": {"type": "stdio", …}}}`.
    JsonVsCodeServers,
    /// opencode: `{"mcp": {"ukma": {"type": "local", "command": […], …}}}`.
    JsonOpencode,
    /// A TOML file extended with a managed `[mcp_servers.ukma]` block.
    TomlBlock,
    /// A YAML file whose `mcp_servers:` map gains a `ukma:` entry (Hermes).
    YamlMcpServers,
}

impl ConfigFormat {
    /// The container key holding the servers, when the format has one.
    pub fn container(&self) -> Option<&'static str> {
        match self {
            ConfigFormat::JsonMcpServers => Some("mcpServers"),
            ConfigFormat::JsonVsCodeServers => Some("servers"),
            ConfigFormat::JsonOpencode => Some("mcp"),
            ConfigFormat::TomlBlock => None,
            ConfigFormat::YamlMcpServers => Some("mcp_servers"),
        }
    }
}

/// Where a file lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPath {
    /// Relative to the home directory.
    Home(&'static str),
    /// Relative to the project directory.
    Project(&'static str),
    /// VS Code's platform-specific global-storage directory (Cline).
    ClineStorage,
    /// `$HERMES_HOME/config.yaml`, else `<home>/.hermes/config.yaml`.
    HermesHome,
    /// Antigravity's MCP config: the unified `~/.gemini/config/mcp_config.json`
    /// once Antigravity migrated, else the legacy
    /// `~/.gemini/antigravity/mcp_config.json`.
    AntigravityMcp,
    /// Not configurable over MCP.
    Unsupported,
}

/// Where an agent's rules file lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesPath {
    /// Relative to the home directory.
    Home(&'static str),
    /// Relative to the project directory.
    Project(&'static str),
}

/// One supported coding agent.
#[derive(Debug)]
pub struct Agent {
    /// The `--target` id.
    pub id: &'static str,
    /// The display name.
    pub name: &'static str,
    /// Probes whose existence implies the agent is installed (any-of).
    pub detect: &'static [ConfigPath],
    /// Where the MCP configuration lives.
    pub config: ConfigPath,
    /// How that configuration is shaped.
    pub format: ConfigFormat,
    /// Where the rules file goes, when the agent has one.
    pub rules: Option<RulesPath>,
    /// Text placed before the rules body (frontmatter).
    pub rules_prefix: &'static str,
    /// The embedded rules body.
    pub rules_body: &'static str,
    /// Why an agent is skipped, when it is not configurable over MCP.
    pub note: &'static str,
}

impl Agent {
    /// Whether the MCP server can be written for this agent.
    pub fn is_supported(&self) -> bool {
        !matches!(self.config, ConfigPath::Unsupported)
    }

    /// The resolved configuration path, or `None` when the agent is not
    /// configurable over MCP.
    pub fn config_path(&self, ctx: &SetupContext) -> Option<PathBuf> {
        self.all_config_paths(ctx).into_iter().next()
    }

    /// Every path this agent's UKMA entry may live at.
    ///
    /// Almost every agent has one; Antigravity has two (it migrated its config
    /// path), so `ukma uninstall` sweeps both instead of leaving an entry
    /// behind in the location a different version of the IDE reads.
    pub fn all_config_paths(&self, ctx: &SetupContext) -> Vec<PathBuf> {
        match self.config {
            ConfigPath::Home(rel) => vec![join(&ctx.home, rel)],
            ConfigPath::Project(rel) => vec![join(&ctx.cwd, rel)],
            ConfigPath::ClineStorage => vec![join(&ctx.home, cline_storage_rel())],
            ConfigPath::HermesHome => vec![hermes_config_path(ctx)],
            ConfigPath::AntigravityMcp => {
                let (preferred, other) = antigravity_config_paths(ctx);
                vec![preferred, other]
            }
            ConfigPath::Unsupported => Vec::new(),
        }
    }

    /// The resolved rules path, when the agent has one.
    pub fn rules_path(&self, ctx: &SetupContext) -> Option<PathBuf> {
        match self.rules {
            Some(RulesPath::Home(rel)) => Some(join(&ctx.home, rel)),
            Some(RulesPath::Project(rel)) => Some(join(&ctx.cwd, rel)),
            None => None,
        }
    }

    /// Whether the agent looks installed on this machine.
    pub fn is_detected(&self, ctx: &SetupContext) -> bool {
        self.detect.iter().any(|probe| match probe {
            ConfigPath::Home(rel) => join(&ctx.home, rel).exists(),
            ConfigPath::Project(rel) => join(&ctx.cwd, rel).exists(),
            ConfigPath::ClineStorage => join(&ctx.home, cline_storage_rel()).exists(),
            ConfigPath::HermesHome => {
                let path = hermes_config_path(ctx);
                path.exists() || path.parent().map(std::path::Path::exists).unwrap_or(false)
            }
            ConfigPath::AntigravityMcp => {
                let (preferred, other) = antigravity_config_paths(ctx);
                preferred.exists()
                    || other.exists()
                    // Antigravity creates `~/.gemini/config/.migrated` when it
                    // upgrades, before it writes any MCP config.
                    || join(&ctx.home, ".gemini/config/.migrated").exists()
                    || join(&ctx.home, ".gemini/antigravity").exists()
            }
            ConfigPath::Unsupported => false,
        })
    }
}

/// Join a `/`-separated relative path with the platform separator.
fn join(root: &std::path::Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

fn cline_storage_rel() -> &'static str {
    if cfg!(target_os = "windows") {
        "AppData/Roaming/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"
    } else if cfg!(target_os = "macos") {
        "Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"
    } else {
        ".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"
    }
}

/// `$HERMES_HOME/config.yaml`, falling back to `<home>/.hermes/config.yaml`.
///
/// `HERMES_HOME` wins when it is set: Hermes itself honours it, so writing
/// anywhere else would configure a directory the agent never reads.
pub fn hermes_config_path(ctx: &SetupContext) -> PathBuf {
    let override_root = std::env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    hermes_config_path_from(ctx, override_root.as_deref())
}

/// [`hermes_config_path`] for an explicit override (pure, so tests need no
/// process-wide environment mutation).
pub fn hermes_config_path_from(
    ctx: &SetupContext,
    override_root: Option<&std::path::Path>,
) -> PathBuf {
    match override_root {
        Some(root) => join(root, "config.yaml"),
        None => join(&ctx.home, ".hermes/config.yaml"),
    }
}

/// Antigravity's preferred config path, plus the other one.
///
/// Antigravity migrated from `~/.gemini/antigravity/mcp_config.json` to a
/// unified `~/.gemini/config/mcp_config.json`; the marker file
/// `~/.gemini/config/.migrated` (or the unified file itself) means the migration
/// happened. Both are returned so an uninstall can sweep the one that is not
/// preferred.
pub fn antigravity_config_paths(ctx: &SetupContext) -> (PathBuf, PathBuf) {
    let unified = join(&ctx.home, ".gemini/config/mcp_config.json");
    let legacy = join(&ctx.home, ".gemini/antigravity/mcp_config.json");
    let migrated = join(&ctx.home, ".gemini/config/.migrated").exists() || unified.exists();
    if migrated {
        (unified, legacy)
    } else {
        (legacy, unified)
    }
}

/// Every agent UKMA knows about.
///
/// `jetbrains` is in the table so detection can explain why it is skipped: it is
/// a REST client, not an MCP client.
pub const AGENTS: &[Agent] = &[
    Agent {
        id: "claude-code",
        name: "Claude Code",
        detect: &[
            ConfigPath::Home(".claude"),
            ConfigPath::Home(".claude.json"),
        ],
        config: ConfigPath::Home(".claude.json"),
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".claude/UKMA.md")),
        rules_prefix: "",
        rules_body: CLAUDE_RULES,
        note: "",
    },
    Agent {
        id: "cursor",
        name: "Cursor",
        detect: &[ConfigPath::Home(".cursor")],
        config: ConfigPath::Home(".cursor/mcp.json"),
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".cursor/UKMA.md")),
        rules_prefix: "",
        rules_body: CURSOR_RULES,
        note: "",
    },
    Agent {
        id: "windsurf",
        name: "Windsurf",
        detect: &[ConfigPath::Home(".codeium/windsurf")],
        config: ConfigPath::Home(".codeium/windsurf/mcp_config.json"),
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".windsurf/UKMA.md")),
        rules_prefix: "",
        rules_body: WINDSURF_RULES,
        note: "",
    },
    Agent {
        id: "codex",
        name: "Codex CLI",
        detect: &[ConfigPath::Home(".codex")],
        config: ConfigPath::Home(".codex/config.toml"),
        format: ConfigFormat::TomlBlock,
        rules: Some(RulesPath::Home(".codex/UKMA.md")),
        rules_prefix: "",
        rules_body: CODEX_RULES,
        note: "",
    },
    Agent {
        id: "cline",
        name: "Cline",
        detect: &[ConfigPath::ClineStorage, ConfigPath::Home(".cline")],
        config: ConfigPath::ClineStorage,
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".cline/UKMA.md")),
        rules_prefix: "",
        rules_body: CLINE_RULES,
        note: "",
    },
    Agent {
        id: "copilot-vscode",
        name: "GitHub Copilot (VS Code)",
        detect: &[ConfigPath::Project(".vscode")],
        config: ConfigPath::Project(".vscode/mcp.json"),
        format: ConfigFormat::JsonVsCodeServers,
        rules: Some(RulesPath::Project(
            ".github/instructions/ukma.instructions.md",
        )),
        rules_prefix: COPILOT_RULES_PREFIX,
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "copilot-cli",
        name: "GitHub Copilot CLI",
        detect: &[ConfigPath::Home(".copilot")],
        config: ConfigPath::Home(".copilot/mcp-config.json"),
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".copilot/UKMA.md")),
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "gemini-cli",
        name: "Gemini CLI",
        detect: &[ConfigPath::Home(".gemini")],
        config: ConfigPath::Home(".gemini/settings.json"),
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".gemini/UKMA.md")),
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "opencode",
        name: "opencode",
        detect: &[
            ConfigPath::Home(".config/opencode"),
            ConfigPath::Home(".opencode"),
        ],
        config: ConfigPath::Home(".config/opencode/opencode.json"),
        format: ConfigFormat::JsonOpencode,
        rules: Some(RulesPath::Home(".config/opencode/UKMA.md")),
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "kiro",
        name: "Kiro",
        detect: &[ConfigPath::Home(".kiro")],
        config: ConfigPath::Home(".kiro/settings/mcp.json"),
        format: ConfigFormat::JsonMcpServers,
        rules: Some(RulesPath::Home(".kiro/UKMA.md")),
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "hermes",
        name: "Hermes Agent",
        detect: &[ConfigPath::HermesHome],
        config: ConfigPath::HermesHome,
        format: ConfigFormat::YamlMcpServers,
        // Hermes has no instructions file UKMA can write without guessing: its
        // behaviour comes from `$HERMES_HOME` config, not from a rules document.
        rules: None,
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "antigravity",
        name: "Antigravity IDE",
        detect: &[ConfigPath::AntigravityMcp],
        config: ConfigPath::AntigravityMcp,
        format: ConfigFormat::JsonMcpServers,
        // The IDE shares `~/.gemini/GEMINI.md` with the Gemini CLI; that file
        // belongs to the user, so UKMA does not write it.
        rules: None,
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "",
    },
    Agent {
        id: "jetbrains",
        name: "JetBrains IDEs",
        detect: &[],
        config: ConfigPath::Unsupported,
        format: ConfigFormat::JsonMcpServers,
        rules: None,
        rules_prefix: "",
        rules_body: COMMON_RULES,
        note: "the JetBrains plugin is a REST client, not an MCP client; point it at `ukma-server`",
    },
];

/// Look an agent up by `--target` id.
pub fn find(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|agent| agent.id == id)
}

/// Every known `--target` id, in table order.
pub fn ids() -> Vec<&'static str> {
    AGENTS.iter().map(|agent| agent.id).collect()
}

/// The agents that look installed on this machine.
pub fn detected(ctx: &SetupContext) -> Vec<&'static Agent> {
    AGENTS
        .iter()
        .filter(|agent| agent.is_supported() && agent.is_detected(ctx))
        .collect()
}

/// The server arguments written into every agent configuration.
pub fn server_args(index_root: &str) -> Vec<String> {
    vec![
        "--mcp".to_string(),
        "--index-cwd".to_string(),
        index_root.to_string(),
    ]
}

/// The JSON entry written for one agent, in that agent's shape.
pub fn server_entry(
    format: ConfigFormat,
    ctx: &SetupContext,
    index_root: &str,
) -> serde_json::Value {
    let binary = ctx.binary.display().to_string();
    let args = server_args(index_root);
    match format {
        ConfigFormat::JsonMcpServers | ConfigFormat::TomlBlock | ConfigFormat::YamlMcpServers => {
            json!({
                "command": binary,
                "args": args,
            })
        }
        ConfigFormat::JsonVsCodeServers => json!({
            "type": "stdio",
            "command": binary,
            "args": args,
        }),
        ConfigFormat::JsonOpencode => {
            let mut command = vec![binary];
            command.extend(args);
            json!({
                "type": "local",
                "command": command,
                "enabled": true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn context() -> SetupContext {
        SetupContext {
            home: PathBuf::from("/home/tester"),
            cwd: PathBuf::from("/work/project"),
            binary: PathBuf::from("/usr/local/bin/ukma"),
        }
    }

    #[test]
    fn the_ids_are_unique_and_lower_case() {
        let ids = ids();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "duplicate agent ids");
        for id in ids {
            assert!(
                id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{id} is not a lower-case slug"
            );
        }
    }

    #[test]
    fn every_configuration_path_stays_inside_a_root() {
        // No agent may point outside the home or project root: that is the
        // authorization boundary of the installer.
        for agent in AGENTS {
            for (index, relative) in [
                agent.config,
                agent.detect.first().copied().unwrap_or(agent.config),
            ]
            .iter()
            .enumerate()
            {
                let relative = match relative {
                    ConfigPath::Home(rel) | ConfigPath::Project(rel) => *rel,
                    ConfigPath::ClineStorage
                    | ConfigPath::HermesHome
                    | ConfigPath::AntigravityMcp
                    | ConfigPath::Unsupported => continue,
                };
                let path = PathBuf::from(relative);
                assert!(
                    path.is_relative(),
                    "{}: path #{index} is absolute",
                    agent.id
                );
                assert!(
                    !path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir)),
                    "{}: path #{index} escapes its root",
                    agent.id
                );
            }
            if let Some(rules) = agent.rules {
                let relative = match rules {
                    RulesPath::Home(rel) | RulesPath::Project(rel) => rel,
                };
                let path = PathBuf::from(relative);
                assert!(path.is_relative(), "{}: rules path is absolute", agent.id);
                assert!(
                    !path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir)),
                    "{}: rules path escapes its root",
                    agent.id
                );
            }
        }
    }

    #[test]
    fn the_project_scoped_agents_resolve_under_the_project() {
        let ctx = context();
        let copilot = find("copilot-vscode").expect("copilot is in the table");
        assert_eq!(
            copilot.config_path(&ctx),
            Some(PathBuf::from("/work/project/.vscode/mcp.json"))
        );
        assert_eq!(
            copilot.rules_path(&ctx),
            Some(PathBuf::from(
                "/work/project/.github/instructions/ukma.instructions.md"
            ))
        );
    }

    #[test]
    fn the_unsupported_agent_has_no_config_path() {
        let agent = find("jetbrains").expect("jetbrains is in the table");
        assert!(!agent.is_supported());
        assert_eq!(agent.config_path(&context()), None);
        assert!(!agent.is_detected(&context()));
    }

    #[test]
    fn find_rejects_unknown_ids() {
        assert!(find("nope").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn the_entries_carry_the_mcp_stdio_shape() {
        let ctx = context();
        let plain = server_entry(ConfigFormat::JsonMcpServers, &ctx, ".");
        assert_eq!(plain["command"], "/usr/local/bin/ukma");
        assert_eq!(plain["args"], json!(["--mcp", "--index-cwd", "."]));

        let vscode = server_entry(ConfigFormat::JsonVsCodeServers, &ctx, ".");
        assert_eq!(vscode["type"], "stdio");

        let opencode = server_entry(ConfigFormat::JsonOpencode, &ctx, ".");
        assert_eq!(opencode["type"], "local");
        assert_eq!(
            opencode["command"],
            json!(["/usr/local/bin/ukma", "--mcp", "--index-cwd", "."])
        );
    }

    #[test]
    fn the_copilot_rules_file_is_scoped_by_frontmatter() {
        let copilot = find("copilot-vscode").expect("copilot is in the table");
        assert!(copilot.rules_prefix.starts_with("---\n"));
        assert!(copilot.rules_prefix.contains("applyTo"));
    }

    #[test]
    fn hermes_writes_the_documented_yaml_config() {
        let agent = find("hermes").expect("hermes is in the table");
        assert!(agent.is_supported());
        assert_eq!(agent.format, ConfigFormat::YamlMcpServers);
        assert_eq!(agent.format.container(), Some("mcp_servers"));
        assert_eq!(agent.rules, None, "Hermes has no rules file UKMA can write");

        let ctx = context();
        let path = agent.config_path(&ctx).expect("a config path");
        assert!(path.ends_with(".hermes/config.yaml"), "{}", path.display());
    }

    #[test]
    fn hermes_honours_the_home_override() {
        let override_dir = std::env::temp_dir().join("ukma-hermes-override");
        assert_eq!(
            hermes_config_path_from(&context(), Some(&override_dir)),
            override_dir.join("config.yaml")
        );
        assert_eq!(
            hermes_config_path_from(&context(), None),
            PathBuf::from("/home/tester/.hermes/config.yaml")
        );
    }

    #[test]
    fn antigravity_prefers_the_unified_config_once_migrated() {
        let home = std::env::temp_dir().join(format!("ukma-antigravity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("home");
        let ctx = SetupContext {
            home: home.clone(),
            cwd: home.join("project"),
            binary: home.join("bin/ukma"),
        };
        let agent = find("antigravity").expect("antigravity is in the table");
        assert!(agent.is_supported());

        // Before the migration marker: the legacy path is the one written.
        let (preferred, other) = antigravity_config_paths(&ctx);
        assert!(
            preferred.ends_with("antigravity/mcp_config.json"),
            "{}",
            preferred.display()
        );
        assert!(other.ends_with(".gemini/config/mcp_config.json"));
        assert!(
            !agent.is_detected(&ctx),
            "nothing points at Antigravity yet"
        );

        // The marker (or the unified file) flips the preference, and both paths
        // stay in the sweep list so an uninstall cleans the old one too.
        std::fs::create_dir_all(home.join(".gemini/config")).expect("config dir");
        std::fs::write(home.join(".gemini/config/.migrated"), b"").expect("marker");
        let (preferred, other) = antigravity_config_paths(&ctx);
        assert!(preferred.ends_with(".gemini/config/mcp_config.json"));
        assert!(other.ends_with("antigravity/mcp_config.json"));
        assert_eq!(agent.all_config_paths(&ctx).len(), 2);
        assert!(agent.is_detected(&ctx));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_antigravity_entry_carries_no_type_field() {
        // Antigravity rejects a `type: "stdio"` entry, so the plain MCP shape is
        // the correct one for it.
        let entry = server_entry(ConfigFormat::JsonMcpServers, &context(), ".");
        assert!(entry.get("type").is_none(), "{entry}");
        assert_eq!(entry["command"], "/usr/local/bin/ukma");
    }
}
