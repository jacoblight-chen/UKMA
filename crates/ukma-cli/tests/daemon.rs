//! The shared per-project daemon driven end to end: the loopback protocol, the
//! warm-index watcher, idle expiry, and the spawned child behind
//! `ukma daemon start`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use ukma_cli::setup::daemon::{
    handle_path, request, running, serve, start_with, status, stop, DaemonClient, DaemonHandle,
};
use ukma_cli::setup::project;
use ukma_contracts::{Consistency, RequestContext, RequestEnvelope, ScopeRef};
use ukma_mcp::index::IndexOptions;
use ukma_mcp::DaemonCall;

struct Tree {
    root: PathBuf,
    revision: String,
}

impl Tree {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ukma-daemon-it-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create tree");
        std::fs::write(root.join("src").join("lib.rs"), "pub fn alpha() {}\n").expect("write");
        let report = project::init(&root, &[], &IndexOptions::default()).expect("init");
        Self {
            root,
            revision: report.revision,
        }
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, content).expect("write");
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // Windows releases the daemon's handles (log, `index.db` and its WAL)
        // slightly after the process exits, so a single attempt can fail.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.root).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Serves `root` on a background thread (the daemon is a loop, so the test
/// needs a thread rather than a function call).
fn spawn(root: &Path, idle: Duration) -> std::thread::JoinHandle<i32> {
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        serve(&root, IndexOptions::default(), idle).expect("the daemon serves")
    })
}

/// Waits until the handle answers a ping (or fails the test).
fn wait_until_running(root: &Path) -> DaemonHandle {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(handle) = running(root) {
            return handle;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the daemon did not start within 15s");
}

fn context(operation: &str, scopes: &[&str]) -> RequestContext {
    RequestContext {
        request_id: format!("daemon-{operation}"),
        trace_id: "trace-daemon".into(),
        scope: ScopeRef {
            tenant_id: "tenant-dev".into(),
            project_id: "project-dev".into(),
            repo_id: None,
            workspace_id: None,
            agent_id: None,
            session_id: None,
        },
        principal_id: "principal-dev".into(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        deadline_at: None,
        consistency: Consistency::Eventual,
        required_generation: None,
        snapshot_id: None,
        idempotency_key: Some(format!("daemon-{operation}")),
        expected_etag: Some("\"daemon-etag\"".into()),
        delegation_token: None,
    }
}

fn capabilities_envelope() -> RequestEnvelope<Value> {
    let tool = ukma_mcp::manifest::tool_by_name("ukma_system_capabilities_v1").expect("tool");
    RequestEnvelope {
        api_version: "1".into(),
        operation: tool.operation_id.to_string(),
        context: context(tool.operation_id, tool.required_scopes),
        input: Value::Null,
    }
}

#[test]
fn a_daemon_answers_status_and_serves_the_same_answers_as_a_local_gateway() {
    let tree = Tree::new("answers");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);

    let answer = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
    assert_eq!(answer["revision"].as_str(), Some(tree.revision.as_str()));
    assert_eq!(answer["files"].as_u64(), Some(1));
    assert_eq!(answer["port"].as_u64(), Some(u64::from(handle.port)));

    let envelope = capabilities_envelope();
    let remote = DaemonClient::new(handle.clone())
        .call(&envelope)
        .expect("call");
    let local = ukma_mcp::default_gateway().dispatch(envelope.clone());
    assert_eq!(
        serde_json::to_string(&remote.data).expect("json"),
        serde_json::to_string(&local.data).expect("json"),
        "delegation must not change the answer"
    );
    assert_eq!(remote.status, local.status);

    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn the_warm_index_follows_the_tree_without_a_new_session() {
    let tree = Tree::new("watcher");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);
    let first = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
    assert_eq!(first["syncs"].as_u64(), Some(1));

    tree.write("src/lib.rs", "pub fn beta() {}\n");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut answer = first;
    while Instant::now() < deadline {
        answer = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
        if answer["revision"].as_str() != Some(tree.revision.as_str()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_ne!(
        answer["revision"].as_str(),
        Some(tree.revision.as_str()),
        "the watcher re-indexes after the tree changed"
    );
    assert!(
        answer["syncs"].as_u64().unwrap_or_default() >= 2,
        "a refresh is counted"
    );

    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn stop_removes_the_handle_and_is_idempotent() {
    let tree = Tree::new("stop");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    wait_until_running(&tree.root);
    assert!(handle_path(&tree.root).exists());

    assert!(stop(&tree.root).expect("stop"));
    assert!(!handle_path(&tree.root).exists(), "the handle is removed");
    assert_eq!(thread.join().expect("thread"), 0);

    assert!(
        !stop(&tree.root).expect("stop"),
        "stopping a stopped daemon reports nothing to do"
    );
    assert!(running(&tree.root).is_none());
    assert!(status(&tree.root).is_none());
}

#[test]
fn an_idle_daemon_exits_and_leaves_no_handle() {
    let tree = Tree::new("idle");
    let thread = spawn(&tree.root, Duration::from_millis(200));
    wait_until_running(&tree.root);
    assert_eq!(thread.join().expect("thread"), 0);
    assert!(!handle_path(&tree.root).exists(), "the handle is removed");
    assert!(running(&tree.root).is_none());
}

#[test]
fn a_request_with_the_wrong_token_is_refused() {
    let tree = Tree::new("token");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);

    let mut forged = handle.clone();
    forged.token = "0".repeat(64);
    let error = request(&forged, &serde_json::json!({ "op": "status" })).expect_err("refused");
    assert_eq!(error, "unauthorized");

    // The real token still works, so the refusal did not stop the daemon.
    assert!(request(&handle, &serde_json::json!({ "op": "ping" })).is_ok());
    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn the_wrong_token_is_refused_but_a_call_still_falls_back_locally() {
    let tree = Tree::new("fallback");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);

    let mut forged = handle.clone();
    forged.token = "1".repeat(64);
    let client = DaemonClient::new(forged);
    assert!(client.call(&capabilities_envelope()).is_err());
    assert_eq!(client.failures(), 1);

    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn a_spawned_daemon_is_reachable_and_stoppable() {
    let tree = Tree::new("spawn");
    let binary = Path::new(env!("CARGO_BIN_EXE_ukma"));

    let handle = start_with(binary, &tree.root, &IndexOptions::default()).expect("start");
    // Even a failing assertion must not leave an orphan daemon behind.
    let guard = SpawnGuard(tree.root.clone());
    assert_ne!(handle.pid, std::process::id(), "a real child process");
    let answer = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
    assert_eq!(answer["revision"].as_str(), Some(tree.revision.as_str()));

    // A second start reuses the running daemon instead of doubling it.
    let again = start_with(binary, &tree.root, &IndexOptions::default()).expect("start");
    assert_eq!(again.pid, handle.pid);

    assert!(stop(&tree.root).expect("stop"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while running(&tree.root).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(running(&tree.root).is_none(), "the child exits on stop");
    assert!(
        tree.root.join(".ukma").join("daemon.log").exists(),
        "the child's stderr is kept for diagnosis"
    );
    // Give the child a moment to release `.ukma/index.db` so the tree can be
    // removed on Windows.
    std::thread::sleep(Duration::from_millis(300));
    drop(guard);
}

#[test]
fn a_client_that_sends_late_is_not_dropped() {
    let tree = Tree::new("slow");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);

    // Connect, wait well past the moment the daemon starts reading, then send.
    // A non-blocking accepted socket would turn that pause into `WouldBlock`
    // and the daemon would drop the connection before the request arrives.
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", handle.port)).expect("connect");
    std::thread::sleep(Duration::from_millis(500));
    {
        use std::io::Write;
        let line = serde_json::json!({ "op": "ping", "token": handle.token }).to_string();
        stream
            .write_all(format!("{line}\n").as_bytes())
            .expect("send");
        stream.flush().expect("flush");
    }
    let mut reader = std::io::BufReader::new(stream);
    let mut reply = String::new();
    {
        use std::io::BufRead;
        reader.read_line(&mut reply).expect("read the reply");
    }
    let value: Value = serde_json::from_str(&reply).expect("a JSON reply");
    assert_eq!(value["ok"], true, "{reply}");

    // A dropped connection must never take the daemon with it.
    assert!(request(&handle, &serde_json::json!({ "op": "ping" })).is_ok());
    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn a_silent_connection_does_not_block_other_clients() {
    let tree = Tree::new("concurrent");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);

    // Hold a connection open without sending anything: with one connection
    // handled at a time this would stall the daemon until the read timeout.
    let silent = std::net::TcpStream::connect(("127.0.0.1", handle.port)).expect("connect");
    std::thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    let answer = request(&handle, &serde_json::json!({ "op": "ping" })).expect("ping");
    let elapsed = started.elapsed();
    assert_eq!(answer["ok"], true);
    assert!(
        elapsed < Duration::from_secs(5),
        "a silent client delayed another session by {elapsed:?}"
    );
    drop(silent);

    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn several_clients_are_served_at_once() {
    let tree = Tree::new("parallel");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);

    let mut workers = Vec::new();
    for _ in 0..4 {
        let handle = handle.clone();
        workers.push(std::thread::spawn(move || {
            for _ in 0..5 {
                let answer =
                    request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
                assert_eq!(answer["files"].as_u64(), Some(1));
            }
        }));
    }
    for worker in workers {
        worker.join().expect("worker");
    }
    let answer = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
    assert!(
        answer["requests"].as_u64().unwrap_or_default() >= 20,
        "every concurrent request was counted: {answer}"
    );

    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

#[test]
fn the_native_watcher_refreshes_faster_than_the_safety_poll() {
    if !cfg!(windows) {
        // Other platforms have no in-process event source (a watcher crate
        // would add a package and invalidate the signed SBOM), so the poll is
        // the mechanism and this bound would be meaningless.
        return;
    }
    // Push the safety poll far out of reach: only the filesystem event can
    // refresh inside the deadline. Other tests keep working because the native
    // watcher is what drives them too.
    std::env::set_var("UKMA_DAEMON_POLL_SECS", "3600");
    let tree = Tree::new("native");
    let thread = spawn(&tree.root, Duration::from_secs(60));
    let handle = wait_until_running(&tree.root);
    let first = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
    assert_eq!(first["watcher"].as_str(), Some("native"));
    assert_eq!(first["syncs"].as_u64(), Some(1));

    tree.write("src/lib.rs", "pub fn gamma() {}\n");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut answer = first;
    while Instant::now() < deadline {
        answer = request(&handle, &serde_json::json!({ "op": "status" })).expect("status");
        if answer["revision"].as_str() != Some(tree.revision.as_str()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::env::remove_var("UKMA_DAEMON_POLL_SECS");
    assert_ne!(
        answer["revision"].as_str(),
        Some(tree.revision.as_str()),
        "the native watcher must refresh with the poll effectively disabled"
    );

    assert!(stop(&tree.root).expect("stop"));
    assert_eq!(thread.join().expect("thread"), 0);
}

/// Stops the daemon of `root` when dropped.
struct SpawnGuard(PathBuf);

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        let _ = stop(&self.0);
    }
}

#[test]
fn a_piped_daemon_start_returns_instead_of_waiting_for_the_daemon() {
    let tree = Tree::new("pipe");
    let binary = Path::new(env!("CARGO_BIN_EXE_ukma"));
    let mut child = std::process::Command::new(binary)
        .args(["daemon", "start"])
        .arg(&tree.root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the CLI");
    let guard = SpawnGuard(tree.root.clone());

    // Reading to EOF is what a shell pipeline does: if the daemon inherited the
    // pipe, this never returns.
    let mut stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let text = receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("`ukma daemon start` must close its stdout even though the daemon keeps running");
    assert!(child.wait().expect("wait").success());
    assert!(text.contains("running"), "{text}");
    let _ = reader.join();
    assert!(
        running(&tree.root).is_some(),
        "the daemon outlives the starter"
    );
    drop(guard);
}
