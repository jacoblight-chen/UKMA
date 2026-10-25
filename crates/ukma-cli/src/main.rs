//! `ukma` binary entry point.
//!
//! Modes (CodeGraph-style single binary):
//! - `ukma --mcp [--index-cwd <path>]` — serve the MCP stdio protocol
//! - `ukma --run-tool <name> [--tool-args json] [--index-cwd <path>]` —
//!   run one tool and exit
//! - `ukma <command>` — the command-line client (health, tools, call, ...)

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match ukma_cli::parse(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("ukma: {error}");
            std::process::exit(ukma_cli::exit_code(&error));
        }
    };

    // Index roots: the repeatable `--workspace` roots (with `--index-cwd` if
    // also given), else the single `--index-cwd` root, else none.
    let mut root_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(path) = parsed.index_cwd.clone() {
        root_paths.push(std::path::PathBuf::from(path));
    }
    for workspace in &parsed.workspaces {
        root_paths.push(std::path::PathBuf::from(workspace));
    }
    let options = parsed.index_options();
    let policy = parsed.search_policy();
    let (embedder, mode) = parsed.embedding_backend();
    // `--watch` + `--mcp` (indexed): build a hot-swappable gateway and run the
    // watch loop on a background thread, so re-indexes are applied to the
    // *live* stdio MCP session (CodeGraph `--watch` parity, upgraded from
    // on-disk re-index to live hot-swap).
    if parsed.watch && parsed.mcp && !root_paths.is_empty() {
        let borrowed: Vec<&std::path::Path> = root_paths.iter().map(std::path::Path::new).collect();
        let (gateway, handle) = match ukma_mcp::gateway_with_indexed_embedding_hot(
            &borrowed,
            &options,
            policy,
            embedder,
            mode,
            parsed.vulnerability_source(),
        ) {
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
        std::process::exit(ukma_mcp::run_stdio_profile(&gateway, parsed.tool_profile()));
    }

    let (gateway, index): (ukma_mcp::Gateway, Option<ukma_mcp::CwdIndex>) = if root_paths.is_empty()
    {
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
        std::process::exit(ukma_mcp::run_stdio_profile(&gateway, parsed.tool_profile()));
    }

    match ukma_cli::run(&gateway, &parsed, index.as_ref()) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("ukma: {error}");
            std::process::exit(ukma_cli::exit_code(&error));
        }
    }

    // `--watch` (non-MCP): after the one-shot command/tool, poll mtimes and
    // re-index on change until interrupted.
    if parsed.watch && !root_paths.is_empty() {
        let borrowed: Vec<&std::path::Path> = root_paths.iter().map(std::path::Path::new).collect();
        ukma_mcp::watch_loop(&borrowed, &options);
    }
}
