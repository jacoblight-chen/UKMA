//! Cache-first startup: the `.ukma/index.db` snapshot in front of the gateway.
//!
//! `ukma --mcp` (and `--run-tool` / `call`) in a project prepared by `ukma init`
//! reuse the persisted snapshot: `sync` re-reads only the files whose
//! `(size, mtime)` moved, and the gateway is built from the cached rows. A
//! multi-root project caches all of its roots together. Only `--no-cache` (or an
//! unusable cache) falls back to the in-memory scanner.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use code_intelligence::domain::SourceFile;
use code_intelligence::service::InMemorySources;
use ukma_mcp::index::IndexOptions;

use super::project::PROJECT_DIR;
use super::SetupError;

/// The cache file name inside the project directory.
pub const INDEX_DB: &str = "index.db";

/// The cache path of a project.
pub fn index_db_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR).join(INDEX_DB)
}

/// Open (creating if absent) the snapshot cache of a project.
pub fn open_store(
    root: &Path,
) -> Result<ukma_adapter_sqlite::snapshot::SqliteSnapshotCache, SetupError> {
    let path = index_db_path(root);
    ukma_adapter_sqlite::snapshot::SqliteSnapshotCache::open(&path).map_err(|error| {
        SetupError::Cache {
            path: path.clone(),
            detail: error.to_string(),
        }
    })
}

/// Delete the cache (including its WAL sidecars) and open a fresh one.
///
/// Used by `ukma sync --force` and by `ukma init` when the existing file is
/// unusable: the cache is not a system of record, so rebuilding it loses
/// nothing but work.
pub fn recreate(
    root: &Path,
) -> Result<ukma_adapter_sqlite::snapshot::SqliteSnapshotCache, SetupError> {
    let path = index_db_path(root);
    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = path.clone().into_os_string();
        candidate.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(candidate));
    }
    open_store(root)
}

/// A snapshot loaded from the cache.
#[derive(Clone)]
pub struct CachedSnapshot {
    /// The revision the rows add up to.
    pub revision: String,
    /// The merged snapshot rows.
    pub files: Vec<storage_ports::snapshot::FileRow>,
    /// What the refresh did.
    pub report: super::snapshot::SyncReport,
    /// The open cache (snapshot rows **and** parse results).
    pub store: Arc<ukma_adapter_sqlite::snapshot::SqliteSnapshotCache>,
}

impl CachedSnapshot {
    /// The number of files in the snapshot.
    pub fn files_count(&self) -> usize {
        self.files.len()
    }
}

impl std::fmt::Debug for CachedSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedSnapshot")
            .field("revision", &self.revision)
            .field("files", &self.files.len())
            .field("report", &self.report)
            .finish()
    }
}

/// Refresh the cache from the working tree and return the snapshot.
pub fn load(root: &Path, options: &IndexOptions) -> Result<CachedSnapshot, SetupError> {
    load_roots(&[root], options)
}

/// Refresh the cache from an ordered root list (a project may have several).
pub fn load_roots(roots: &[&Path], options: &IndexOptions) -> Result<CachedSnapshot, SetupError> {
    let root = roots.first().copied().unwrap_or_else(|| Path::new("."));
    let store = Arc::new(open_store(root)?);
    let (report, files) =
        super::snapshot::sync_snapshot_roots(roots, options, store.as_ref(), false).map_err(
            |error| SetupError::Cache {
                path: index_db_path(root),
                detail: error.to_string(),
            },
        )?;
    Ok(CachedSnapshot {
        revision: report.revision.clone(),
        files,
        report,
        store,
    })
}

/// The cached rows as code-domain source files.
pub fn source_files(rows: &[storage_ports::snapshot::FileRow]) -> Vec<SourceFile> {
    rows.iter()
        .map(|row| SourceFile::new(&row.path, &row.content))
        .collect()
}

/// The staleness probe for a session over `roots`.
///
/// Cheap by construction — the statistics come from the cached rows or from a
/// stat-only walk, never from reading files. `None` when the tree has no
/// snapshot to compare against.
pub fn pending_probe(
    roots: &[&Path],
    snapshot: Option<&CachedSnapshot>,
    options: &IndexOptions,
) -> Option<Arc<dyn ukma_mcp::PendingProbe>> {
    let root = roots.first().copied()?.to_path_buf();
    match snapshot {
        Some(snapshot) => {
            // The cached rows only keep repo-relative paths, so the absolute
            // path of each one comes from a stat-only walk (no file is read).
            let stats: ukma_mcp::SnapshotStat = snapshot
                .files
                .iter()
                .map(|row| (row.path.clone(), (row.size, row.mtime_ns)))
                .collect();
            if stats.is_empty() {
                return None;
            }
            let paths = super::snapshot::stat_map_roots(roots, options).unwrap_or_default();
            let absolute = paths
                .into_iter()
                .map(|(path, (absolute, _, _))| (path, absolute))
                .collect();
            Some(Arc::new(ukma_mcp::SnapshotProbe::with_paths(
                root, stats, absolute,
            )))
        }
        None => {
            let stats = super::snapshot::stat_map_roots(roots, options).ok()?;
            if stats.is_empty() {
                return None;
            }
            let recorded: ukma_mcp::SnapshotStat = stats
                .iter()
                .map(|(path, (_, size, mtime))| (path.clone(), (*size, *mtime)))
                .collect();
            let absolute = stats
                .into_iter()
                .map(|(path, (absolute, _, _))| (path, absolute))
                .collect();
            Some(Arc::new(ukma_mcp::SnapshotProbe::with_paths(
                root, recorded, absolute,
            )))
        }
    }
}

/// A gateway built from a cached snapshot, plus the parse cache in front of it.
pub struct CachedGateway {
    /// The gateway to serve.
    pub gateway: ukma_mcp::Gateway,
    /// The parse cache decorator, when one was wired (for statistics).
    pub parser: Option<Arc<ukma_mcp::CachingParser>>,
}

/// Build a gateway bound to an already-materialized snapshot.
///
/// This mirrors what [`ukma_mcp::gateway_with_indexed_embedding`] does after its
/// own scan, with two substitutions: the snapshot comes from the cache, and the
/// parser is a [`ukma_mcp::CachingParser`] over the same database, so an
/// unchanged file is never parsed twice across processes.
pub fn gateway(
    revision: &str,
    files: &[SourceFile],
    policy: ukma_mcp::SearchPolicy,
    embedder: Option<(Arc<dyn ukma_mcp::EmbeddingPort>, String)>,
    backend_mode: ukma_mcp::BackendMode,
    vuln: Option<Arc<dyn ukma_mcp::VulnerabilitySource>>,
    parse_cache: Option<Arc<dyn storage_ports::snapshot::ParseCache>>,
) -> Result<CachedGateway, String> {
    let sources = Arc::new(
        InMemorySources::new()
            .with_dirty_commit(revision, files.to_vec())
            .with_alias("@cwd", revision),
    );
    let parser = parse_cache.map(|cache| {
        Arc::new(ukma_mcp::CachingParser::new(
            Arc::new(code_intelligence::parsers::engine::UnifiedParser::new()),
            cache,
        ))
    });
    let services = ukma_mcp::Services::with_embedding_backend_and_parser(
        sources.clone(),
        policy,
        embedder,
        backend_mode,
        parser
            .clone()
            .map(|parser| parser as Arc<dyn code_intelligence::ParserPort>),
    )
    .with_pro_from(sources, revision, vuln);
    Ok(CachedGateway {
        gateway: ukma_mcp::build(&services)?,
        parser,
    })
}
