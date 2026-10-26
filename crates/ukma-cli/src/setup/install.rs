//! `ukma install` and `ukma uninstall`: wire (and unwire) the MCP server into
//! the coding agents detected on this machine.
//!
//! Both commands are merge-only and idempotent: running them twice is a no-op,
//! and every non-UKMA key in an agent's configuration is preserved. A dry run
//! computes the full report without touching the disk.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::agents::{self, Agent, ConfigFormat};
use super::config::{self, Plan};
use super::{SetupContext, SetupError};

/// What happened to one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The configuration did not exist and was created.
    Configured,
    /// An existing configuration was updated.
    Updated,
    /// UKMA's wiring was removed.
    Removed,
    /// Nothing needed to change.
    Unchanged,
    /// The agent was not touched (not detected, or not MCP-configurable).
    Skipped,
    /// The change was computed but not written (dry run).
    Planned,
}

impl Action {
    /// The label used in the report.
    pub fn label(&self) -> &'static str {
        match self {
            Action::Configured => "configured",
            Action::Updated => "updated",
            Action::Removed => "removed",
            Action::Unchanged => "unchanged",
            Action::Skipped => "skipped",
            Action::Planned => "would change",
        }
    }
}

/// What happened to one agent.
#[derive(Debug, Clone)]
pub struct AgentAction {
    /// The `--target` id (`cli` for the binary itself).
    pub id: &'static str,
    /// The display name.
    pub name: &'static str,
    /// The outcome.
    pub action: Action,
    /// The file involved, when there is one.
    pub path: Option<PathBuf>,
    /// A short human note.
    pub detail: String,
}

/// A request to wire agents up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    /// Explicit `--target` ids; empty means "every detected agent".
    pub targets: Vec<String>,
    /// Compute the report without writing anything.
    pub dry_run: bool,
    /// Also write the agent's UKMA rules file.
    pub rules: bool,
    /// The root passed to the server as `--index-cwd`.
    pub index_root: String,
}

impl Default for InstallRequest {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            dry_run: false,
            rules: true,
            index_root: ".".into(),
        }
    }
}

impl InstallRequest {
    fn server_args(&self) -> Vec<String> {
        agents::server_args(&self.index_root)
    }
}

/// A request to remove the wiring again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallRequest {
    /// Explicit `--target` ids; empty means "every detected agent".
    pub targets: Vec<String>,
    /// Compute the report without writing anything.
    pub dry_run: bool,
    /// Also delete the `ukma` binary itself.
    pub remove_cli: bool,
    /// Keep the binary even if `--remove-cli` was implied.
    pub keep_cli: bool,
    /// Confirmed: destructive commands require this (or a dry run).
    pub yes: bool,
}

/// The outcome of an install or uninstall run.
#[derive(Debug, Clone)]
pub struct InstallReport {
    /// Whether this was an install (`true`) or an uninstall (`false`).
    pub install: bool,
    /// Whether it was a dry run.
    pub dry_run: bool,
    /// The binary the entries point at.
    pub binary: PathBuf,
    /// One entry per agent, in table order.
    pub actions: Vec<AgentAction>,
}

impl InstallReport {
    /// The count of agents in one outcome.
    pub fn count(&self, action: Action) -> usize {
        self.actions
            .iter()
            .filter(|entry| entry.action == action)
            .count()
    }

    /// The rendered report.
    pub fn text(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(
            text,
            "ukma {} ({}) -> {}",
            if self.install { "install" } else { "uninstall" },
            if self.dry_run { "dry run" } else { "applied" },
            self.binary.display()
        );
        if self.actions.is_empty() {
            let _ = writeln!(
                text,
                "  no agents selected (nothing detected; pass --target <id>)"
            );
        }
        for entry in &self.actions {
            let path = entry
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            let detail = if entry.detail.is_empty() {
                path
            } else if path.is_empty() {
                entry.detail.clone()
            } else {
                format!("{path} ({})", entry.detail)
            };
            let _ = writeln!(
                text,
                "  {:<14} {:<26} {:<11} {}",
                entry.id,
                entry.name,
                entry.action.label(),
                detail
            );
        }
        let mut counts: Vec<String> = Vec::new();
        for action in [
            Action::Configured,
            Action::Updated,
            Action::Removed,
            Action::Planned,
            Action::Unchanged,
            Action::Skipped,
        ] {
            let count = self.count(action);
            if count > 0 {
                counts.push(format!("{count} {}", action.label()));
            }
        }
        if counts.is_empty() {
            let _ = writeln!(text, "summary: nothing to do");
        } else {
            let _ = writeln!(text, "summary: {}", counts.join(", "));
        }
        if self.install {
            let _ = write!(
                text,
                "next: run `ukma init` inside each project, then restart the agent \
                 so it reloads its MCP servers."
            );
        } else {
            let _ = write!(
                text,
                "project directories were left untouched; remove them per project \
                 with `ukma uninit`."
            );
        }
        text
    }
}

/// Wire the MCP server into the selected (or detected) agents.
pub fn install(ctx: &SetupContext, request: &InstallRequest) -> Result<InstallReport, SetupError> {
    let selected = select(ctx, &request.targets)?;
    let mut actions = Vec::with_capacity(selected.len());
    for agent in selected {
        actions.push(install_agent(ctx, agent, request)?);
    }
    Ok(InstallReport {
        install: true,
        dry_run: request.dry_run,
        binary: ctx.binary.clone(),
        actions,
    })
}

/// Remove the MCP server wiring from the selected (or detected) agents.
pub fn uninstall(
    ctx: &SetupContext,
    request: &UninstallRequest,
) -> Result<InstallReport, SetupError> {
    if !request.yes && !request.dry_run {
        return Err(SetupError::ConfirmationRequired(
            "`ukma uninstall` rewrites agent configuration; re-run with `--yes` \
             (or `--dry-run` to preview)"
                .into(),
        ));
    }
    let selected = select(ctx, &request.targets)?;
    let mut actions = Vec::with_capacity(selected.len() + 1);
    for agent in selected {
        actions.push(uninstall_agent(ctx, agent, request)?);
    }
    if request.remove_cli && !request.keep_cli {
        actions.push(remove_cli(ctx, request)?);
    }
    Ok(InstallReport {
        install: false,
        dry_run: request.dry_run,
        binary: ctx.binary.clone(),
        actions,
    })
}

fn select(ctx: &SetupContext, targets: &[String]) -> Result<Vec<&'static Agent>, SetupError> {
    if targets.is_empty() {
        return Ok(agents::detected(ctx));
    }
    let mut selected: Vec<&'static Agent> = Vec::new();
    for target in targets {
        if target == "all" {
            for agent in agents::AGENTS.iter().filter(|agent| agent.is_supported()) {
                if !selected.iter().any(|chosen| chosen.id == agent.id) {
                    selected.push(agent);
                }
            }
            continue;
        }
        let agent = agents::find(target).ok_or_else(|| {
            SetupError::UnknownTarget(format!(
                "unknown agent `{target}`; known targets: {} (or `all`)",
                agents::ids().join(", ")
            ))
        })?;
        if !selected.iter().any(|chosen| chosen.id == agent.id) {
            selected.push(agent);
        }
    }
    Ok(selected)
}

fn install_agent(
    ctx: &SetupContext,
    agent: &Agent,
    request: &InstallRequest,
) -> Result<AgentAction, SetupError> {
    let Some(path) = agent.config_path(ctx) else {
        return Ok(AgentAction {
            id: agent.id,
            name: agent.name,
            action: Action::Skipped,
            path: None,
            detail: agent.note.to_string(),
        });
    };
    let plan = match agent.format {
        ConfigFormat::TomlBlock => {
            config::install_toml(&path, &ctx.binary, &request.server_args(), request.dry_run)?
        }
        format => config::install_json(
            &path,
            format,
            &agents::server_entry(format, ctx, &request.index_root),
            request.dry_run,
        )?,
    };
    let mut detail = String::new();
    let mut rules_changed = false;
    if request.rules {
        if let Some(rules_path) = agent.rules_path(ctx) {
            let rules = config::write_rules(
                &rules_path,
                agent.rules_prefix,
                agent.rules_body,
                request.dry_run,
            )?;
            rules_changed = rules.changed;
            detail = if rules.changed {
                format!(
                    "rules {} {}",
                    if request.dry_run {
                        "would write"
                    } else {
                        "written"
                    },
                    rules_path.display()
                )
            } else {
                format!("rules unchanged {}", rules_path.display())
            };
        }
    }
    Ok(AgentAction {
        id: agent.id,
        name: agent.name,
        action: classify(&plan, rules_changed, request.dry_run),
        path: Some(plan.path),
        detail,
    })
}

fn uninstall_agent(
    ctx: &SetupContext,
    agent: &Agent,
    request: &UninstallRequest,
) -> Result<AgentAction, SetupError> {
    let Some(path) = agent.config_path(ctx) else {
        return Ok(AgentAction {
            id: agent.id,
            name: agent.name,
            action: Action::Skipped,
            path: None,
            detail: agent.note.to_string(),
        });
    };
    let plan = match agent.format {
        ConfigFormat::TomlBlock => config::remove_toml(&path, request.dry_run)?,
        format => config::remove_json(&path, format, request.dry_run)?,
    };
    let mut detail = String::new();
    let mut rules_changed = false;
    if let Some(rules_path) = agent.rules_path(ctx) {
        let rules = config::remove_rules(
            &rules_path,
            agent.rules_prefix,
            agent.rules_body,
            request.dry_run,
        )?;
        rules_changed = rules.changed;
        if rules.changed {
            detail = format!("rules removed {}", rules_path.display());
        } else if !rules.detail.is_empty() {
            detail = rules.detail.clone();
        }
    }
    let action = if plan.changed || rules_changed {
        if request.dry_run {
            Action::Planned
        } else {
            Action::Removed
        }
    } else {
        Action::Unchanged
    };
    Ok(AgentAction {
        id: agent.id,
        name: agent.name,
        action,
        path: Some(plan.path),
        detail,
    })
}

fn remove_cli(ctx: &SetupContext, request: &UninstallRequest) -> Result<AgentAction, SetupError> {
    let binary = ctx.binary.clone();
    let name = binary
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| SetupContext::binary_name().to_string());
    let stale = binary.with_file_name(format!("{name}.old"));
    let action = if !binary.exists() {
        AgentAction {
            id: "cli",
            name: "ukma binary",
            action: Action::Skipped,
            path: None,
            detail: format!("{} not found", binary.display()),
        }
    } else if request.dry_run {
        AgentAction {
            id: "cli",
            name: "ukma binary",
            action: Action::Planned,
            path: Some(binary),
            detail: "would delete".into(),
        }
    } else {
        // The binary being uninstalled is often the one running this command. A
        // running image cannot be deleted on Windows, but it can be renamed, so
        // the install path is vacated either way and the leftover is a `.old`
        // file the next install/uninstall sweeps.
        let _ = std::fs::remove_file(&stale);
        let detail = match std::fs::remove_file(&binary) {
            Ok(()) => Some("deleted".to_string()),
            Err(error) => match std::fs::rename(&binary, &stale) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&stale);
                    Some(format!(
                        "deleted (the running image was moved to {}; it goes away on the next run)",
                        stale.display()
                    ))
                }
                Err(_) => {
                    let _ = error;
                    None
                }
            },
        };
        match detail {
            Some(detail) => AgentAction {
                id: "cli",
                name: "ukma binary",
                action: Action::Removed,
                path: Some(binary),
                detail,
            },
            None => AgentAction {
                id: "cli",
                name: "ukma binary",
                action: Action::Skipped,
                path: Some(binary.clone()),
                detail: "could not delete; a running binary must be removed \
                         from a shell that is not using it"
                    .into(),
            },
        }
    };
    Ok(action)
}

fn classify(plan: &Plan, rules_changed: bool, dry_run: bool) -> Action {
    if plan.changed || rules_changed {
        if dry_run {
            return Action::Planned;
        }
        if plan.changed {
            return if plan.created {
                Action::Configured
            } else {
                Action::Updated
            };
        }
        return Action::Updated;
    }
    Action::Unchanged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(install: bool, actions: Vec<AgentAction>) -> InstallReport {
        InstallReport {
            install,
            dry_run: false,
            binary: PathBuf::from("/usr/local/bin/ukma"),
            actions,
        }
    }

    #[test]
    fn the_counts_add_up() {
        let report = report(
            true,
            vec![
                AgentAction {
                    id: "a",
                    name: "A",
                    action: Action::Configured,
                    path: None,
                    detail: String::new(),
                },
                AgentAction {
                    id: "b",
                    name: "B",
                    action: Action::Skipped,
                    path: None,
                    detail: String::new(),
                },
                AgentAction {
                    id: "c",
                    name: "C",
                    action: Action::Skipped,
                    path: None,
                    detail: String::new(),
                },
            ],
        );
        assert_eq!(report.count(Action::Configured), 1);
        assert_eq!(report.count(Action::Skipped), 2);
        let text = report.text();
        assert!(text.contains("1 configured"));
        assert!(text.contains("2 skipped"));
        assert!(text.contains("ukma init"));
    }

    #[test]
    fn an_empty_report_explains_how_to_select_agents() {
        let text = report(true, Vec::new()).text();
        assert!(text.contains("no agents selected"));
        assert!(text.contains("--target"));
        assert!(text.contains("summary: nothing to do"));
    }

    #[test]
    fn the_uninstall_report_points_at_uninit() {
        let text = report(false, Vec::new()).text();
        assert!(text.contains("ukma uninit"));
    }

    #[test]
    fn classify_reports_created_updated_and_unchanged() {
        let created = Plan {
            path: PathBuf::from("x"),
            changed: true,
            created: true,
            detail: String::new(),
        };
        let updated = Plan {
            created: false,
            ..created.clone()
        };
        let unchanged = Plan {
            changed: false,
            ..created.clone()
        };
        assert_eq!(classify(&created, false, false), Action::Configured);
        assert_eq!(classify(&updated, false, false), Action::Updated);
        assert_eq!(classify(&unchanged, false, false), Action::Unchanged);
        assert_eq!(classify(&unchanged, true, false), Action::Updated);
        assert_eq!(classify(&created, false, true), Action::Planned);
    }

    #[test]
    fn select_rejects_an_unknown_target() {
        let ctx = SetupContext {
            home: PathBuf::from("/home/tester"),
            cwd: PathBuf::from("/work/project"),
            binary: PathBuf::from("/usr/local/bin/ukma"),
        };
        let error = select(&ctx, &["nope".to_string()]).expect_err("unknown target");
        match error {
            SetupError::UnknownTarget(detail) => {
                assert!(detail.contains("nope"));
                assert!(detail.contains("claude-code"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn select_all_includes_every_supported_agent_once() {
        let ctx = SetupContext {
            home: PathBuf::from("/home/tester"),
            cwd: PathBuf::from("/work/project"),
            binary: PathBuf::from("/usr/local/bin/ukma"),
        };
        let selected =
            select(&ctx, &["all".to_string(), "cursor".to_string()]).expect("all is always valid");
        assert_eq!(selected.len(), agents::AGENTS.len() - 1);
        assert_eq!(
            selected.iter().filter(|agent| agent.id == "cursor").count(),
            1,
            "a duplicated target is selected once"
        );
        assert!(selected.iter().all(|agent| agent.is_supported()));
    }
}
