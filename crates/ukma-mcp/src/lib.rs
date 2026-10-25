//! UKMA gateway: registry-driven MCP tool + REST dispatch for the 108 domain
//! operations and the 4 system operations.
//!
//! Normative source: `docs/UKMA_v1.2.3_Full_Spec.md` §31.
//!
//! Properties this crate enforces:
//! - the operation registry is the only source of tools, routes and error sets;
//!   the manifest is projected from it and drift is reported, never hand-written;
//! - the gateway authenticates and injects the trusted scope — client-declared
//!   scopes are expectations only (§31.4);
//! - deny-by-default authorization, and an invisible object is reported as
//!   `NOT_FOUND` so existence never leaks across tenants (§40.7);
//! - a handler can never emit a code its operation did not declare: undeclared
//!   codes are normalised to `INTERNAL` with a warning.

pub mod alias;
pub mod context;
pub mod error;
pub mod gateway;
pub mod handlers;
pub mod index;
pub mod instant;
pub mod manifest;
pub mod rest;
pub mod sdk;
pub mod stdio;

#[cfg(feature = "osv")]
pub use code_intelligence::pro::OsVulnerabilitySource;
pub use code_intelligence::pro::{
    DependencyRef, OfflineVulnerabilitySource, OsVulnerability, VulnerabilitySource,
};
pub use code_intelligence::search::SearchPolicy;
use code_intelligence::service::SourceSnapshotPort;
pub use context::{visibility, visibility_error, OpContext, Visibility};
#[cfg(feature = "onnx")]
pub use embedding_engine::RealOnnxEmbedder;
pub use embedding_engine::{
    effective_mode, model_gate_passes, skip_memory_check, standard_registry, BackendMode,
    EmbeddingModel, EmbeddingPort, MemoryProbe, OnnxEmbedder, SystemMemoryProbe, RAM_GATE_BYTES,
};
pub use error::{DomainFailure, GatewayError};
pub use gateway::{Gateway, GatewayOutcome, Handler};
pub use handlers::{build, default_gateway, JobIndex, JobRecord, Services};
pub use index::{
    glob_match, index_directory, index_multi, resolve_cwd_alias, tree_signature, CwdIndex,
    IndexOptions, IndexedSources,
};
pub use manifest::{drift, manifest_json, tool_by_name, tools, Tool, ToolProfile};
pub use rest::{resolve, Route};
pub use sdk::{ApiError, Client, ClientError, InProcess, Transport};
pub use stdio::{run_stdio, run_stdio_profile, StdioSession};

/// Build a gateway whose code domain is bound to an indexed working
/// directory (CodeGraph-style `auto-index cwd`). Returns the gateway and the
/// revision id of the scan.
pub fn gateway_with_indexed_cwd(root: &std::path::Path) -> Result<(Gateway, CwdIndex), String> {
    gateway_with_indexed(&[root], &IndexOptions::default())
}

/// Build a gateway whose code domain is bound to one or more indexed
/// directory trees (CodeGraph-style `auto-index`), honouring the scan options
/// (`--exclude`, `--max-files`, `--profile`). Returns the gateway and the
/// revision id of the scan.
pub fn gateway_with_indexed(
    roots: &[&std::path::Path],
    options: &IndexOptions,
) -> Result<(Gateway, CwdIndex), String> {
    gateway_with_indexed_policy(
        roots,
        options,
        code_intelligence::search::SearchPolicy::default(),
    )
}

/// Like [`gateway_with_indexed`], but the code domain is built with an
/// explicit search / embedding policy (CodeGraph `--split-identifiers`,
/// `--full-body-embedding`, `--graph-only`).
pub fn gateway_with_indexed_policy(
    roots: &[&std::path::Path],
    options: &IndexOptions,
    policy: code_intelligence::search::SearchPolicy,
) -> Result<(Gateway, CwdIndex), String> {
    let mut indexed = index::index_multi(roots, options)
        .map_err(|error| format!("indexing {roots:?}: {error}"))?;
    // The `@cwd` alias stands for the scanned revision on every transport
    // (HTTP, CLI, MCP), not only in the CLI argument layer.
    indexed.sources = indexed
        .sources
        .with_alias("@cwd", &indexed.index.commit_sha);
    let sources = std::sync::Arc::new(indexed.sources);
    let revision = indexed.index.commit_sha.clone();
    let services = Services::with_code_sources_policy(sources.clone(), policy)
        .with_pro_from(sources, &revision, None);
    let gateway = build(&services)?;
    Ok((gateway, indexed.index))
}

/// Like [`gateway_with_indexed_policy`], but the code domain is also bound to
/// an embedding port + selected profile and the effective mode after the RAM
/// gate (CodeGraph `--embedding-model` + `CODEGRAPH_SKIP_MEMORY_CHECK` parity).
/// `vuln` is the optional OSV online source (`--online`); `None` is offline.
pub fn gateway_with_indexed_embedding(
    roots: &[&std::path::Path],
    options: &IndexOptions,
    policy: code_intelligence::search::SearchPolicy,
    embedder: Option<(std::sync::Arc<dyn EmbeddingPort>, String)>,
    backend_mode: BackendMode,
    vuln: Option<std::sync::Arc<dyn code_intelligence::pro::VulnerabilitySource>>,
) -> Result<(Gateway, CwdIndex), String> {
    let mut indexed = index::index_multi(roots, options)
        .map_err(|error| format!("indexing {roots:?}: {error}"))?;
    indexed.sources = indexed
        .sources
        .with_alias("@cwd", &indexed.index.commit_sha);
    let sources = std::sync::Arc::new(indexed.sources);
    let revision = indexed.index.commit_sha.clone();
    let services =
        Services::with_embedding_backend(sources.clone(), policy, embedder, backend_mode)
            .with_pro_from(sources, &revision, vuln);
    let gateway = build(&services)?;
    Ok((gateway, indexed.index))
}

/// Watch the trees by polling mtimes and re-indexing on change (CodeGraph-style
/// `--watch`). Blocks until the process is interrupted; diagnostics go to
/// stderr. Honest boundary: this re-indexes a *fresh* snapshot each time and
/// logs the new revision — it does not hot-swap a live gateway or MCP session.
pub fn watch_loop(roots: &[&std::path::Path], options: &IndexOptions) {
    let initial = match index::tree_signature(roots) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("ukma: watch: cannot read the tree: {error}");
            return;
        }
    };
    eprintln!(
        "ukma: watching {} root(s); re-indexing on change (Ctrl+C to stop)",
        roots.len()
    );
    let mut last = initial;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let current = match index::tree_signature(roots) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        if current == last {
            continue;
        }
        last = current;
        match index::index_multi(roots, options) {
            Ok(indexed) => eprintln!(
                "ukma: re-indexed {} files at revision {}",
                indexed.index.files, indexed.index.commit_sha
            ),
            Err(error) => eprintln!("ukma: re-index failed: {error}"),
        }
    }
}

/// A handle for hot-swapping an indexed gateway (CodeGraph `--watch`
/// hot-apply). `replace` re-indexes the trees and atomically swaps the new
/// snapshot into both the code domain (read on every call) and the Pro corpus.
pub struct SwappableIndexHandle {
    sources: std::sync::Arc<code_intelligence::service::SwappableSources>,
    pro: std::sync::Arc<code_intelligence::pro::ProService>,
    roots: Vec<std::path::PathBuf>,
    options: IndexOptions,
}

impl SwappableIndexHandle {
    /// Read the current (possibly hot-swapped) snapshot for a revision.
    pub fn snapshot(
        &self,
        revision: &code_intelligence::domain::RepoRevision,
    ) -> code_intelligence::CodeResult<code_intelligence::domain::SourceSnapshot> {
        self.sources.snapshot(revision)
    }

    /// Re-index the trees and atomically swap the result into the live
    /// gateway. Returns the new revision id.
    pub fn replace(&self) -> Result<CwdIndex, String> {
        let roots_refs: Vec<&std::path::Path> =
            self.roots.iter().map(std::path::Path::new).collect();
        let indexed = index::index_multi(&roots_refs, &self.options)
            .map_err(|error| format!("re-index: {error}"))?;
        let files = indexed
            .sources
            .files_for(&indexed.index.commit_sha)
            .unwrap_or_default();
        let new_sources = indexed
            .sources
            .with_alias("@cwd", &indexed.index.commit_sha);
        // 1) Swap the code-domain snapshot (visible on the next call).
        self.sources.replace(new_sources);
        // 2) Re-index the Pro corpus from the new files.
        self.pro
            .index(&indexed.index.commit_sha, files, Vec::new())
            .expect("pro corpus indexing cannot fail for an empty symbol set");
        Ok(indexed.index)
    }
}

/// Like [`gateway_with_indexed_embedding`], but the code domain is bound to a
/// [`code_intelligence::service::SwappableSources`] so `--watch` can hot-swap
/// the index into the live gateway. Returns the gateway plus a handle that
/// atomically re-indexes and swaps.
pub fn gateway_with_indexed_embedding_hot(
    roots: &[&std::path::Path],
    options: &IndexOptions,
    policy: code_intelligence::search::SearchPolicy,
    embedder: Option<(std::sync::Arc<dyn EmbeddingPort>, String)>,
    backend_mode: BackendMode,
    vuln: Option<std::sync::Arc<dyn code_intelligence::pro::VulnerabilitySource>>,
) -> Result<(Gateway, SwappableIndexHandle), String> {
    let indexed = index::index_multi(roots, options)
        .map_err(|error| format!("indexing {roots:?}: {error}"))?;
    let initial_sources = indexed
        .sources
        .with_alias("@cwd", &indexed.index.commit_sha);
    let files = initial_sources
        .files_for(&indexed.index.commit_sha)
        .unwrap_or_default();
    let revision = indexed.index.commit_sha.clone();
    let swappable = std::sync::Arc::new(code_intelligence::service::SwappableSources::new(
        initial_sources,
    ));
    let mut pro = code_intelligence::pro::ProService::new();
    if let Some(source) = vuln {
        pro = pro.with_vulnerability_source(source);
    }
    let pro = std::sync::Arc::new(pro);
    pro.index(&revision, files, Vec::new())
        .expect("pro corpus indexing cannot fail for an empty symbol set");
    let services =
        Services::with_embedding_backend(swappable.clone(), policy, embedder, backend_mode)
            .with_pro(std::sync::Arc::clone(&pro));
    let gateway = build(&services)?;
    let handle = SwappableIndexHandle {
        sources: swappable,
        pro,
        roots: roots.iter().map(|r| r.to_path_buf()).collect(),
        options: options.clone(),
    };
    Ok((gateway, handle))
}

/// Watch the trees by polling mtimes and, on change, hot-swap the re-indexed
/// snapshot into a live gateway (CodeGraph `--watch` hot-apply). Blocks until
/// the process is interrupted; diagnostics go to stderr.
pub fn watch_loop_hot(handle: &SwappableIndexHandle) {
    let roots: Vec<&std::path::Path> = handle.roots.iter().map(std::path::Path::new).collect();
    let initial = match index::tree_signature(&roots) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("ukma: watch: cannot read the tree: {error}");
            return;
        }
    };
    eprintln!(
        "ukma: watching {} root(s); hot-swapping the re-index into the live session (Ctrl+C to stop)",
        roots.len()
    );
    let mut last = initial;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let current = match index::tree_signature(&roots) {
            Ok(signature) => signature,
            Err(_) => continue,
        };
        if current == last {
            continue;
        }
        last = current;
        match handle.replace() {
            Ok(index) => eprintln!(
                "ukma: hot-swapped {} files at revision {}",
                index.files, index.commit_sha
            ),
            Err(error) => eprintln!("ukma: hot-swap re-index failed: {error}"),
        }
    }
}

#[cfg(test)]
mod hot_swap_tests {
    use super::*;
    use code_intelligence::domain::{RepoRevision, SourceFile};
    use code_intelligence::service::{
        CodeAuthorizer, InMemorySources, ScopeAuthorizer, SourceSnapshotPort, SwappableSources,
        SCOPE_READ,
    };
    use std::fs;
    use std::path::PathBuf;
    use ukma_contracts::{Consistency, RequestContext, ScopeRef};

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ukma-hot-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the test tree");
        path
    }

    fn ctx(scopes: Vec<&str>) -> RequestContext {
        RequestContext {
            request_id: "req".into(),
            trace_id: "trace".into(),
            scope: ScopeRef {
                tenant_id: "t".into(),
                project_id: "p".into(),
                repo_id: None,
                workspace_id: None,
                agent_id: None,
                session_id: None,
            },
            principal_id: "principal".into(),
            scopes: scopes.into_iter().map(String::from).collect(),
            deadline_at: None,
            consistency: Consistency::ReadYourWrites,
            required_generation: None,
            snapshot_id: None,
            idempotency_key: None,
            expected_etag: None,
            delegation_token: None,
        }
    }

    fn sources_with(sha: &str, content: &str) -> InMemorySources {
        InMemorySources::new()
            .with_commit(sha, vec![SourceFile::new("main.rs", content)])
            .with_alias("@cwd", sha)
    }

    /// Positive: a re-index + hot-swap is visible to a subsequent read of the
    /// live (swappable) index.
    #[test]
    fn positive_a_hot_swap_is_visible_to_subsequent_reads() {
        let root = temp_dir("positive");
        fs::write(root.join("main.rs"), "v1\n").unwrap();
        let options = IndexOptions::default();
        let roots = [root.as_path()];
        let (_gateway, handle) = gateway_with_indexed_embedding_hot(
            &roots,
            &options,
            SearchPolicy::default(),
            None,
            BackendMode::GraphOnly,
            None,
        )
        .expect("build the hot gateway");
        let before = handle
            .snapshot(&RepoRevision::git("repo", "@cwd"))
            .expect("read the initial snapshot");
        assert_eq!(before.files[0].content, "v1\n");
        // Mutate the tree and hot-swap.
        fs::write(root.join("main.rs"), "v2\n").unwrap();
        let swapped = handle.replace().expect("re-index and swap");
        let after = handle
            .snapshot(&RepoRevision::git("repo", "@cwd"))
            .expect("read the swapped snapshot");
        assert_eq!(after.files[0].content, "v2\n", "the swap is visible");
        assert_eq!(after.commit_sha, swapped.commit_sha);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Negative: after a swap replaces the snapshot set, the superseded
    /// revision is no longer resolvable.
    #[test]
    fn negative_a_superseded_revision_is_no_longer_resolvable() {
        let old = "0123456789abcdef0123456789abcdef01234567";
        let new = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let swappable = SwappableSources::new(sources_with(old, "v1"));
        assert!(swappable.snapshot(&RepoRevision::git("repo", old)).is_ok());
        swappable.replace(sources_with(new, "v2"));
        assert!(
            swappable.snapshot(&RepoRevision::git("repo", old)).is_err(),
            "the superseded revision is gone"
        );
    }

    /// Authorization: reading a snapshot requires `code:read`; deny by default.
    #[test]
    fn authorization_reading_requires_the_code_read_scope() {
        let authorizer = ScopeAuthorizer;
        assert!(
            authorizer.require_scope(&ctx(vec![]), SCOPE_READ).is_err(),
            "no scope -> denied"
        );
        assert!(
            authorizer
                .require_scope(&ctx(vec!["code:read"]), SCOPE_READ)
                .is_ok(),
            "with the scope -> allowed"
        );
    }

    /// Version: `resolve("@cwd")` returns the concrete (new) commit sha after a
    /// swap, so the `@cwd` alias always names the current revision.
    #[test]
    fn version_the_cwd_alias_tracks_the_swapped_revision() {
        let swappable = SwappableSources::new(sources_with("aaa", "v1"));
        assert_eq!(swappable.resolve("@cwd"), "aaa");
        swappable.replace(sources_with("bbb", "v2"));
        assert_eq!(swappable.resolve("@cwd"), "bbb");
    }

    /// Failure: reading a revision that was never registered is an error.
    #[test]
    fn failure_an_unregistered_revision_is_an_error() {
        let swappable = SwappableSources::new(InMemorySources::new());
        let unregistered = "1111111111111111111111111111111111111111";
        assert!(swappable
            .snapshot(&RepoRevision::git("repo", unregistered))
            .is_err());
    }
}
