//! Project bootstrap: the `.ukma/` directory behind `ukma init` / `ukma uninit`.
//!
//! A project prepared by `ukma init` carries a small manifest (format,
//! schema version, roots, excludes, file cap) plus the summary of the last
//! index. The server picks the manifest up automatically when it is started
//! without an explicit `--index-cwd` / `--workspace`, which is what makes a
//! bare `ukma --mcp` agent entry work per project.
//!
//! Honest boundary: the index is still a **dirty snapshot** of the working
//! tree, rebuilt on every start. Writing the manifest to disk does not make
//! the index durable or shared across processes.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{io_error, now_rfc3339, SetupError};

/// The project directory name.
pub const PROJECT_DIR: &str = ".ukma";
/// The manifest file name inside the project directory.
pub const MANIFEST_FILE: &str = "project.json";
/// The last-index summary file name inside the project directory.
pub const INDEX_FILE: &str = "last-index.json";
/// The manifest format tag.
pub const FORMAT: &str = "ukma.project";
/// The manifest schema version this build writes and understands.
pub const SCHEMA_VERSION: u32 = 1;

/// The persisted project manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectManifest {
    /// Always [`FORMAT`]; a different tag is rejected.
    pub format: String,
    /// The manifest schema version ([`SCHEMA_VERSION`] when written).
    pub schema_version: u32,
    /// When the project was first initialised (RFC 3339).
    pub created_at: String,
    /// When the manifest was last refreshed (RFC 3339).
    pub updated_at: String,
    /// Index roots, relative to the project directory (`["."]` by default).
    #[serde(default)]
    pub roots: Vec<String>,
    /// Path globs skipped while indexing.
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Cap on the number of indexed files.
    #[serde(default)]
    pub max_files: Option<usize>,
}

/// The summary of the last `ukma init` scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSummary {
    /// The content-addressed revision id (40 hex chars) of the scan.
    pub revision: String,
    /// Number of files in the scan.
    pub files: usize,
    /// When the scan ran (RFC 3339).
    pub generated_at: String,
}

/// The outcome of `ukma init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// The project root.
    pub root: PathBuf,
    /// The `.ukma` directory written.
    pub project_dir: PathBuf,
    /// The revision id of the scan.
    pub revision: String,
    /// The number of files indexed.
    pub files: usize,
    /// Whether the manifest is new (as opposed to refreshed).
    pub created: bool,
    /// Whether the snapshot cache (`.ukma/index.db`) also holds this snapshot.
    pub cached: bool,
    /// How many files this run read from disk.
    pub read: usize,
}

/// The outcome of `ukma uninit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninitReport {
    /// The `.ukma` directory addressed.
    pub project_dir: PathBuf,
    /// Whether it was removed.
    pub removed: bool,
}

/// A project manifest found next to the working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredProject {
    /// The project root (the directory holding `.ukma/`).
    pub root: PathBuf,
    /// The manifest.
    pub manifest: ProjectManifest,
}

impl DiscoveredProject {
    /// The absolute index roots implied by the manifest.
    pub fn roots(&self) -> Vec<PathBuf> {
        resolve_roots(&self.root, &self.manifest.roots)
    }

    /// Merge the manifest's excludes and file cap under the explicit
    /// command-line options (the command line wins).
    pub fn index_options(
        &self,
        base: &ukma_mcp::index::IndexOptions,
    ) -> ukma_mcp::index::IndexOptions {
        let mut options = base.clone();
        for exclude in &self.manifest.excludes {
            if !options.excludes.contains(exclude) {
                options.excludes.push(exclude.clone());
            }
        }
        if options.max_files.is_none() {
            options.max_files = self.manifest.max_files;
        }
        options
    }
}

/// Create (or refresh) the `.ukma/` directory of a project and index it.
pub fn init(
    root: &Path,
    workspaces: &[String],
    options: &ukma_mcp::index::IndexOptions,
) -> Result<InitReport, SetupError> {
    let root = resolve_root(root)?;
    if !root.is_dir() {
        return Err(SetupError::Missing(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let project_dir = root.join(PROJECT_DIR);
    let manifest_path = project_dir.join(MANIFEST_FILE);
    let previous = read_manifest(&manifest_path)?;

    let mut roots = vec![".".to_string()];
    roots.extend(workspaces.iter().cloned());
    let root_paths = resolve_roots(&root, &roots);

    let now = now_rfc3339();
    let manifest = ProjectManifest {
        format: FORMAT.to_string(),
        schema_version: SCHEMA_VERSION,
        created_at: previous
            .as_ref()
            .map(|manifest| manifest.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now.clone(),
        roots: roots.clone(),
        excludes: options.excludes.clone(),
        max_files: options.max_files,
    };

    // A cache that cannot be opened is rebuilt: it is a cache, and `init` is the
    // command whose job is to produce a usable snapshot.
    let store = match super::cache::open_store(&root) {
        Ok(store) => store,
        Err(_) => super::cache::recreate(&root)?,
    };
    let borrowed: Vec<&Path> = root_paths.iter().map(PathBuf::as_path).collect();
    let report =
        super::snapshot::sync_roots(&borrowed, options, &store, false).map_err(|error| {
            SetupError::Cache {
                path: super::cache::index_db_path(&root),
                detail: error.to_string(),
            }
        })?;
    let (revision, files, cached, read) = (report.revision, report.files, true, report.read);

    let summary = IndexSummary {
        revision: revision.clone(),
        files,
        generated_at: now,
    };
    write_json(&manifest_path, &manifest)?;
    write_json(&project_dir.join(INDEX_FILE), &summary)?;

    Ok(InitReport {
        root,
        project_dir,
        revision,
        files,
        created: previous.is_none(),
        cached,
        read,
    })
}

/// Remove a project's `.ukma/` directory.
pub fn uninit(root: &Path) -> Result<UninitReport, SetupError> {
    let root = resolve_root(root)?;
    let project_dir = root.join(PROJECT_DIR);
    if !project_dir.is_dir() {
        return Ok(UninitReport {
            project_dir,
            removed: false,
        });
    }
    fs::remove_dir_all(&project_dir).map_err(|error| io_error("remove", &project_dir, &error))?;
    Ok(UninitReport {
        project_dir,
        removed: true,
    })
}

/// Find and validate the manifest of `cwd` (no parent traversal).
pub fn discover(cwd: &Path) -> Result<Option<DiscoveredProject>, SetupError> {
    let root = resolve_root(cwd)?;
    let manifest_path = root.join(PROJECT_DIR).join(MANIFEST_FILE);
    let Some(manifest) = read_manifest(&manifest_path)? else {
        return Ok(None);
    };
    Ok(Some(DiscoveredProject { root, manifest }))
}

/// The scan options a command should use for `root`: the explicit options with
/// the project manifest's excludes and file cap merged underneath (the command
/// line wins).
pub fn effective_options(
    root: &Path,
    options: &ukma_mcp::index::IndexOptions,
) -> Result<ukma_mcp::index::IndexOptions, SetupError> {
    match discover(root)? {
        Some(project) => Ok(project.index_options(options)),
        None => Ok(options.clone()),
    }
}

/// Read and validate a manifest; `None` when the file is absent.
pub fn read_manifest(path: &Path) -> Result<Option<ProjectManifest>, SetupError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|error| io_error("read", path, &error))?;
    let manifest: ProjectManifest =
        serde_json::from_str(&text).map_err(|error| SetupError::Config {
            path: path.to_path_buf(),
            detail: format!("invalid JSON: {error}"),
        })?;
    if manifest.format != FORMAT {
        return Err(SetupError::Config {
            path: path.to_path_buf(),
            detail: format!("`format` is `{}`, expected `{FORMAT}`", manifest.format),
        });
    }
    if manifest.schema_version > SCHEMA_VERSION {
        return Err(SetupError::Config {
            path: path.to_path_buf(),
            detail: format!(
                "`schema_version` {} is newer than the supported {SCHEMA_VERSION}; \
                 re-run `ukma init` with a newer ukma",
                manifest.schema_version
            ),
        });
    }
    Ok(Some(manifest))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), SetupError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|error| SetupError::Config {
        path: path.to_path_buf(),
        detail: format!("cannot serialize: {error}"),
    })?;
    fs::write(path, format!("{text}\n")).map_err(|error| io_error("write", path, &error))
}

fn resolve_roots(base: &Path, roots: &[String]) -> Vec<PathBuf> {
    let roots: Vec<String> = if roots.is_empty() {
        vec![".".to_string()]
    } else {
        roots.to_vec()
    };
    roots
        .iter()
        .map(|relative| {
            let path = PathBuf::from(relative);
            if path.is_absolute() {
                normalize(path)
            } else {
                normalize(base.join(path))
            }
        })
        .collect()
}

/// Drop `.` components so paths render without a trailing `\.`.
fn normalize(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::CurDir) {
            continue;
        }
        normalized.push(component);
    }
    normalized
}

/// Resolve `path` against the working directory and drop `.` components.
pub fn resolve_root(path: &Path) -> Result<PathBuf, SetupError> {
    if path.is_absolute() {
        return Ok(normalize(path.to_path_buf()));
    }
    let cwd = std::env::current_dir()
        .map_err(|error| io_error("read the working directory", path, &error))?;
    Ok(normalize(cwd.join(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_root_list_defaults_to_the_project_directory() {
        let roots = resolve_roots(Path::new("/work/project"), &[]);
        assert_eq!(roots, vec![PathBuf::from("/work/project")]);
    }

    #[test]
    fn absolute_roots_are_kept_and_relative_ones_are_anchored() {
        let roots = resolve_roots(
            Path::new("/work/project"),
            &[".", "/elsewhere"].map(str::to_string),
        );
        assert_eq!(roots[0], PathBuf::from("/work/project"));
        assert_eq!(roots[1], PathBuf::from("/elsewhere"));
    }

    #[test]
    fn the_manifest_round_trips_through_json() {
        let manifest = ProjectManifest {
            format: FORMAT.to_string(),
            schema_version: SCHEMA_VERSION,
            created_at: "2026-10-26T00:00:00Z".into(),
            updated_at: "2026-10-26T00:00:01Z".into(),
            roots: vec![".".into()],
            excludes: vec!["docs/**".into()],
            max_files: Some(10),
        };
        let text = serde_json::to_string(&manifest).expect("serializes");
        let parsed: ProjectManifest = serde_json::from_str(&text).expect("parses");
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn a_manifest_without_optional_fields_still_parses() {
        let text = r#"{
            "format": "ukma.project",
            "schema_version": 1,
            "created_at": "2026-10-26T00:00:00Z",
            "updated_at": "2026-10-26T00:00:00Z"
        }"#;
        let parsed: ProjectManifest = serde_json::from_str(text).expect("parses");
        assert!(parsed.roots.is_empty());
        assert_eq!(parsed.max_files, None);
    }

    #[test]
    fn the_index_summary_round_trips() {
        let summary = IndexSummary {
            revision: "a".repeat(40),
            files: 3,
            generated_at: "2026-10-26T00:00:00Z".into(),
        };
        let text = serde_json::to_string(&summary).expect("serializes");
        assert_eq!(
            serde_json::from_str::<IndexSummary>(&text).expect("parses"),
            summary
        );
    }
}
