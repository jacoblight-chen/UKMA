//! Staleness disclosure: tell the agent when a file it is about to trust has
//! changed since the index snapshot.
//!
//! The index is a **snapshot of the working tree at session start** (see
//! [`crate::index`]). Between that scan and a tool call the user (or the agent
//! itself) may edit a file, and no amount of indexing can close that window —
//! so this module makes it *visible* instead of silent: a response that
//! references a changed file is prefixed with an explicit warning naming the
//! file and telling the model to read it directly.
//!
//! The probe is deliberately cheap and precise: it only looks at the paths the
//! response actually mentions, stats those files, and compares against the
//! snapshot's recorded `(size, mtime)`. Files the response does not mention are
//! never stat-ed, so a tool call never pays for a tree walk.
//!
//! Because the check is stat-based, an edit that keeps the size **and** restores
//! the timestamp looks unchanged here. That is deliberate (hashing every
//! referenced file on every call would cost more than the walk this avoids) and
//! it is closable on demand: `ukma sync --verify` / `ukma status --verify` read
//! every file and compare content hashes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One file that changed after the index snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFile {
    /// The repo-relative path.
    pub path: String,
    /// Why it is considered pending (already human-readable).
    pub reason: String,
}

/// The `(size, mtime_ns)` the snapshot was built from.
pub type SnapshotStat = BTreeMap<String, (u64, u64)>;

/// Detects staleness for the files a response references.
pub trait PendingProbe: Send + Sync {
    /// The referenced-but-changed files, in path order. Empty when the response
    /// only touches files the snapshot still matches.
    fn pending_for(&self, response: &serde_json::Value) -> Vec<PendingFile>;
}

/// A [`PendingProbe`] over one indexed tree (or an ordered root list).
#[derive(Debug, Clone)]
pub struct SnapshotProbe {
    root: PathBuf,
    stats: SnapshotStat,
    absolute: BTreeMap<String, PathBuf>,
}

impl SnapshotProbe {
    /// Probe `root` against the snapshot statistics.
    pub fn new(root: impl Into<PathBuf>, stats: SnapshotStat) -> Self {
        Self {
            root: root.into(),
            stats,
            absolute: BTreeMap::new(),
        }
    }

    /// Probe a multi-root snapshot: `absolute` maps each repo-relative path to
    /// the file it came from (a project can have several roots, so the path is
    /// not always relative to one directory).
    pub fn with_paths(
        root: impl Into<PathBuf>,
        stats: SnapshotStat,
        absolute: BTreeMap<String, PathBuf>,
    ) -> Self {
        Self {
            root: root.into(),
            stats,
            absolute,
        }
    }

    /// The number of files in the snapshot (diagnostics).
    pub fn len(&self) -> usize {
        self.stats.len()
    }

    /// Whether the snapshot is empty.
    pub fn is_empty(&self) -> bool {
        self.stats.is_empty()
    }

    /// The file a referenced path names.
    fn resolve(&self, path: &str) -> PathBuf {
        self.absolute
            .get(path)
            .cloned()
            .unwrap_or_else(|| self.root.join(path))
    }
}

impl PendingProbe for SnapshotProbe {
    fn pending_for(&self, response: &serde_json::Value) -> Vec<PendingFile> {
        let mut referenced: Vec<&str> = Vec::new();
        collect_paths(response, &self.stats, &mut referenced);
        referenced.sort_unstable();
        referenced.dedup();

        let mut pending = Vec::new();
        for path in referenced {
            let Ok(metadata) = std::fs::metadata(self.resolve(path)) else {
                pending.push(PendingFile {
                    path: path.to_string(),
                    reason: "deleted after the index snapshot".into(),
                });
                continue;
            };
            let recorded = self.stats.get(path).copied().unwrap_or_default();
            if (metadata.len(), mtime_ns(&metadata)) == recorded {
                continue;
            }
            pending.push(PendingFile {
                path: path.to_string(),
                reason: format!(
                    "edited {} ago, pending sync",
                    human_age(metadata.modified().ok())
                ),
            });
        }
        pending
    }
}

/// The warning prepended to a response, or `None` when nothing is pending.
pub fn banner(pending: &[PendingFile]) -> Option<String> {
    if pending.is_empty() {
        return None;
    }
    let mut text = String::from(
        "\u{26a0}\u{fe0f} Some files referenced below changed after the index snapshot — \
         their entries may be stale:\n",
    );
    for file in pending {
        text.push_str(&format!("  - {} ({})\n", file.path, file.reason));
    }
    text.push_str(
        "For accurate content of those specific files, Read them directly.\n\
         The rest of this response is fresh.",
    );
    Some(text)
}

/// Collect the response strings that name a snapshot file.
///
/// Both the bare path (`src/a.rs`) and the `path:line` form (`src/a.rs:12`) are
/// recognised, which is what symbol locations and diagnostics use.
fn collect_paths<'a>(value: &'a serde_json::Value, stats: &SnapshotStat, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(text) => {
            if stats.contains_key(text.as_str()) {
                out.push(text.as_str());
                return;
            }
            if let Some((head, _)) = text.split_once(':') {
                if stats.contains_key(head) {
                    out.push(head);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_paths(item, stats, out);
            }
        }
        serde_json::Value::Object(map) => {
            for nested in map.values() {
                collect_paths(nested, stats, out);
            }
        }
        _ => {}
    }
}

fn mtime_ns(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

/// `800ms` / `1.2s` / `3m` — coarse on purpose: the number tells the agent
/// "recently", not "at 12:04:33.812".
fn human_age(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "an unknown time".into();
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    let millis = age.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if age.as_secs() < 60 {
        format!("{:.1}s", age.as_secs_f64())
    } else if age.as_secs() < 3_600 {
        format!("{}m", age.as_secs() / 60)
    } else {
        format!("{}h", age.as_secs() / 3_600)
    }
}

/// The snapshot statistics of the tree, from a `(path, size, mtime_ns)` walk.
pub fn stats_from_rows(rows: &[(String, u64, u64)]) -> SnapshotStat {
    rows.iter()
        .map(|(path, size, mtime)| (path.clone(), (*size, *mtime)))
        .collect()
}

/// Whether a path exists in the snapshot (test helper).
pub fn contains(stats: &SnapshotStat, path: &Path) -> bool {
    stats.contains_key(&path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ukma-pending-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create tree");
        path
    }

    fn snapshot(root: &Path, files: &[&str]) -> SnapshotStat {
        files
            .iter()
            .map(|relative| {
                let metadata = std::fs::metadata(root.join(relative)).expect("metadata");
                (
                    (*relative).to_string(),
                    (metadata.len(), mtime_ns(&metadata)),
                )
            })
            .collect()
    }

    #[test]
    fn an_unchanged_reference_is_not_pending() {
        let root = tree("clean");
        std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let probe = SnapshotProbe::new(&root, snapshot(&root, &["a.rs"]));
        let response =
            json!({ "data": { "symbols": [{ "definition": { "path": "a.rs", "line": 1 } }] } });
        assert!(probe.pending_for(&response).is_empty());
        assert_eq!(banner(&[]), None);
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_changed_reference_is_pending_with_a_reason() {
        let root = tree("changed");
        std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let stats = snapshot(&root, &["a.rs"]);
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(root.join("a.rs"), "fn a() { a(); }\n").expect("edit");
        let probe = SnapshotProbe::new(&root, stats);
        let response = json!({ "data": { "path": "a.rs" } });
        let pending = probe.pending_for(&response);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "a.rs");
        assert!(
            pending[0].reason.contains("pending sync"),
            "{}",
            pending[0].reason
        );
        let banner = banner(&pending).expect("a banner");
        assert!(banner.starts_with("\u{26a0}\u{fe0f}"));
        assert!(banner.contains("  - a.rs (edited "), "{banner}");
        assert!(banner.contains("Read them directly"), "{banner}");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_deleted_reference_is_pending() {
        let root = tree("deleted");
        std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let stats = snapshot(&root, &["a.rs"]);
        std::fs::remove_file(root.join("a.rs")).expect("delete");
        let probe = SnapshotProbe::new(&root, stats);
        let pending = probe.pending_for(&json!({ "path": "a.rs" }));
        assert_eq!(pending.len(), 1);
        assert!(pending[0].reason.contains("deleted"), "{:?}", pending);
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn an_unreferenced_changed_file_is_not_reported() {
        let root = tree("unreferenced");
        std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        std::fs::write(root.join("b.rs"), "fn b() {}\n").expect("write");
        let stats = snapshot(&root, &["a.rs", "b.rs"]);
        std::fs::write(root.join("b.rs"), "fn b() { b(); }\n").expect("edit");
        let probe = SnapshotProbe::new(&root, stats);
        let pending = probe.pending_for(&json!({ "path": "a.rs" }));
        assert!(
            pending.is_empty(),
            "only referenced files are checked (no tree walk per call)"
        );
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_location_string_is_recognised() {
        let root = tree("location");
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").expect("write");
        let stats = snapshot(&root, &["src/a.rs"]);
        std::fs::write(root.join("src/a.rs"), "fn a() { a(); }\n").expect("edit");
        let probe = SnapshotProbe::new(&root, stats);
        let pending = probe.pending_for(&json!({ "definition": "src/a.rs:1" }));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "src/a.rs");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_path_that_is_not_in_the_snapshot_is_ignored() {
        let root = tree("unknown");
        std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
        let probe = SnapshotProbe::new(&root, snapshot(&root, &["a.rs"]));
        let pending = probe.pending_for(&json!({ "path": "elsewhere.rs" }));
        assert!(pending.is_empty());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn the_age_is_human_readable() {
        assert_eq!(human_age(None), "an unknown time");
        assert_eq!(human_age(Some(SystemTime::now())), "0ms");
        assert!(human_age(Some(SystemTime::now() - Duration::from_secs(90))).ends_with('m'));
        assert!(human_age(Some(SystemTime::now() - Duration::from_secs(7_200))).ends_with('h'));
    }
}
