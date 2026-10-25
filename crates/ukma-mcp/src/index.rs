//! Working-directory indexing (CodeGraph-style `auto-index cwd`): scan a
//! directory tree into a code-domain source snapshot with a deterministic
//! revision id, so the same tree always produces the same `commit_sha`.
//!
//! Honest boundaries: this is an in-memory snapshot of the *current* tree
//! (registered as a dirty snapshot — a worktree is never a clean commit), it
//! skips vendored/build trees and oversized files, and it makes no durability
//! or cross-process claim. The revision id is content-addressed: the first 40
//! hex characters of the sha256 over the sorted `(path, content)` pairs (a
//! full 40-char hex SHA, as `RepoRevision` requires).

use std::fs;
use std::io;
use std::path::Path;

use code_intelligence::domain::SourceFile;
use code_intelligence::service::InMemorySources;
use sha2::{Digest, Sha256};

/// Largest single file the scanner will read (256 KiB).
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Directory names the scanner never descends into (CodeGraph parity: build /
/// cache, IDE / IaC state and sensitive credential trees). Dot-prefixed
/// names are already skipped by the walk; the non-dot ones are listed here.
const SKIPPED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "vendor",
    ".venv",
    "venv",
    "dist",
    "build",
    "out",
    "__pycache__",
    "DerivedData",
    "Pods",
    "xcuserdata",
];

/// Directory-name prefixes the scanner never descends into.
const SKIPPED_DIR_PREFIXES: &[&str] = &["cmake-build-"];

/// File names / extensions the scanner never reads: binary artifacts, native
/// libraries, archives, OS metadata and secret files (CodeGraph parity —
/// defense in depth against indexing/embedding credentials).
const SKIPPED_FILES: &[&str] = &[
    "thumbs.db",
    "desktop.ini",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
];

/// File-name suffixes the scanner never reads.
const SKIPPED_FILE_SUFFIXES: &[&str] = &[
    // binary / native / object
    ".o", ".a", ".so", ".dylib", ".dll", ".lib", ".exe", ".bin", ".obj", ".pdb", ".tbd", ".la",
    // archives / compiled
    ".zip", ".tar", ".gz", ".tgz", ".bz2", ".xz", ".7z", ".rar", ".jar", ".war", ".class", ".pyc",
    ".wasm", // secrets
    ".pem", ".key", ".p12", ".pfx", ".crt", ".gpg", ".kdbx",
];

/// Whether a file name should be skipped (binary / archive / OS metadata /
/// secret). Case-insensitive.
fn is_skipped_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if SKIPPED_FILES.iter().any(|skipped| *skipped == lower) {
        return true;
    }
    SKIPPED_FILE_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// Knobs for a scan (CLI parity: `--exclude`, `--max-files`).
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Repo-relative path globs to skip (a file is skipped when its path
    /// matches any of them).
    pub excludes: Vec<String>,
    /// Cap on the number of files indexed (after the built-in skips).
    pub max_files: Option<usize>,
}

/// Minimal glob matcher (no external dependency). Segment-based: `**` matches
/// zero or more whole path segments, `*` matches zero or more non-`/`
/// characters, `?` matches one non-`/` character, and literals match
/// themselves. A pattern without wildcards matches when it is a substring of
/// the path (e.g. `Cargo.lock`, `tests/`).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return path.contains(pattern) || pattern == path;
    }
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    match_segments(&pattern_segments, 0, &path_segments, 0)
}

/// Match pattern segments against path segments (handles `**`).
fn match_segments(pattern: &[&str], pi: usize, path: &[&str], si: usize) -> bool {
    if pi == pattern.len() {
        return si == path.len();
    }
    let segment = pattern[pi];
    if segment == "**" {
        // `**` consumes zero or more path segments.
        for skip in si..=path.len() {
            if match_segments(pattern, pi + 1, path, skip) {
                return true;
            }
        }
        return false;
    }
    if si == path.len() {
        return false;
    }
    if glob_in_segment(segment, path[si]) {
        match_segments(pattern, pi + 1, path, si + 1)
    } else {
        false
    }
}

/// Match one pattern segment against one path segment (`*`/`?`, no `/`).
/// Classic linear-time backtracking: on a failed match after a `*`, extend the
/// run the `*` consumed by one character (so the string index always advances).
fn glob_in_segment(pattern: &str, segment: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = segment.chars().collect();
    let mut pi = 0usize;
    let mut si = 0usize;
    let mut star: Option<usize> = None;
    let mut star_si = 0usize;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_si = si;
            pi += 1;
        } else if let Some(star_pi) = star {
            // Extend the run the `*` consumed: move the string index forward.
            pi = star_pi + 1;
            si = star_si + 1;
            star_si = si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The outcome of one scan.
pub struct CwdIndex {
    /// The deterministic revision id (40-char hex, content-addressed).
    pub commit_sha: String,
    /// Number of files indexed.
    pub files: usize,
}

/// One indexed source tree: the source port plus the revision id to use in
/// requests.
pub struct IndexedSources {
    /// The snapshot port to hand to the code service.
    pub sources: InMemorySources,
    /// The revision id and file count of the scan.
    pub index: CwdIndex,
}

/// Scan a directory tree into source files with the default options.
pub fn scan_directory(root: &Path) -> io::Result<Vec<SourceFile>> {
    scan_with(root, &IndexOptions::default())
}

/// Scan a directory tree into source files (repo-relative, `/` separators,
/// sorted by path) honouring the exclude globs and the file cap.
pub fn scan_with(root: &Path, options: &IndexOptions) -> io::Result<Vec<SourceFile>> {
    let mut files: Vec<SourceFile> = Vec::new();
    walk(root, root, options, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    if let Some(cap) = options.max_files {
        files.truncate(cap);
    }
    Ok(files)
}

fn walk(
    root: &Path,
    current: &Path,
    options: &IndexOptions,
    files: &mut Vec<SourceFile>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.')
                || SKIPPED_DIRS.contains(&name.as_ref())
                || SKIPPED_DIR_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            walk(root, &path, options, files)?;
        } else if path.is_file() {
            if name.starts_with('.') || is_skipped_file(&name) {
                continue;
            }
            let metadata = fs::metadata(&path)?;
            if metadata.len() > MAX_FILE_BYTES {
                continue;
            }
            let content = fs::read_to_string(&path)?;
            let relative = path
                .strip_prefix(root)
                .expect("walked path is under the root")
                .to_string_lossy()
                .replace('\\', "/");
            if options
                .excludes
                .iter()
                .any(|glob| glob_match(glob, &relative))
            {
                continue;
            }
            files.push(SourceFile::new(&relative, &content));
        }
    }
    Ok(())
}

/// Index a directory tree with the default options.
pub fn index_directory(root: &Path) -> io::Result<IndexedSources> {
    index_with(root, &IndexOptions::default())
}

/// Index one or more directory trees: scan them (honouring the options),
/// merge and de-duplicate by path, compute the deterministic revision id and
/// register the result as a dirty snapshot (a worktree is never a clean
/// commit). For multiple roots the first root wins on a path collision.
pub fn index_multi(roots: &[&Path], options: &IndexOptions) -> io::Result<IndexedSources> {
    let mut merged: Vec<SourceFile> = Vec::new();
    for root in roots {
        for file in scan_with(root, options)? {
            if !merged.iter().any(|existing| existing.path == file.path) {
                merged.push(file);
            }
        }
    }
    merged.sort_by(|left, right| left.path.cmp(&right.path));
    let mut digest = Sha256::new();
    for file in &merged {
        digest.update(file.path.as_bytes());
        digest.update(b"\n");
        digest.update(file.content.as_bytes());
        digest.update(b"\n");
    }
    let count = merged.len();
    // A content-addressed 40-char hex revision (RepoRevision requires a
    // full 40-char hex SHA, so the id is the first 40 hex chars of the
    // tree digest).
    let commit_sha = hex_prefix(digest.finalize());
    let sources = InMemorySources::new().with_dirty_commit(&commit_sha, merged);
    Ok(IndexedSources {
        index: CwdIndex {
            commit_sha,
            files: count,
        },
        sources,
    })
}

/// Index a directory tree: scan it, compute the deterministic revision id and
/// register it as a dirty snapshot (a worktree is never a clean commit).
pub fn index_with(root: &Path, options: &IndexOptions) -> io::Result<IndexedSources> {
    index_multi(&[root], options)
}

/// A `(path, mtime-epoch)` signature over the trees, used by `--watch` to
/// detect changes without re-reading every file.
pub fn tree_signature(roots: &[&Path]) -> io::Result<Vec<(String, u64)>> {
    let mut signature: Vec<(String, u64)> = Vec::new();
    for root in roots {
        collect_signature(root, root, &mut signature)?;
    }
    signature.sort();
    Ok(signature)
}

fn collect_signature(
    root: &Path,
    current: &Path,
    signature: &mut Vec<(String, u64)>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.')
                || SKIPPED_DIRS.contains(&name.as_ref())
                || SKIPPED_DIR_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            collect_signature(root, &path, signature)?;
        } else if path.is_file() {
            if name.starts_with('.') || is_skipped_file(&name) {
                continue;
            }
            let metadata = fs::metadata(&path)?;
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|modified| {
                    use std::time::UNIX_EPOCH;
                    modified
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_secs())
                })
                .unwrap_or(0);
            let relative = path
                .strip_prefix(root)
                .expect("walked path is under the root")
                .to_string_lossy()
                .replace('\\', "/");
            signature.push((relative, mtime));
        }
    }
    Ok(())
}

/// The first 40 hex characters of the digest (a full 40-char hex SHA, as
/// `RepoRevision` requires).
fn hex_prefix(hash: impl AsRef<[u8]>) -> String {
    hash.as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(40)
        .collect()
}

/// The `@cwd` alias in a request's `revision` resolves to the indexed
/// revision. Both request shapes are handled: the object form
/// (`"revision": { "commit_sha": "@cwd", ... }`, e.g. `code.analyze_repository`)
/// and the bare-string form (`"revision": "@cwd"`, e.g. `code.find_symbols`).
/// Returns whether it substituted anything.
pub fn resolve_cwd_alias(input: &mut serde_json::Value, index: &CwdIndex) -> bool {
    let Some(revision) = input.get_mut("revision") else {
        return false;
    };
    match revision {
        serde_json::Value::String(text) if text == "@cwd" => {
            *revision = serde_json::Value::String(index.commit_sha.clone());
            true
        }
        serde_json::Value::Object(_) => {
            let Some(commit_sha) = revision.get_mut("commit_sha") else {
                return false;
            };
            if commit_sha.as_str() == Some("@cwd") {
                *commit_sha = serde_json::Value::String(index.commit_sha.clone());
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_intelligence::service::SourceSnapshotPort;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ukma-index-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the test tree");
        path
    }

    fn is_hex40(text: &str) -> bool {
        text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    #[test]
    fn a_scan_is_deterministic_and_skips_vendored_trees() {
        let root = temp_dir("deterministic");
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/stale.txt"), "skip me\n").unwrap();
        fs::write(root.join(".hidden.rs"), "skip me too\n").unwrap();

        let first = index_directory(&root).expect("scan the tree");
        let second = index_directory(&root).expect("scan the tree again");
        assert_eq!(
            first.index.commit_sha, second.index.commit_sha,
            "content-addressed"
        );
        assert!(
            is_hex40(&first.index.commit_sha),
            "the id is a 40-char hex SHA"
        );
        let files = scan_directory(&root).unwrap();
        assert_eq!(files.len(), 1, "vendored and hidden files are skipped");
        assert_eq!(files[0].path, "main.rs");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_scan_is_sorted_and_uses_forward_slashes() {
        let root = temp_dir("sorted");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("b.rs"), "b\n").unwrap();
        fs::write(root.join("src/a.rs"), "a\n").unwrap();
        let files = scan_directory(&root).unwrap();
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "src/a.rs"], "sorted, / separators");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_oversized_file_is_skipped() {
        let root = temp_dir("oversized");
        fs::write(
            root.join("big.txt"),
            "x".repeat(MAX_FILE_BYTES as usize + 1),
        )
        .unwrap();
        fs::write(root.join("small.txt"), "ok\n").unwrap();
        let files = scan_directory(&root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "small.txt");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_cwd_alias_resolves_to_the_indexed_revision() {
        let root = temp_dir("alias");
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        let indexed = index_directory(&root).unwrap();
        let mut request = serde_json::json!({
            "revision": { "commit_sha": "@cwd" }
        });
        assert!(resolve_cwd_alias(&mut request, &indexed.index));
        assert_eq!(request["revision"]["commit_sha"], indexed.index.commit_sha);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_cwd_alias_is_inert_without_a_matching_field() {
        let root = temp_dir("alias-inert");
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        let indexed = index_directory(&root).unwrap();
        let mut request = serde_json::json!({ "query": "hello" });
        assert!(!resolve_cwd_alias(&mut request, &indexed.index));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_directory_is_an_io_error() {
        let missing = std::env::temp_dir().join(format!("ukma-nope-{}", std::process::id()));
        assert!(index_directory(&missing).is_err());
    }

    #[test]
    fn the_glob_matcher_handles_wildcards() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("src/**", "src/a/b.rs"));
        assert!(glob_match("src/**", "src/a.rs"));
        assert!(glob_match("**/*.md", "docs/readme.md"));
        assert!(!glob_match("**/*.md", "docs/readme.txt"));
        assert!(glob_match("test_*.py", "test_foo.py"));
        assert!(!glob_match("test_*.py", "prod_foo.py"));
        // No wildcards: substring or exact.
        assert!(glob_match("Cargo.lock", "x/Cargo.lock"));
        assert!(glob_match("tests/", "tests/a.rs"));
        assert!(!glob_match("build/", "src/main.rs"));
    }

    #[test]
    fn exclude_globs_skip_matching_files() {
        let root = temp_dir("exclude");
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("src_main.rs"), "a\n").unwrap();
        fs::write(root.join("tests/it.rs"), "b\n").unwrap();
        fs::write(root.join("notes.md"), "c\n").unwrap();
        let options = IndexOptions {
            excludes: vec!["tests/**".to_string(), "*.md".to_string()],
            ..IndexOptions::default()
        };
        let files = scan_with(&root, &options).unwrap();
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, vec!["src_main.rs"], "excluded files are skipped");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_file_cap_truncates_the_scan() {
        let root = temp_dir("cap");
        fs::write(root.join("a.rs"), "a\n").unwrap();
        fs::write(root.join("b.rs"), "b\n").unwrap();
        fs::write(root.join("c.rs"), "c\n").unwrap();
        let options = IndexOptions {
            max_files: Some(2),
            ..IndexOptions::default()
        };
        let files = scan_with(&root, &options).unwrap();
        assert_eq!(files.len(), 2, "truncated to the cap");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn multiple_roots_merge_and_deduplicate_by_path() {
        let first = temp_dir("multi-a");
        let second = temp_dir("multi-b");
        fs::create_dir_all(first.join("shared")).unwrap();
        fs::create_dir_all(second.join("shared")).unwrap();
        fs::write(first.join("a.rs"), "from-a\n").unwrap();
        fs::write(first.join("shared/x.rs"), "from-a\n").unwrap();
        fs::write(second.join("b.rs"), "from-b\n").unwrap();
        fs::write(second.join("shared/x.rs"), "from-b\n").unwrap();
        let indexed = index_multi(&[&first, &second], &IndexOptions::default()).unwrap();
        let snapshot = indexed
            .sources
            .snapshot(&code_intelligence::domain::RepoRevision::git(
                "repo",
                &indexed.index.commit_sha,
            ))
            .unwrap();
        let paths: Vec<&str> = snapshot
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["a.rs", "b.rs", "shared/x.rs"],
            "merged and de-duplicated (first root wins)"
        );
        assert_eq!(indexed.index.files, 3);
        fs::remove_dir_all(&first).unwrap();
        fs::remove_dir_all(&second).unwrap();
    }

    #[test]
    fn the_comprehensive_exclusion_set_skips_build_binary_and_secret_files() {
        let root = temp_dir("exclusion-set");
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        // New non-dot build directories.
        fs::create_dir_all(root.join("dist")).unwrap();
        fs::write(root.join("dist/bundle.js"), "x\n").unwrap();
        fs::create_dir_all(root.join("out")).unwrap();
        fs::write(root.join("out/app.js"), "x\n").unwrap();
        fs::create_dir_all(root.join("__pycache__")).unwrap();
        fs::write(root.join("__pycache__/a.pyc"), "x\n").unwrap();
        fs::create_dir_all(root.join("cmake-build-debug")).unwrap();
        fs::write(root.join("cmake-build-debug/CMakeCache.txt"), "x\n").unwrap();
        // Binary / archive / native files.
        fs::write(root.join("lib.a"), "x\n").unwrap();
        fs::write(root.join("module.so"), "x\n").unwrap();
        fs::write(root.join("archive.zip"), "x\n").unwrap();
        // OS metadata + secret files.
        fs::write(root.join("thumbs.db"), "x\n").unwrap();
        fs::write(root.join("id_rsa"), "x\n").unwrap();
        fs::write(root.join("server.pem"), "x\n").unwrap();
        let files = scan_directory(&root).unwrap();
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["main.rs"],
            "only the real source file survives the exclusion set"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_tree_signature_changes_when_a_file_changes() {
        let root = temp_dir("signature");
        fs::write(root.join("a.rs"), "a\n").unwrap();
        let first = tree_signature(&[&root]).unwrap();
        // Touching the content changes the file set's content, but the
        // signature tracks mtime + path; rewriting the file updates mtime.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(root.join("a.rs"), "a-modified\n").unwrap();
        fs::write(root.join("b.rs"), "b\n").unwrap();
        let second = tree_signature(&[&root]).unwrap();
        assert_ne!(first, second, "adding a file changes the signature");
        fs::remove_dir_all(&root).unwrap();
    }
}
