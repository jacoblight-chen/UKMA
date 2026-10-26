//! `ukma` binary entry point.
//!
//! Modes (CodeGraph-style single binary):
//! - `ukma --mcp [--index-cwd <path>]` — serve the MCP stdio protocol
//! - `ukma --run-tool <name> [--tool-args json] [--index-cwd <path>]` —
//!   run one tool and exit
//! - `ukma <command>` — the command-line client (health, tools, call, ...)
//! - `ukma <setup>` — the setup commands (init, uninit, install, uninstall,
//!   upgrade, --version)

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match ukma_cli::parse(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("ukma: {error}");
            std::process::exit(ukma_cli::exit_code(&error));
        }
    };

    // Setup commands (and --help / --version) never index: they write
    // configuration and exit, so `ukma init` in a large repository does not pay
    // for a scan it does not use.
    if parsed.help || parsed.version || is_setup_command(&parsed) {
        let gateway = ukma_mcp::default_gateway();
        match ukma_cli::run(&gateway, &parsed, None) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("ukma: {error}");
                std::process::exit(ukma_cli::exit_code(&error));
            }
        }
        return;
    }

    // Index roots: the repeatable `--workspace` roots (with `--index-cwd` if
    // also given), else the single `--index-cwd` root, else none.
    let mut root_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(path) = parsed.index_cwd.clone() {
        root_paths.push(std::path::PathBuf::from(path));
    }
    for workspace in &parsed.workspaces {
        root_paths.push(std::path::PathBuf::from(workspace));
    }
    // A project prepared by `ukma init` supplies the roots when the command
    // line names none and the command actually reads the index, so a bare
    // `ukma --mcp` works per project without slowing `ukma tools` down.
    let project = if root_paths.is_empty() && wants_index(&parsed) {
        match ukma_cli::setup::project::discover(std::path::Path::new(".")) {
            Ok(project) => project,
            Err(error) => {
                eprintln!("ukma: ignoring the project manifest: {error}");
                None
            }
        }
    } else {
        None
    };
    if let Some(project) = &project {
        root_paths.extend(project.roots());
        eprintln!(
            "ukma: project {} ({})",
            project.root.display(),
            ukma_cli::setup::project::PROJECT_DIR
        );
    }
    let mut options = parsed.index_options();
    if let Some(project) = &project {
        options = project.index_options(&options);
    }
    let policy = parsed.search_policy();
    let (embedder, mode) = parsed.embedding_backend();

    // Cache-first: a project prepared by `ukma init` reuses `.ukma/index.db`,
    // refreshing only the files whose `(size, mtime)` moved. A multi-root
    // project caches its roots together (the manifest's root order is the merge
    // order), so it takes the same path.
    let mut cached: Option<ukma_cli::setup::cache::CachedSnapshot> = None;
    if let Some(project) = &project {
        if !parsed.no_cache {
            let roots = project.roots();
            let borrowed: Vec<&std::path::Path> =
                roots.iter().map(std::path::PathBuf::as_path).collect();
            match ukma_cli::setup::cache::load_roots(&borrowed, &options) {
                Ok(snapshot) => {
                    eprintln!(
                        "ukma: snapshot cache: {} read, {} reused{}",
                        snapshot.report.read,
                        snapshot.report.unchanged,
                        if snapshot.report.full {
                            " (full rescan)"
                        } else {
                            ""
                        }
                    );
                    cached = Some(snapshot);
                }
                Err(error) => {
                    eprintln!("ukma: snapshot cache unusable ({error}); rescanning");
                }
            }
        }
    }

    // `--watch` + `--mcp` (indexed): build a hot-swappable gateway and run the
    // watch loop on a background thread, so re-indexes are applied to the
    // *live* stdio MCP session (CodeGraph `--watch` parity, upgraded from
    // on-disk re-index to live hot-swap).
    if parsed.watch && parsed.mcp && !root_paths.is_empty() {
        let borrowed: Vec<&std::path::Path> = root_paths.iter().map(std::path::Path::new).collect();
        let hot = match &cached {
            // A watch session starts from the persisted snapshot: the tree was
            // already read once, so there is nothing to gain by scanning it again.
            Some(snapshot) => {
                let files = ukma_cli::setup::cache::source_files(&snapshot.files);
                let parse_cache: std::sync::Arc<dyn storage_ports::snapshot::ParseCache> =
                    snapshot.store.clone();
                let parser = std::sync::Arc::new(ukma_mcp::CachingParser::new(
                    std::sync::Arc::new(code_intelligence::parsers::engine::UnifiedParser::new()),
                    parse_cache,
                ));
                eprintln!(
                    "ukma: watch: starting from the snapshot cache (revision {})",
                    snapshot.revision
                );
                ukma_mcp::gateway_hot(ukma_mcp::HotSnapshot {
                    revision: snapshot.revision.clone(),
                    files,
                    roots: root_paths.clone(),
                    options: options.clone(),
                    policy,
                    embedder,
                    backend_mode: mode,
                    vuln: parsed.vulnerability_source(),
                    parser: Some(parser),
                })
            }
            None => ukma_mcp::gateway_with_indexed_embedding_hot(
                &borrowed,
                &options,
                policy,
                embedder,
                mode,
                parsed.vulnerability_source(),
            ),
        };
        let (gateway, handle) = match hot {
            Ok(pair) => pair,
            Err(error) => {
                eprintln!("ukma: {error}");
                std::process::exit(2);
            }
        };
        eprintln!(
            "ukma: live MCP session with hot-swap watch on {} root(s)",
            borrowed.len()
        );
        let handle = std::sync::Arc::new(handle);
        let watcher = std::sync::Arc::clone(&handle);
        std::thread::spawn(move || {
            ukma_mcp::watch_loop_hot(&watcher);
        });
        let probe = pending_probe(&project, &cached, &options);
        std::process::exit(ukma_mcp::run_stdio_profile_with_probe(
            &gateway,
            parsed.tool_profile(),
            probe,
        ));
    }

    if let Some(snapshot) = &cached {
        if let Ok(entries) = storage_ports::snapshot::ParseCache::len(snapshot.store.as_ref()) {
            eprintln!("ukma: parse cache: {entries} cached parse(s)");
        }
    }

    let mut parser_handle: Option<std::sync::Arc<ukma_mcp::CachingParser>> = None;
    let (gateway, index): (ukma_mcp::Gateway, Option<ukma_mcp::CwdIndex>) = if let Some(snapshot) =
        &cached
    {
        let files = ukma_cli::setup::cache::source_files(&snapshot.files);
        let parse_cache: std::sync::Arc<dyn storage_ports::snapshot::ParseCache> =
            snapshot.store.clone();
        match ukma_cli::setup::cache::gateway(
            &snapshot.revision,
            &files,
            policy,
            embedder,
            mode,
            parsed.vulnerability_source(),
            Some(parse_cache),
        ) {
            Ok(cached_gateway) => {
                parser_handle = cached_gateway.parser;
                (
                    cached_gateway.gateway,
                    Some(ukma_mcp::CwdIndex {
                        commit_sha: snapshot.revision.clone(),
                        files: snapshot.files.len(),
                    }),
                )
            }
            Err(error) => {
                eprintln!("ukma: {error}");
                std::process::exit(2);
            }
        }
    } else if root_paths.is_empty() {
        (ukma_mcp::default_gateway(), None)
    } else {
        let borrowed: Vec<&std::path::Path> = root_paths.iter().map(std::path::Path::new).collect();
        match ukma_mcp::gateway_with_indexed_embedding(
            &borrowed,
            &options,
            policy,
            embedder,
            mode,
            parsed.vulnerability_source(),
        ) {
            Ok((gateway, index)) => (gateway, Some(index)),
            Err(error) => {
                eprintln!("ukma: {error}");
                std::process::exit(2);
            }
        }
    };

    // Diagnostics go to stderr so stdout stays clean for piping tool output
    // (and, in --mcp mode, for the JSON-RPC protocol).
    if let Some(index) = &index {
        eprintln!(
            "ukma: indexed {} files at revision {}",
            index.files, index.commit_sha
        );
    }
    // Surface the effective embedding mode so the degradation is visible
    // (CodeGraph `--embedding-model` + RAM-gate parity).
    if let Some(model) = &parsed.embedding_model {
        eprintln!(
            "ukma: embedding model `{model}` -> mode {:?}",
            parsed.backend_mode()
        );
    }

    if parsed.mcp {
        let probe = pending_probe(&project, &cached, &options);
        let remote = if can_delegate(&parsed) {
            project
                .as_ref()
                .and_then(|project| ukma_cli::setup::daemon::running(&project.root))
                .map(|handle| {
                    std::sync::Arc::new(ukma_cli::setup::daemon::DaemonClient::new(handle))
                        as std::sync::Arc<dyn ukma_mcp::DaemonCall>
                })
        } else {
            None
        };
        if let Some(remote) = &remote {
            eprintln!("ukma: delegating tool calls to the project daemon");
            let _ = remote;
        }
        std::process::exit(ukma_mcp::run_stdio_with(
            &gateway,
            ukma_mcp::SessionOptions {
                profile: parsed.tool_profile(),
                probe,
                remote,
            },
        ));
    }

    let output = match ukma_cli::run(&gateway, &parsed, index.as_ref()) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("ukma: {error}");
            std::process::exit(ukma_cli::exit_code(&error));
        }
    };
    println!("{output}");

    // A one-shot command prints the envelope on stdout (machine-readable), so
    // the staleness warning goes to stderr.
    if let Some(probe) = pending_probe(&project, &cached, &options) {
        if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&output) {
            if let Some(banner) = ukma_mcp::banner(&probe.pending_for(&envelope)) {
                eprintln!("{banner}");
            }
        }
    }

    // The parse cache only pays off across processes, so report what this one
    // reused (stderr keeps stdout clean for piping).
    if let Some(parser) = &parser_handle {
        if parser.hits() + parser.misses() > 0 {
            eprintln!(
                "ukma: parse cache: {} reused, {} parsed",
                parser.hits(),
                parser.misses()
            );
        }
    }

    // `--watch` (non-MCP): after the one-shot command/tool, poll mtimes and
    // re-index on change until interrupted.
    if parsed.watch && !root_paths.is_empty() {
        let borrowed: Vec<&std::path::Path> = root_paths.iter().map(std::path::Path::new).collect();
        ukma_mcp::watch_loop(&borrowed, &options);
    }
}

/// Whether the command line is a setup command (they never index).
fn is_setup_command(parsed: &ukma_cli::Parsed) -> bool {
    !parsed.mcp
        && !parsed.watch
        && matches!(
            parsed.positional.first().map(String::as_str),
            Some(
                "init"
                    | "uninit"
                    | "install"
                    | "uninstall"
                    | "upgrade"
                    | "daemon"
                    | "status"
                    | "sync"
            )
        )
}

/// Whether this session may delegate its tool calls to the project daemon.
///
/// Only when every index/search option is the default the daemon serves:
/// delegation must never change an answer, only skip rebuilding it.
fn can_delegate(parsed: &ukma_cli::Parsed) -> bool {
    !parsed.no_cache
        && !parsed.watch
        && !parsed.online
        && parsed.excludes.is_empty()
        && parsed.max_files.is_none()
        && parsed.index_cwd.is_none()
        && parsed.workspaces.is_empty()
        && parsed.embedding_model.is_none()
        && parsed.split_identifiers.is_none()
        && parsed.full_body.is_none()
        && !parsed.graph_only
}

/// The staleness probe for this session: the snapshot statistics of the indexed
/// tree, used to warn when a response references a file that changed since.
///
/// Cheap by construction — the statistics come from the cache rows or from a
/// stat-only walk, never from reading files.
fn pending_probe(
    project: &Option<ukma_cli::setup::project::DiscoveredProject>,
    cached: &Option<ukma_cli::setup::cache::CachedSnapshot>,
    options: &ukma_mcp::index::IndexOptions,
) -> Option<std::sync::Arc<dyn ukma_mcp::PendingProbe>> {
    let roots = project.as_ref()?.roots();
    let borrowed: Vec<&std::path::Path> = roots.iter().map(std::path::PathBuf::as_path).collect();
    ukma_cli::setup::cache::pending_probe(&borrowed, cached.as_ref(), options)
}

/// Whether the command line reads an index (and so may use the project
/// manifest).
fn wants_index(parsed: &ukma_cli::Parsed) -> bool {
    parsed.mcp
        || parsed.run_tool.is_some()
        || matches!(parsed.positional.first().map(String::as_str), Some("call"))
}
