//! Change notification for the daemon: the OS watcher where one is available,
//! a stat poll otherwise.
//!
//! The daemon keeps a warm index, so it has to know when the tree moved. A stat
//! walk is proportional to the number of files: fine as a safety net, wasteful as
//! the only mechanism. Windows therefore gets a real event source
//! (`ReadDirectoryChangesW`, via the Win32 API because adding a watcher crate
//! would add a package and invalidate the signed SBOM), and everything else
//! falls back to a poll the operator can tune.

use std::path::Path;
use std::time::Duration;

#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use super::project::PROJECT_DIR;

/// The default safety poll interval.
pub const DEFAULT_POLL_SECS: u64 = 2;
/// The default safety poll interval when a native watcher is in charge.
const NATIVE_SAFETY_POLL_SECS: u64 = 30;
/// The file written to wake a blocked directory read (inside `.ukma/`).
#[cfg(windows)]
const NUDGE_FILE: &str = "watch.nudge";

/// The safety poll interval from `UKMA_DAEMON_POLL_SECS` (clamped to `[1, 3600]`).
pub fn poll_interval() -> Duration {
    poll_interval_from(std::env::var("UKMA_DAEMON_POLL_SECS").ok().as_deref())
}

/// The interval for an explicit setting (pure, so tests need no environment).
pub fn poll_interval_from(value: Option<&str>) -> Duration {
    let seconds = value
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_SECS)
        .clamp(1, 3_600);
    Duration::from_secs(seconds)
}

/// How long the tree has to stay quiet before the index is refreshed, so a
/// compiler writing a thousand files costs one refresh instead of a thousand.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// A running change source.
pub enum Watcher {
    /// A native, event-driven source (`ReadDirectoryChangesW`).
    #[cfg(windows)]
    Native {
        /// Set when the watched tree changed.
        changed: Arc<AtomicBool>,
        /// Set when the source should stop.
        stop: Arc<AtomicBool>,
        /// Set once the first read is armed (so a caller can avoid the race
        /// between starting the watcher and changing the tree).
        ready: Arc<AtomicBool>,
        /// The blocking read thread.
        thread: Option<std::thread::JoinHandle<()>>,
        /// The file used to wake the blocked read.
        nudge: PathBuf,
    },
    /// No event source on this platform: the daemon polls.
    Poll,
}

impl Watcher {
    /// Whether the tree changed since the last call (clears the flag).
    pub fn take_change(&self) -> bool {
        match self {
            #[cfg(windows)]
            Watcher::Native { changed, .. } => changed.swap(false, Ordering::SeqCst),
            Watcher::Poll => false,
        }
    }

    /// Wait until the source is armed (always true for polling).
    pub fn wait_until_ready(&self, timeout: Duration) -> bool {
        match self {
            #[cfg(windows)]
            Watcher::Native { ready, .. } => {
                let deadline = std::time::Instant::now() + timeout;
                while std::time::Instant::now() < deadline {
                    if ready.load(Ordering::SeqCst) {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                ready.load(Ordering::SeqCst)
            }
            Watcher::Poll => {
                let _ = timeout;
                true
            }
        }
    }

    /// Whether a native source is in charge (so the poll is only a safety net).
    pub fn is_native(&self) -> bool {
        match self {
            #[cfg(windows)]
            Watcher::Native { .. } => true,
            Watcher::Poll => false,
        }
    }

    /// The safety poll interval for this source.
    pub fn safety_interval(&self) -> Duration {
        if self.is_native() {
            Duration::from_secs(NATIVE_SAFETY_POLL_SECS)
        } else {
            poll_interval()
        }
    }

    /// Stop the source and wait for its thread.
    pub fn stop(&mut self) {
        match self {
            #[cfg(windows)]
            Watcher::Native {
                stop,
                thread,
                nudge,
                ..
            } => {
                stop.store(true, Ordering::SeqCst);
                // The pending read only returns when something happens, so make
                // something happen (and clean it up instead of leaving debris).
                // The parent directory has to exist: a watcher can be started for
                // a tree without a `.ukma/` directory, and without the nudge the
                // blocking read would never return.
                let nudged = nudge
                    .parent()
                    .map(|parent| std::fs::create_dir_all(parent).is_ok())
                    .unwrap_or(false)
                    && std::fs::write(&*nudge, b"stop\n").is_ok();
                let _ = std::fs::remove_file(&*nudge);
                if nudged {
                    if let Some(thread) = thread.take() {
                        let _ = thread.join();
                    }
                }
                // Without the nudge the read stays blocked; the thread then ends
                // with the process instead of being joinable here.
            }
            Watcher::Poll => {}
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start watching `root`, falling back to polling when the platform (or the
/// filesystem) has no usable event source.
pub fn start(root: &Path) -> Watcher {
    #[cfg(windows)]
    {
        if let Some(watcher) = native::start(root) {
            return watcher;
        }
    }
    let _ = root;
    Watcher::Poll
}

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    use super::*;

    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_SHARE_DELETE: u32 = 0x0004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x0001;
    const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x0002;
    const FILE_NOTIFY_CHANGE_SIZE: u32 = 0x0008;
    const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x0010;
    const INVALID_HANDLE_VALUE: isize = -1;
    const BUFFER_BYTES: usize = 8 * 1024;

    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn ReadDirectoryChangesW(
            directory: *mut c_void,
            buffer: *mut c_void,
            length: u32,
            subtree: i32,
            filter: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
            routine: *mut c_void,
            parameter: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    pub fn start(root: &Path) -> Option<Watcher> {
        let wide: Vec<u16> = root
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path, the flags are the
        // documented combination for opening a directory for change
        // notification, and the returned handle is checked before use.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
            return None;
        }

        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let thread_changed = Arc::clone(&changed);
        let thread_stop = Arc::clone(&stop);
        let thread_ready = Arc::clone(&ready);
        // The handle crosses the thread boundary as an integer: a raw pointer is
        // not `Send`, and the thread that receives it is the only user.
        let handle_bits = handle as usize;

        let thread = std::thread::Builder::new()
            .name("ukma-daemon-watch".into())
            .spawn(move || {
                let handle = handle_bits as *mut c_void;
                let mut buffer = vec![0u8; BUFFER_BYTES];
                let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE;
                loop {
                    if thread_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut returned = 0u32;
                    thread_ready.store(true, Ordering::SeqCst);
                    // SAFETY: the handle is valid for this thread's lifetime,
                    // the buffer is owned and sized as passed, and every out
                    // pointer refers to a live local.
                    let ok = unsafe {
                        ReadDirectoryChangesW(
                            handle,
                            buffer.as_mut_ptr().cast::<c_void>(),
                            buffer.len() as u32,
                            1,
                            filter,
                            &mut returned,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    };
                    if ok == 0 || thread_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    thread_changed.store(true, Ordering::SeqCst);
                }
                // SAFETY: the handle was created above and is closed exactly
                // once, at the end of the thread that owns it.
                unsafe {
                    CloseHandle(handle);
                }
            })
            .ok()?;

        Some(Watcher::Native {
            changed,
            stop,
            ready,
            thread: Some(thread),
            nudge: root.join(PROJECT_DIR).join(NUDGE_FILE),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poll_interval_is_configurable_and_clamped() {
        assert_eq!(
            poll_interval_from(Some("0")),
            Duration::from_secs(1),
            "zero is clamped up"
        );
        assert_eq!(
            poll_interval_from(Some("99999")),
            Duration::from_secs(3_600)
        );
        assert_eq!(
            poll_interval_from(Some("nonsense")),
            Duration::from_secs(DEFAULT_POLL_SECS)
        );
        assert_eq!(
            poll_interval_from(None),
            Duration::from_secs(DEFAULT_POLL_SECS)
        );
        assert_eq!(poll_interval_from(Some("7")), Duration::from_secs(7));
    }

    #[test]
    fn a_missing_directory_falls_back_to_polling() {
        let missing = std::env::temp_dir().join("ukma-watch-missing-does-not-exist");
        let mut watcher = start(&missing);
        assert!(!watcher.take_change());
        watcher.stop();
    }

    #[test]
    fn the_poll_source_never_reports_a_change() {
        let poll = Watcher::Poll;
        assert!(!poll.is_native());
        assert!(!poll.take_change());
        assert_eq!(poll.safety_interval(), poll_interval());
    }
    #[cfg(windows)]
    #[test]
    fn the_native_source_reports_a_file_write_and_stops_promptly() {
        let root = std::env::temp_dir().join(format!("ukma-watch-it-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let mut watcher = start(&root);
        assert!(watcher.is_native(), "Windows has an event source");
        assert!(
            watcher.wait_until_ready(Duration::from_secs(10)),
            "the source must arm itself"
        );
        std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut changed = false;
        while std::time::Instant::now() < deadline {
            if watcher.take_change() {
                changed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(changed, "a write in the tree must raise the change flag");

        let started = std::time::Instant::now();
        watcher.stop();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stopping must not wait for the next filesystem event"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
