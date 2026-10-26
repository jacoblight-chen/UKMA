//! Git awareness for the project index: what the working tree is, version-control-wise.
//!
//! The snapshot cache (`.ukma/index.db`) tracks files by `(size, mtime)`; it does
//! not know which commit the tree is at, which branch is checked out, or which
//! files are staged, modified, or untracked. This module adds that layer on top:
//! it runs `git` against a project root and records the result in a sidecar
//! (`.ukma/git-state.json`) that is refreshed on every `init` / `sync` and shown
//! by `status`.
//!
//! It is CLI-side orchestration only: it never touches the `SnapshotStore` port
//! or any backend (so the incremental policy stays testable with an in-memory
//! store), never declares an operation or error code, and does not change the
//! wire-level API (ADR-021). The 65-operation registry is untouched.
//!
//! Capture is graceful: a root that is not a repository, a `git` that is missing,
//! or a corrupt repository all produce a typed [`GitState`], never a panic. The
//! `git` executable is injectable ([`capture_with`]) so the failure paths are
//! testable without a real `git` on the machine.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use super::{now_rfc3339, SetupError};

/// The sidecar file name inside the project directory.
pub const GIT_STATE_FILE: &str = "git-state.json";
/// The schema version this build writes and understands.
pub const GIT_STATE_SCHEMA_VERSION: i64 = 1;

/// The git state of one project root, as of the last capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GitState {
    /// The root is inside a git work tree.
    Repo {
        /// The current `HEAD` commit (full SHA), or `None` for an empty
        /// repository (no commits yet).
        head: Option<String>,
        /// The checked-out branch, or `None` when detached.
        branch: Option<String>,
        /// The short commit id when detached (and `head` is known).
        detached: Option<String>,
        /// Files with an index (staged) change.
        staged: Vec<String>,
        /// Files with a working-tree (unstaged) change.
        unstaged: Vec<String>,
        /// Files not yet tracked by the repository.
        untracked: Vec<String>,
    },
    /// The root is not inside a git work tree.
    NotRepo,
    /// `git` could not be run (missing, or the repository is unusable).
    Unavailable(String),
}

impl GitState {
    /// Whether this state describes a repository.
    pub fn is_repo(&self) -> bool {
        matches!(self, GitState::Repo { .. })
    }

    /// Whether the repository has any pending change.
    pub fn dirty(&self) -> bool {
        matches!(
            self,
            GitState::Repo {
                staged,
                unstaged,
                untracked,
                ..
            } if !staged.is_empty() || !unstaged.is_empty() || !untracked.is_empty()
        )
    }

    /// A one-line human summary for reports.
    pub fn summary(&self) -> String {
        match self {
            GitState::Repo {
                head,
                branch,
                detached,
                staged,
                unstaged,
                untracked,
            } => {
                let at = match (branch.as_deref(), detached.as_deref()) {
                    (Some(branch), _) => format!("@ {branch}"),
                    (None, Some(detached)) => format!("(detached @ {detached})"),
                    (None, None) => String::new(),
                };
                let head = head
                    .as_ref()
                    .map(|head| format!(" ({})", head.chars().take(7).collect::<String>()))
                    .unwrap_or_default();
                format!(
                    "repo {at}{head}: {} staged, {} unstaged, {} untracked",
                    staged.len(),
                    unstaged.len(),
                    untracked.len()
                )
            }
            GitState::NotRepo => "not a git repository".to_string(),
            GitState::Unavailable(detail) => format!("git unavailable: {detail}"),
        }
    }
}

/// The sidecar path of a project.
pub fn path(root: &Path) -> PathBuf {
    root.join(super::project::PROJECT_DIR).join(GIT_STATE_FILE)
}

/// Capture the git state of `root` using the default `git` on `PATH`.
pub fn capture(root: &Path) -> GitState {
    capture_with(root, "git")
}

/// Capture the git state of `root` using an explicit `git` executable.
///
/// `git` is injectable so the failure paths (missing executable, corrupt
/// repository) are testable without a real `git` on the machine.
pub fn capture_with(root: &Path, git: &str) -> GitState {
    // 1. Is the root inside a work tree? A "true" answer is a repository. A
    //    "false" answer, or a "not a git repository" fatal (some `git` versions
    //    report a non-repository this way instead of printing "false"), is a
    //    clean NotRepo; a missing or unusable `git` is a hard failure.
    match run_git(git, root, &["rev-parse", "--is-inside-work-tree"]) {
        Run::Ok(output) if output.trim() == "true" => {}
        Run::Ok(_) => return GitState::NotRepo,
        Run::Err(detail) => {
            if detail.to_lowercase().contains("not a git") {
                return GitState::NotRepo;
            }
            return GitState::Unavailable(detail);
        }
    }
    // 2. HEAD (absent on an empty repository — still a repository).
    let head = match run_git(git, root, &["rev-parse", "HEAD"]) {
        Run::Ok(output) => Some(output.trim().to_string()),
        Run::Err(_) => None,
    };
    // 3. Branch; `--abbrev-ref HEAD` reports "HEAD" when detached.
    let (branch, detached) = match run_git(git, root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Run::Ok(output) => {
            let name = output.trim();
            if name == "HEAD" {
                (
                    None,
                    head.as_ref().map(|head| head.chars().take(7).collect()),
                )
            } else {
                (Some(name.to_string()), None)
            }
        }
        Run::Err(_) => (None, None),
    };
    // 4. The staged / unstaged / untracked split, from `status --porcelain`.
    let (staged, unstaged, untracked) = match run_git(
        git,
        root,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    ) {
        Run::Ok(output) => parse_status(&output),
        Run::Err(detail) => return GitState::Unavailable(detail),
    };
    GitState::Repo {
        head,
        branch,
        detached,
        staged,
        unstaged,
        untracked,
    }
}

/// Persist the git state to the sidecar (creating the project directory if
/// needed). The sidecar is a cache, not a system of record: a stale one is
/// simply refreshed on the next `sync`.
pub fn persist(root: &Path, state: &GitState) -> Result<(), SetupError> {
    let dir = root.join(super::project::PROJECT_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|error| super::io_error("create the project directory", &dir, &error))?;
    let document = serde_json::json!({
        "schema_version": GIT_STATE_SCHEMA_VERSION,
        "updated_at": now_rfc3339(),
        "state": state,
    });
    let bytes = serde_json::to_vec_pretty(&document).map_err(|error| SetupError::Io {
        action: "serialize the git state".into(),
        path: path(root),
        detail: error.to_string(),
    })?;
    std::fs::write(path(root), bytes)
        .map_err(|error| super::io_error("write the git state", &path(root), &error))
}

/// Read the git state from the sidecar. `None` when the sidecar is absent,
/// unreadable, or written by an incompatible schema version (the next `sync`
/// refreshes it).
pub fn load(root: &Path) -> Option<GitState> {
    let bytes = std::fs::read(path(root)).ok()?;
    let document: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let version = document.get("schema_version")?.as_i64()?;
    if version != GIT_STATE_SCHEMA_VERSION {
        return None;
    }
    serde_json::from_value(document.get("state")?.clone()).ok()
}

/// The result of one `git` invocation.
enum Run {
    /// The command succeeded; the standard output.
    Ok(String),
    /// The command could not be run or exited non-zero; a short reason.
    Err(String),
}

/// Run one `git` command against `root` and return its (success, output).
///
/// `git -C <root>` makes the command operate on the repository at `root` without
/// changing the process working directory, so a multi-root project captures each
/// root independently.
fn run_git(git: &str, root: &Path, args: &[&str]) -> Run {
    let mut command = std::process::Command::new(git);
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match command.output() {
        Ok(output) if output.status.success() => {
            Run::Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Run::Err(if stderr.trim().is_empty() {
                format!("git exited with {}", output.status)
            } else {
                stderr.trim().to_string()
            })
        }
        Err(error) => Run::Err(format!("cannot run git: {error}")),
    }
}

/// Split `git status --porcelain=v1` output into staged / unstaged / untracked.
///
/// Each line is `XY <path>` (or `XY <old> -> <new>` for a rename): `X` is the
/// index (staged) status, `Y` the working-tree (unstaged) status. `??` is
/// untracked; a real `X` marks a staged change, a real `Y` an unstaged one (a
/// file may be both, e.g. `MM`).
fn parse_status(output: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    for line in output.lines() {
        if line.len() < 4 {
            continue;
        }
        let x = line.as_bytes()[0];
        let y = line.as_bytes()[1];
        let path_part = &line[3..];
        let path = if let Some(arrow) = path_part.find(" -> ") {
            path_part[arrow + 4..].to_string()
        } else {
            path_part.to_string()
        };
        // The index's own `.ukma/` directory is a tool artifact (like `.git/`),
        // not the user's code: exclude it so the state reflects the project.
        if path == super::project::PROJECT_DIR
            || path.starts_with(&format!("{}/", super::project::PROJECT_DIR))
        {
            continue;
        }
        if x == b'?' && y == b'?' {
            untracked.push(path);
        } else {
            if x != b' ' {
                staged.push(path.clone());
            }
            if y != b' ' {
                unstaged.push(path);
            }
        }
    }
    (staged, unstaged, untracked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A scratch directory that is removed when dropped.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("ukma-git-state-{unique}"));
        std::fs::create_dir_all(&path).expect("create the scratch directory");
        TempDir(path)
    }

    /// Whether a usable `git` is on `PATH` (the repository tests skip otherwise).
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Initialize a git repository with one committed file, then apply the given
    /// post-commit mutations. Returns the root (an owned path; the `TempDir`
    /// keeps it alive for the test's duration).
    fn make_repo(mutate: impl Fn(&Path)) -> (TempDir, PathBuf) {
        let dir = temp_dir();
        let root = dir.0.clone();
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .env("GIT_AUTHOR_NAME", "ukma-test")
                .env("GIT_AUTHOR_EMAIL", "ukma-test@example.com")
                .env("GIT_COMMITTER_NAME", "ukma-test")
                .env("GIT_COMMITTER_EMAIL", "ukma-test@example.com")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .expect("run git")
        };
        run(&["init", "-q"]);
        std::fs::write(root.join("tracked.txt"), "one\n").expect("write a file");
        run(&["add", "tracked.txt"]);
        run(&["commit", "-q", "-m", "initial"]);
        mutate(&root);
        (dir, root)
    }

    #[test]
    fn capture_reports_the_repo_head_branch_and_changes() {
        if !git_available() {
            return; // skip: git is not available
        }
        let (_dir, root) = make_repo(|root| {
            std::fs::write(root.join("tracked.txt"), "changed\n").expect("modify");
            std::fs::write(root.join("new.txt"), "new\n").expect("add a file");
            Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("add")
                .arg("new.txt")
                .output()
                .expect("git add");
        });
        let state = capture(&root);
        let repo = match &state {
            GitState::Repo {
                head,
                branch,
                staged,
                unstaged,
                untracked,
                ..
            } => (head, branch, staged, unstaged, untracked),
            other => panic!("expected a repository, got {other:?}"),
        };
        let (head, branch, staged, unstaged, untracked) = repo;
        assert!(
            head.as_ref().map(|h| h.len() == 40).unwrap_or(false),
            "head is a full SHA"
        );
        assert!(branch.is_some(), "a fresh `git init` checks out a branch");
        assert_eq!(staged, &["new.txt".to_string()], "the added file is staged");
        assert_eq!(
            unstaged,
            &["tracked.txt".to_string()],
            "the edited file is unstaged"
        );
        assert!(untracked.is_empty(), "a staged file is not also untracked");
    }

    #[test]
    fn a_clean_repo_reports_no_pending_changes() {
        if !git_available() {
            return; // skip: git is not available
        }
        let (_dir, root) = make_repo(|_| {});
        let state = capture(&root);
        assert!(
            state.is_repo(),
            "a committed repository is a repo: {state:?}"
        );
        assert!(!state.dirty(), "a clean repository is not dirty");
    }

    #[test]
    fn a_non_repository_root_is_not_a_repo() {
        let dir = temp_dir();
        let state = capture(&dir.0);
        assert_eq!(state, GitState::NotRepo, "a plain directory is not a repo");
    }

    #[test]
    fn a_missing_git_executable_is_unavailable() {
        let dir = temp_dir();
        let state = capture_with(&dir.0, "definitely-not-a-real-git-binary");
        assert!(
            matches!(state, GitState::Unavailable(_)),
            "missing git is unavailable: {state:?}"
        );
    }

    #[test]
    fn a_corrupt_repository_is_not_a_valid_repo() {
        let dir = temp_dir();
        // A `.git` that is a regular file (not a directory) is not a valid repo.
        std::fs::write(dir.0.join(".git"), "not a real repository").expect("write a fake .git");
        let state = capture(&dir.0);
        assert!(
            !state.is_repo(),
            "a corrupt repository must not be reported as a valid repo: {state:?}"
        );
    }

    #[test]
    fn detached_head_reports_no_branch_and_the_commit() {
        if !git_available() {
            return; // skip: git is not available
        }
        let (_dir, root) = make_repo(|root| {
            Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("checkout")
                .arg("--detach")
                .output()
                .expect("git checkout --detach");
        });
        let state = capture(&root);
        let repo = match &state {
            GitState::Repo {
                head,
                branch,
                detached,
                ..
            } => (head, branch, detached),
            other => panic!("expected a repository, got {other:?}"),
        };
        let (head, branch, detached) = repo;
        assert!(head.is_some(), "a detached head still has a commit");
        assert!(branch.is_none(), "detached has no branch");
        assert!(detached.is_some(), "detached reports the short commit");
    }

    #[test]
    fn the_sidecar_round_trips_and_is_versioned() {
        let dir = temp_dir();
        let state = GitState::Repo {
            head: Some("a".repeat(40)),
            branch: Some("main".to_string()),
            detached: None,
            staged: vec!["a.txt".to_string()],
            unstaged: vec!["b.txt".to_string()],
            untracked: vec!["c.txt".to_string()],
        };
        persist(&dir.0, &state).expect("persist the git state");
        let file = path(&dir.0);
        let raw = std::fs::read_to_string(&file).expect("read the sidecar");
        assert!(
            raw.contains("\"schema_version\": 1"),
            "the sidecar records the schema version"
        );
        assert_eq!(load(&dir.0), Some(state.clone()), "the sidecar round-trips");
        // A sidecar written by a newer/older schema is ignored (refreshed next sync).
        let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        value["schema_version"] = serde_json::json!(999);
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&value).expect("serialize"),
        )
        .expect("write");
        assert_eq!(
            load(&dir.0),
            None,
            "an incompatible schema version is ignored"
        );
    }

    #[test]
    fn two_roots_get_independent_git_states() {
        if !git_available() {
            return; // skip: git is not available
        }
        let (_repo_dir, repo_root) = make_repo(|_| {});
        let plain = temp_dir();
        assert!(capture(&repo_root).is_repo(), "the repo root is a repo");
        assert_eq!(
            capture(&plain.0),
            GitState::NotRepo,
            "the plain root is not a repo"
        );
        // Persisting one root's state must not leak into the other's sidecar.
        persist(
            &repo_root,
            &GitState::Repo {
                head: Some("b".repeat(40)),
                branch: Some("main".to_string()),
                detached: None,
                staged: vec![],
                unstaged: vec![],
                untracked: vec![],
            },
        )
        .expect("persist");
        assert_eq!(load(&plain.0), None, "the plain root has no sidecar");
        assert!(
            load(&repo_root).is_some(),
            "the repo root has its own sidecar"
        );
    }

    #[test]
    fn parse_status_splits_staged_unstaged_and_untracked() {
        let output = "M  staged.txt\n M unstaged.txt\nMM both.txt\n?? untracked.txt\nR  old.txt -> renamed.txt\n";
        let (staged, unstaged, untracked) = parse_status(output);
        assert_eq!(staged, vec!["staged.txt", "both.txt", "renamed.txt"]);
        assert_eq!(unstaged, vec!["unstaged.txt", "both.txt"]);
        assert_eq!(untracked, vec!["untracked.txt"]);
    }

    #[test]
    fn parse_status_excludes_the_index_directory() {
        let output = "?? .ukma\n?? .ukma/index.db\n M src/main.rs\n";
        let (staged, unstaged, untracked) = parse_status(output);
        assert!(staged.is_empty(), "no staged changes");
        assert_eq!(unstaged, vec!["src/main.rs"], "the user file is reported");
        assert!(untracked.is_empty(), "the `.ukma/` artifact is excluded");
    }
}
