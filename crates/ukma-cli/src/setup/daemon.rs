//! The project daemon: one long-lived process per project that keeps the
//! snapshot warm and serves tool calls to every session.
//!
//! Why it exists: the snapshot and the parse cache are already shared through
//! `.ukma/index.db`, but each `ukma --mcp` session still builds its own in-memory
//! code index and runs its own freshness loop. The daemon centralises both — it
//! watches the tree (an OS event source where the platform has one), refreshes
//! the cache incrementally, and answers `call` requests from one warm gateway, so
//! two agents in the same project share the same derived state instead of
//! duplicating it. Connections are served on their own threads: a slow or silent
//! client never delays the others.
//!
//! Transport: **loopback TCP + a per-project bearer token**, chosen because it
//! needs no new dependency (a Unix socket / named pipe crate would add packages
//! and invalidate the signed SBOM). The listener binds `127.0.0.1:0` only; the
//! port and token live in `.ukma/daemon.json`, which is written `0600` on Unix.
//!
//! Lifecycle: `ukma daemon start` spawns a detached `ukma daemon serve` child,
//! `status` pings it, `stop` asks it to shut down, and an idle timeout
//! (default 30 min, `UKMA_DAEMON_IDLE_SECS`) ends it on its own.
//!
//! Honest boundaries:
//! - it is **not** a proxy for the MCP transport: a session keeps its own stdio
//!   channel and only delegates `tools/call` (on any daemon failure it falls back
//!   to its own gateway). Sharing the whole protocol would let one client's
//!   protocol state influence another's, which is what per-project isolation
//!   exists to prevent;
//! - the daemon is per project (`ukma daemon start [dir]`), not per machine;
//! - a second daemon for the same project is refused (the lock is the handle
//!   file plus the listener bind).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ukma_mcp::index::IndexOptions;

use super::project::PROJECT_DIR;
use super::{now_rfc3339, SetupError};

/// The handle file name inside the project directory.
pub const DAEMON_FILE: &str = "daemon.json";
/// Where a spawned daemon's stderr goes (inside the project directory).
pub const DAEMON_LOG: &str = "daemon.log";
/// The default idle timeout.
pub const DEFAULT_IDLE_SECS: u64 = 1_800;

/// The on-disk handle of a running daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandle {
    /// Process id.
    pub pid: u32,
    /// The loopback port.
    pub port: u16,
    /// The bearer token every request must carry.
    pub token: String,
    /// When the daemon started (RFC 3339).
    pub started_at: String,
    /// The binary version that started it.
    pub version: String,
    /// The project root it serves.
    pub root: String,
}

/// The handle path of a project.
pub fn handle_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR).join(DAEMON_FILE)
}

/// A daemon is "running" when its handle file is readable, parseable and the
/// port still answers a ping with the right token.
pub fn running(root: &Path) -> Option<DaemonHandle> {
    let path = handle_path(root);
    let text = std::fs::read_to_string(&path).ok()?;
    let handle: DaemonHandle = serde_json::from_str(&text).ok()?;
    match request(&handle, &json!({ "op": "ping" })) {
        Ok(_) => Some(handle),
        Err(_) => {
            // A stale handle (killed -9, reboot) must not block a restart.
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

fn io_error(action: &str, path: &Path, error: &std::io::Error) -> SetupError {
    SetupError::Io {
        action: action.to_string(),
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn daemon_error(detail: String) -> SetupError {
    SetupError::Unsupported(format!("daemon: {detail}"))
}

// ------------------------------------------------------------------ client ---

/// One request/response round trip.
pub fn request(handle: &DaemonHandle, payload: &Value) -> Result<Value, String> {
    let address = format!("127.0.0.1:{}", handle.port);
    let mut stream = TcpStream::connect(&address).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    let mut message = payload.clone();
    if let Some(object) = message.as_object_mut() {
        object.insert("token".into(), json!(handle.token));
    }
    let line = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&response).map_err(|error| error.to_string())?;
    if value["ok"] == true {
        Ok(value)
    } else {
        Err(value["error"]
            .as_str()
            .unwrap_or("the daemon rejected the request")
            .to_string())
    }
}

/// A [`ukma_mcp::DaemonCall`] over the loopback protocol: a session delegates
/// its `tools/call` here and keeps its own gateway as the fallback.
pub struct DaemonClient {
    handle: DaemonHandle,
    requests: AtomicU64,
    failures: AtomicU64,
}

impl DaemonClient {
    /// Bind to a running daemon.
    pub fn new(handle: DaemonHandle) -> Self {
        Self {
            handle,
            requests: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }

    /// How many calls were delegated.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// How many delegations failed (and fell back to the local gateway).
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}

impl ukma_mcp::DaemonCall for DaemonClient {
    fn call(
        &self,
        envelope: &ukma_contracts::RequestEnvelope<serde_json::Value>,
    ) -> Result<ukma_contracts::ResponseEnvelope<serde_json::Value>, String> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let payload = json!({ "op": "call", "envelope": envelope });
        match request(&self.handle, &payload) {
            Ok(response) => {
                let envelope: ukma_contracts::ResponseEnvelope<serde_json::Value> =
                    serde_json::from_value(response["envelope"].clone())
                        .map_err(|error| error.to_string())?;
                Ok(envelope)
            }
            Err(error) => {
                self.failures.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }
}

// ------------------------------------------------------------ start / stop ---

/// Start a daemon for `root`, or report the one already running.
pub fn start(root: &Path, options: &IndexOptions) -> Result<DaemonHandle, SetupError> {
    let executable = std::env::current_exe()
        .map_err(|error| io_error("locate the running binary", root, &error))?;
    start_with(&executable, root, options)
}

/// Start a daemon for `root` with an explicit binary.
pub fn start_with(
    executable: &Path,
    root: &Path,
    options: &IndexOptions,
) -> Result<DaemonHandle, SetupError> {
    if let Some(handle) = running(root) {
        return Ok(handle);
    }
    let mut command = std::process::Command::new(executable);
    command.args(["daemon", "serve"]).arg(root);
    if let Some(max_files) = options.max_files {
        command.args(["--max-files", &max_files.to_string()]);
    }
    for exclude in &options.excludes {
        command.args(["--exclude", exclude]);
    }
    let log_path = root.join(PROJECT_DIR).join(DAEMON_LOG);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| io_error("open the daemon log", &log_path, &error))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log));
    let child =
        spawn_detached(&mut command).map_err(|error| io_error("start the daemon", root, &error))?;

    // Wait for the handle file to answer, so `start` never returns a lie.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(handle) = running(root) {
            return Ok(handle);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(daemon_error(format!(
        "the daemon (pid {}) did not answer within 10s",
        child.id()
    )))
}

/// Ask the daemon of `root` to shut down.
pub fn stop(root: &Path) -> Result<bool, SetupError> {
    let Some(handle) = running(root) else {
        return Ok(false);
    };
    let _ = request(&handle, &json!({ "op": "shutdown" }));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !handle_path(root).exists() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The socket is closed either way; drop the handle so a restart is possible.
    let _ = std::fs::remove_file(handle_path(root));
    Ok(true)
}

/// The daemon's own status answer, when one is running.
pub fn status(root: &Path) -> Option<(DaemonHandle, Value)> {
    let handle = running(root)?;
    let response = request(&handle, &json!({ "op": "status" })).ok()?;
    Some((handle, response))
}

// ------------------------------------------------------------------ server ---

/// Serve this project until asked to stop (or until it goes idle).
///
/// Returns the process exit code.
pub fn serve(root: &Path, options: IndexOptions, idle: Duration) -> Result<i32, SetupError> {
    let root = super::project::resolve_root(root)?;
    if !root.is_dir() {
        return Err(SetupError::Missing(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let handle_file = handle_path(&root);
    if let Some(parent) = handle_file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
    }

    // A listener bound before the handle is written means a second daemon
    // cannot race us: whichever writes last would still find a live port, so
    // refuse instead of silently doubling the watcher.
    if running(&root).is_some() {
        return Err(daemon_error(format!(
            "a daemon is already serving {}",
            root.display()
        )));
    }

    // The change source starts *before* the snapshot is read: an edit that
    // lands while the index is being built must still raise an event.
    let watcher = super::watch::start(&root);
    let state = Arc::new(Mutex::new(Warm::load(&root, &options)?));
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| io_error("bind the daemon listener", &root, &error))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| io_error("configure the daemon listener", &root, &error))?;
    let port = listener
        .local_addr()
        .map_err(|error| io_error("read the daemon address", &root, &error))?
        .port();
    let handle = DaemonHandle {
        pid: std::process::id(),
        port,
        token: new_token(),
        started_at: now_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        root: root.display().to_string(),
    };
    write_handle(&handle_file, &handle)?;

    let started = Instant::now();
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let requests = Arc::new(AtomicU64::new(0));
    let in_flight = Arc::new(AtomicU64::new(0));
    let shutdown = Arc::new(AtomicBool::new(false));
    let safety_interval = watcher.safety_interval();
    let native = watcher.is_native();
    let mut last_poll = Instant::now();
    let mut dirty_since: Option<Instant> = None;

    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                requests.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut guard) = last_activity.lock() {
                    *guard = Instant::now();
                }
                // The accepted socket inherits the listener's non-blocking mode
                // on Windows, which turns a request that has not arrived yet
                // into `WouldBlock`; and an unbounded blocking read would let
                // one silent client stall the daemon. Blocking plus a timeout is
                // the pair that makes both impossible.
                if let Err(error) = configure_connection(&stream) {
                    eprintln!("ukma daemon: dropped a connection ({error})");
                    continue;
                }
                // One thread per connection: a slow or silent client must never
                // delay the other sessions (the warm state is shared behind its
                // own mutex, so the heavy part is still serialized).
                let worker = Worker {
                    handle: handle.clone(),
                    root: root.clone(),
                    options: options.clone(),
                    state: Arc::clone(&state),
                    requests: Arc::clone(&requests),
                    in_flight: Arc::clone(&in_flight),
                    shutdown: Arc::clone(&shutdown),
                    native_watcher: native,
                };
                in_flight.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || {
                    let result = handle_connection(stream, &worker);
                    worker.in_flight.fetch_sub(1, Ordering::Relaxed);
                    if let Err(error) = result {
                        eprintln!("ukma daemon: dropped a connection ({error})");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&handle_file);
                return Err(io_error("accept a daemon connection", &root, &error));
            }
        }

        // The watcher: an OS event (debounced) or the safety poll decides when
        // the warm index is refreshed.
        let event = watcher.take_change();
        if event {
            dirty_since = Some(Instant::now());
        }
        let debounced = dirty_since
            .map(|since| since.elapsed() >= super::watch::DEBOUNCE)
            .unwrap_or(false);
        if debounced || last_poll.elapsed() >= safety_interval {
            dirty_since = None;
            last_poll = Instant::now();
            if let Ok(mut warm) = state.lock() {
                let _ = warm.refresh(&root, &options);
            }
        }

        let idle_for = last_activity
            .lock()
            .map(|guard| guard.elapsed())
            .unwrap_or(Duration::ZERO);
        // Never exit under a request that is still being served.
        if idle_for > idle && in_flight.load(Ordering::Relaxed) == 0 {
            shutdown.store(true, Ordering::Relaxed);
        }
    }

    let _ = std::fs::remove_file(&handle_file);
    let uptime = started.elapsed();
    let mut watcher = watcher;
    watcher.stop();
    eprintln!(
        "ukma daemon: stopped after {:.1}s and {} request(s) (watcher: {})",
        uptime.as_secs_f64(),
        requests.load(Ordering::Relaxed),
        if native { "native" } else { "poll" }
    );
    Ok(0)
}

/// Everything one connection handler needs.
struct Worker {
    handle: DaemonHandle,
    root: PathBuf,
    options: IndexOptions,
    state: Arc<Mutex<Warm>>,
    requests: Arc<AtomicU64>,
    in_flight: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    /// Whether a filesystem event source (rather than only the poll) drives
    /// refresh.
    native_watcher: bool,
}

/// The warm state: one gateway over the project snapshot, rebuilt only when the
/// tree changes.
struct Warm {
    revision: String,
    files: usize,
    gateway: ukma_mcp::Gateway,
    signature: u64,
    synced_at: String,
    syncs: u64,
}

impl Warm {
    fn load(root: &Path, options: &IndexOptions) -> Result<Self, SetupError> {
        let snapshot = super::cache::load(root, options)?;
        let files = super::cache::source_files(&snapshot.files);
        let parse_cache: Arc<dyn storage_ports::snapshot::ParseCache> = snapshot.store.clone();
        let cached = super::cache::gateway(
            &snapshot.revision,
            &files,
            ukma_mcp::SearchPolicy::default(),
            None,
            ukma_mcp::BackendMode::WithModel,
            None,
            Some(parse_cache),
        )
        .map_err(daemon_error)?;
        Ok(Self {
            revision: snapshot.revision,
            files: snapshot.files.len(),
            gateway: cached.gateway,
            signature: tree_signature(root),
            synced_at: now_rfc3339(),
            syncs: 1,
        })
    }

    /// Refresh when the tree moved. Returns whether anything changed.
    fn refresh(&mut self, root: &Path, options: &IndexOptions) -> bool {
        let signature = tree_signature(root);
        if signature == self.signature {
            return false;
        }
        self.signature = signature;
        match Self::load(root, options) {
            Ok(fresh) => {
                self.revision = fresh.revision;
                self.files = fresh.files;
                self.gateway = fresh.gateway;
                self.synced_at = now_rfc3339();
                self.syncs += 1;
                true
            }
            Err(error) => {
                // Keep serving the previous snapshot and retry on the next
                // poll rather than dropping the daemon.
                eprintln!("ukma daemon: refresh failed ({error}); serving the previous snapshot");
                false
            }
        }
    }
}

/// A cheap change detector: `(path, size, mtime)` over the tree, folded into one
/// number. False positives (a rebuild) only cost a refresh.
fn tree_signature(root: &Path) -> u64 {
    match ukma_mcp::index::tree_signature(&[root]) {
        Ok(signature) => {
            let mut hash = 1_469_598_103_934_665_603u64;
            for (path, mtime) in signature {
                for byte in path.as_bytes() {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(1_099_511_628_211);
                }
                hash ^= mtime;
                hash = hash.wrapping_mul(1_099_511_628_211);
            }
            hash
        }
        Err(_) => 0,
    }
}

/// Make an accepted connection blocking with a bounded read/write timeout.
fn configure_connection(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))
}

fn handle_connection(mut stream: TcpStream, worker: &Worker) -> Result<bool, SetupError> {
    let Worker {
        handle,
        root,
        options,
        state,
        requests,
        shutdown,
        ..
    } = worker;
    let mut line = String::new();
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| io_error("clone the connection", root, &error))?,
    );
    if reader
        .read_line(&mut line)
        .map_err(|error| io_error("read a daemon request", root, &error))?
        == 0
    {
        return Ok(false);
    }
    let request: Value = match serde_json::from_str(&line) {
        Ok(value) => value,
        Err(error) => {
            let _ = writeln!(
                stream,
                "{}",
                json!({ "ok": false, "error": format!("invalid request: {error}") })
            );
            return Ok(false);
        }
    };
    // Loopback plus a per-project token: a local process without the token is
    // refused (defence in depth; the port is ephemeral and bound to 127.0.0.1).
    if request["token"].as_str() != Some(handle.token.as_str()) {
        let _ = writeln!(
            stream,
            "{}",
            json!({ "ok": false, "error": "unauthorized" })
        );
        return Ok(false);
    }

    let op = request["op"].as_str().unwrap_or("");
    let response = match op {
        "ping" => json!({
            "ok": true,
            "pid": handle.pid,
            "revision": state.lock().map(|warm| warm.revision.clone()).unwrap_or_default()
        }),
        "status" => match state.lock() {
            Ok(warm) => json!({
                "ok": true,
                "pid": handle.pid,
                "port": handle.port,
                "root": handle.root,
                "version": handle.version,
                "started_at": handle.started_at,
                "revision": warm.revision,
                "files": warm.files,
                "syncs": warm.syncs,
                "synced_at": warm.synced_at,
                "requests": requests.load(Ordering::Relaxed),
                "watcher": if worker.native_watcher { "native" } else { "poll" },
            }),
            Err(_) => json!({ "ok": false, "error": "daemon state is poisoned" }),
        },
        "sync" => match state.lock() {
            Ok(mut warm) => {
                warm.signature = 0;
                let changed = warm.refresh(root, options);
                json!({
                    "ok": true,
                    "changed": changed,
                    "revision": warm.revision,
                    "files": warm.files,
                    "syncs": warm.syncs,
                })
            }
            Err(_) => json!({ "ok": false, "error": "daemon state is poisoned" }),
        },
        "call" => {
            let envelope: ukma_contracts::RequestEnvelope<serde_json::Value> =
                match serde_json::from_value(request["envelope"].clone()) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        let _ = writeln!(
                            stream,
                            "{}",
                            json!({ "ok": false, "error": format!("invalid envelope: {error}") })
                        );
                        return Ok(false);
                    }
                };
            match state.lock() {
                Ok(warm) => {
                    let response = warm.gateway.dispatch(envelope);
                    json!({ "ok": true, "envelope": response })
                }
                Err(_) => json!({ "ok": false, "error": "daemon state is poisoned" }),
            }
        }
        "shutdown" => {
            shutdown.store(true, Ordering::Relaxed);
            json!({ "ok": true, "stopping": true })
        }
        other => json!({ "ok": false, "error": format!("unknown op `{other}`") }),
    };
    let _ = writeln!(stream, "{response}");
    Ok(shutdown.load(Ordering::Relaxed))
}

/// Spawn the daemon so it cannot hold the parent's standard streams.
///
/// On Windows the child inherits every inheritable handle of the parent, which
/// includes a redirected `stdout`: `ukma daemon start | Out-Null` (or any
/// pipeline, `$(...)` capture or CI wrapper) would then never see EOF and hang
/// for as long as the daemon lives. The standard handles are made
/// non-inheritable for the duration of the spawn and restored afterwards.
#[cfg(windows)]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::ffi::c_void;

    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetHandleInformation(handle: *mut c_void, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }

    const HANDLE_FLAG_INHERIT: u32 = 0x0001;
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;

    /// Restores the inherit flag when the spawn attempt is over.
    struct Restore(Vec<(*mut c_void, u32)>);

    impl Drop for Restore {
        fn drop(&mut self) {
            for (handle, flags) in &self.0 {
                // SAFETY: the handle came from `GetStdHandle` and was still
                // valid when the flag was cleared; restoring a documented flag
                // on it is safe.
                unsafe {
                    SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, *flags);
                }
            }
        }
    }

    let mut cleared: Vec<(*mut c_void, u32)> = Vec::new();
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: the standard-handle getters are safe to call with the
        // documented constants; the results are validated before use.
        unsafe {
            let handle = GetStdHandle(which);
            if handle.is_null() || handle as isize == -1 {
                continue;
            }
            let mut flags = 0u32;
            if GetHandleInformation(handle, &mut flags) == 0 {
                continue;
            }
            if flags & HANDLE_FLAG_INHERIT != 0
                && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) != 0
            {
                cleared.push((handle, flags));
            }
        }
    }
    let restore = Restore(cleared);
    let child = command.spawn();
    drop(restore);
    child
}

/// Non-Windows spawn: `exec` only leaks the descriptors it is given.
#[cfg(not(windows))]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    command.spawn()
}

fn write_handle(path: &Path, handle: &DaemonHandle) -> Result<(), SetupError> {
    let text = serde_json::to_string_pretty(handle).map_err(|error| SetupError::Config {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    std::fs::write(path, format!("{text}\n")).map_err(|error| io_error("write", path, &error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn new_token() -> String {
    // Not a crypto RNG: the token only has to be unguessable by another local
    // process for the lifetime of an ephemeral loopback port, and it is
    // generated from the pid, the clock and the address of a fresh allocation.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let marker = Box::new(0u8);
    let address = std::ptr::from_ref(&*marker) as usize;
    let mut state = now ^ ((std::process::id() as u128) << 64) ^ (address as u128);
    let mut token = String::new();
    for _ in 0..8 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        token.push_str(&format!("{:08x}", (state >> 64) as u32));
    }
    token
}

/// The idle timeout from `UKMA_DAEMON_IDLE_SECS` (clamped to `[10s, 24h]`).
pub fn idle_timeout() -> Duration {
    idle_timeout_from(std::env::var("UKMA_DAEMON_IDLE_SECS").ok().as_deref())
}

/// The timeout for an explicit setting (pure, so tests need no environment).
pub fn idle_timeout_from(value: Option<&str>) -> Duration {
    let seconds = value
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_SECS)
        .clamp(10, 86_400);
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_path_lives_in_the_project_directory() {
        let path = handle_path(Path::new("/work/project"));
        assert!(path.ends_with("daemon.json"));
        assert!(path.to_string_lossy().contains(PROJECT_DIR));
    }

    #[test]
    fn a_missing_handle_means_no_daemon() {
        let root = std::env::temp_dir().join("ukma-daemon-missing");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        assert!(running(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_handle_is_cleaned_up() {
        let root = std::env::temp_dir().join(format!("ukma-daemon-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(PROJECT_DIR)).expect("mkdir");
        let handle = DaemonHandle {
            pid: 999_999,
            port: 1, // nothing listens there
            token: "t".into(),
            started_at: "2026-10-26T00:00:00Z".into(),
            version: "0.1.0".into(),
            root: root.display().to_string(),
        };
        std::fs::write(
            handle_path(&root),
            serde_json::to_string(&handle).expect("json"),
        )
        .expect("write");
        assert!(running(&root).is_none(), "a dead port is not a daemon");
        assert!(!handle_path(&root).exists(), "the stale handle is removed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tokens_are_long_and_distinct() {
        let first = new_token();
        let second = new_token();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
    }

    #[test]
    fn the_idle_timeout_is_clamped() {
        assert_eq!(idle_timeout_from(Some("1")), Duration::from_secs(10));
        assert_eq!(
            idle_timeout_from(Some("999999")),
            Duration::from_secs(86_400)
        );
        assert_eq!(
            idle_timeout_from(Some("nonsense")),
            Duration::from_secs(DEFAULT_IDLE_SECS)
        );
        assert_eq!(
            idle_timeout_from(None),
            Duration::from_secs(DEFAULT_IDLE_SECS)
        );
        assert_eq!(idle_timeout_from(Some("300")), Duration::from_secs(300));
    }
}
