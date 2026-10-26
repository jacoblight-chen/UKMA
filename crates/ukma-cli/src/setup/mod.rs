//! Installer, agent wiring and project bootstrap.
//!
//! This module is the CodeGraph-style setup surface of the single `ukma`
//! binary: `install` / `uninstall` wire (and unwire) the MCP server into coding
//! agents and write their rules files, `init` / `uninit` create (and remove) a
//! project's `.ukma/` directory, and `upgrade` self-updates the binary.
//!
//! It is CLI-side orchestration only. It never declares an operation, a scope
//! or an error code, never talks to a database, and does not touch the
//! wire-level API (ADR-021): the 65-operation registry and its schemas are
//! unchanged by anything in this module.

pub mod agents;
pub mod cache;
pub mod config;
pub mod daemon;
pub mod git_state;
pub mod install;
pub mod project;
pub mod snapshot;
pub mod upgrade;
pub mod watch;

use std::path::{Path, PathBuf};

/// The filesystem context a setup command acts on.
///
/// Every path a setup command writes is derived from one of these three roots,
/// so a command can be pointed at a scratch directory (tests, CI, a
/// multi-account machine) instead of the real one.
#[derive(Debug, Clone)]
pub struct SetupContext {
    /// The home directory agent configurations are resolved against
    /// (`--home`, `$UKMA_HOME`, else the platform home).
    pub home: PathBuf,
    /// The project directory project-scoped configuration is resolved against
    /// (`--cwd`, else the process working directory).
    pub cwd: PathBuf,
    /// The `ukma` binary path written into the agent configuration
    /// (`--binary`, else the running executable).
    pub binary: PathBuf,
}

impl SetupContext {
    /// Resolve a context from explicit overrides, falling back to the
    /// environment.
    pub fn resolve(
        home: Option<&str>,
        cwd: Option<&str>,
        binary: Option<&str>,
    ) -> Result<Self, SetupError> {
        let home = match home {
            Some(path) => PathBuf::from(path),
            None => default_home()?,
        };
        let cwd = match cwd {
            Some(path) => PathBuf::from(path),
            None => std::env::current_dir().map_err(|error| SetupError::Io {
                action: "read the working directory".into(),
                path: PathBuf::from("."),
                detail: error.to_string(),
            })?,
        };
        let binary = match binary {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe().map_err(|error| SetupError::Io {
                action: "locate the running binary".into(),
                path: PathBuf::from("."),
                detail: error.to_string(),
            })?,
        };
        Ok(Self { home, cwd, binary })
    }

    /// The `ukma` executable name for this platform.
    pub fn binary_name() -> &'static str {
        if cfg!(windows) {
            "ukma.exe"
        } else {
            "ukma"
        }
    }

    /// The default install directory for this platform: `LOCALAPPDATA\UKMA\bin`
    /// on Windows, `~/.local/bin` elsewhere.
    pub fn default_install_dir(&self) -> PathBuf {
        if cfg!(windows) {
            match std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
                Some(base) => PathBuf::from(base).join("UKMA").join("bin"),
                None => self
                    .home
                    .join("AppData")
                    .join("Local")
                    .join("UKMA")
                    .join("bin"),
            }
        } else {
            self.home.join(".local").join("bin")
        }
    }
}

fn default_home() -> Result<PathBuf, SetupError> {
    if let Some(explicit) = std::env::var_os("UKMA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(explicit));
    }
    for key in ["USERPROFILE", "HOME"] {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(value));
        }
    }
    Err(SetupError::Missing(
        "cannot resolve the user home directory; pass `--home <dir>` or set `UKMA_HOME`".into(),
    ))
}

/// Everything that can go wrong in a setup command.
#[derive(Debug)]
pub enum SetupError {
    /// A filesystem operation failed.
    Io {
        /// What was being attempted.
        action: String,
        /// The path involved.
        path: PathBuf,
        /// The underlying message.
        detail: String,
    },
    /// A configuration file exists but is not in the shape this installer
    /// understands.
    Config {
        /// The file involved.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },
    /// `--target` named an agent that does not exist.
    UnknownTarget(String),
    /// A destructive command ran without `--yes`.
    ConfirmationRequired(String),
    /// A required input or path is missing.
    Missing(String),
    /// A network step failed (or `curl` is unavailable).
    Network(String),
    /// A downloaded or supplied file failed its integrity check.
    Integrity(String),
    /// The snapshot cache could not be used.
    Cache {
        /// The database file.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },
    /// The platform does not support the operation.
    Unsupported(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Io {
                action,
                path,
                detail,
            } => write!(f, "cannot {action} {}: {detail}", path.display()),
            SetupError::Config { path, detail } => {
                write!(
                    f,
                    "{} is not a usable configuration: {detail}",
                    path.display()
                )
            }
            SetupError::UnknownTarget(detail) => write!(f, "{detail}"),
            SetupError::ConfirmationRequired(detail) => write!(f, "{detail}"),
            SetupError::Missing(detail) => write!(f, "{detail}"),
            SetupError::Network(detail) => write!(f, "{detail}"),
            SetupError::Integrity(detail) => write!(f, "{detail}"),
            SetupError::Cache { path, detail } => {
                write!(f, "snapshot cache {}: {detail}", path.display())
            }
            SetupError::Unsupported(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for SetupError {}

/// Build an [`SetupError::Io`] from a failed operation.
pub fn io_error(action: &str, path: &Path, error: &std::io::Error) -> SetupError {
    SetupError::Io {
        action: action.to_string(),
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

/// Whether a path exists (used for detection probes).
pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// The current UTC instant as RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    ukma_mcp::instant::format_rfc3339(seconds)
}
