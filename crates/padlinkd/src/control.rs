//! What the daemon shares with its control socket, and the socket itself.
//!
//! The CLI sends one JSON request per connection and gets one JSON reply
//! (`padlink_config::runtime`).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use padlink_config::runtime::{
    Reply, Request, State, Status, control_path, lock_path, runtime_dir,
};
use tracing::{debug, warn};

/// State shared between the daemon loop, the session and the control socket.
#[derive(Debug)]
pub struct Shared {
    status: Mutex<Status>,
    /// Whether the user wants a connection. `padlink disconnect` clears it.
    wanted: AtomicBool,
}

impl Shared {
    pub fn new(wanted: bool) -> Arc<Self> {
        Arc::new(Shared {
            status: Mutex::new(Status::default()),
            wanted: AtomicBool::new(wanted),
        })
    }

    pub fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed)
    }

    pub fn set_wanted(&self, wanted: bool) {
        self.wanted.store(wanted, Ordering::Relaxed);
    }

    /// Back to a bare state, keeping only the reason the last attempt ended.
    pub fn reset(&self, state: State, message: Option<String>) {
        *self.status() = Status {
            state,
            message,
            ..Status::default()
        };
    }
}

/// The lock that makes this the only daemon of the session (R1.21). The
/// kernel releases it when the process ends, however it ends.
pub struct Lock(#[allow(dead_code)] File);

pub fn acquire_lock(xdg_runtime_dir: &Path) -> Result<Lock, String> {
    let dir = runtime_dir(xdg_runtime_dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = lock_path(xdg_runtime_dir);
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open the lock file {}: {e}", path.display()))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |_| {
            "another padlinkd is already running in this session. \
             Use `padlink status` to see what it is doing"
                .to_string()
        },
    )?;
    Ok(Lock(file))
}

/// Listen on the control socket. Call only while holding the lock: a socket
/// file left by a dead daemon is replaced.
pub fn serve(xdg_runtime_dir: &Path, shared: Arc<Shared>) -> Result<(), String> {
    let path = control_path(xdg_runtime_dir);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    std::thread::Builder::new()
        .name("padlink-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                // One thread per request: `disconnect` waits for teardown
                // and must not hold up a `status` asked meanwhile.
                let _ = std::thread::Builder::new()
                    .name("padlink-request".into())
                    .spawn(move || {
                        if let Err(e) = answer(stream, &shared) {
                            debug!("control request failed: {e}");
                        }
                    });
            }
        })
        .map_err(|e| format!("cannot start the control thread: {e}"))?;
    Ok(())
}

fn answer(stream: UnixStream, shared: &Shared) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let reply = match serde_json::from_str::<Request>(&line) {
        Ok(request) => {
            handle(request, shared);
            Reply {
                ok: true,
                error: None,
                status: shared.status().clone(),
            }
        }
        Err(e) => {
            warn!("ignoring a control request that is not understood: {e}");
            Reply {
                ok: false,
                error: Some(format!("request not understood: {e}")),
                status: shared.status().clone(),
            }
        }
    };
    let mut out = serde_json::to_string(&reply).expect("replies serialise");
    out.push('\n');
    (&stream).write_all(out.as_bytes())
}

fn handle(request: Request, shared: &Shared) {
    match request {
        Request::Status => {}
        Request::Connect => shared.set_wanted(true),
        Request::Disconnect => {
            shared.set_wanted(false);
            // Answer once the display is gone, so `padlink disconnect`
            // returning means the desktop is back as it was.
            let deadline = Instant::now() + Duration::from_secs(10);
            while shared.status().state != State::Paused && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlink_config::runtime::ask;

    #[test]
    fn only_one_daemon_can_hold_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire_lock(dir.path()).unwrap();
        let err = acquire_lock(dir.path()).err().unwrap();
        assert!(err.contains("already running"), "{err}");
        drop(first);
        // A lock file left by a dead daemon does not block the next one.
        assert!(acquire_lock(dir.path()).is_ok());
    }

    #[test]
    fn the_runtime_directory_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = acquire_lock(dir.path()).unwrap();
        let mode = std::fs::metadata(runtime_dir(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn the_socket_answers_status_connect_and_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = acquire_lock(dir.path()).unwrap();
        let shared = Shared::new(true);
        shared.status().device = Some("iPad".into());
        serve(dir.path(), shared.clone()).unwrap();
        let control = control_path(dir.path());

        let status = ask(&control, Request::Status).unwrap();
        assert_eq!(status.device.as_deref(), Some("iPad"));
        assert_eq!(status.state, State::Idle);

        // Disconnect clears the wish and waits for the daemon loop, played
        // here by this thread, to reach Paused.
        let waiter = {
            let control = control.clone();
            std::thread::spawn(move || ask(&control, Request::Disconnect).unwrap())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.wanted() {
            assert!(Instant::now() < deadline, "disconnect never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
        shared.reset(State::Paused, None);
        assert_eq!(waiter.join().unwrap().state, State::Paused);

        ask(&control, Request::Connect).unwrap();
        assert!(shared.wanted());
    }

    #[test]
    fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = acquire_lock(dir.path()).unwrap();
        std::fs::write(control_path(dir.path()), b"stale").unwrap();
        serve(dir.path(), Shared::new(true)).unwrap();
        assert!(ask(&control_path(dir.path()), Request::Status).is_ok());
    }

    #[test]
    fn garbage_gets_an_error_reply_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = acquire_lock(dir.path()).unwrap();
        serve(dir.path(), Shared::new(true)).unwrap();
        let mut stream = UnixStream::connect(control_path(dir.path())).unwrap();
        stream.write_all(b"hello?\n").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert!(!reply.ok);
        assert!(reply.error.unwrap().contains("not understood"));
    }
}
