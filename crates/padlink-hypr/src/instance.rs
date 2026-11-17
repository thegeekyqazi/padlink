//! Finding the Hyprland instance to talk to.
//!
//! Each running Hyprland has a directory `$XDG_RUNTIME_DIR/hypr/<signature>/`
//! holding `hyprland.lock` (the pid, then the Wayland socket name) and the
//! IPC sockets. `HYPRLAND_INSTANCE_SIGNATURE` names the instance a process
//! was started under, but a systemd user service may not have it (R2.8), so
//! the directory is the source of truth.
//!
//! Adapted from OpenDisplay's `od-sender/src/backend/hyprland/instance.rs`
//! (GPL-3.0).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub signature: String,
    pub pid: u32,
    /// The Wayland socket name, such as `wayland-1`.
    pub wl_socket: String,
    /// Start time taken from the signature (`<commit>_<time>_<random>`).
    pub started: u64,
    dir: PathBuf,
}

impl Instance {
    /// The request socket: one command per connection.
    pub fn request_socket(&self) -> PathBuf {
        self.dir.join(".socket.sock")
    }

    /// The event socket: a stream of `name>>data` lines.
    pub fn event_socket(&self) -> PathBuf {
        self.dir.join(".socket2.sock")
    }
}

/// Every instance under `runtime_dir` with a readable lock file, oldest
/// first. An instance that crashed can leave its directory behind; see
/// [`Instance::pid`] and [`is_alive`].
pub fn list(runtime_dir: &Path) -> Vec<Instance> {
    let Ok(entries) = std::fs::read_dir(runtime_dir.join("hypr")) else {
        return Vec::new();
    };
    let mut out: Vec<Instance> = entries
        .flatten()
        .filter_map(|entry| {
            let signature = entry.file_name().to_string_lossy().into_owned();
            let lock = std::fs::read_to_string(entry.path().join("hyprland.lock")).ok()?;
            let mut lines = lock.lines();
            let pid = lines.next()?.trim().parse().ok()?;
            let wl_socket = lines.next()?.trim().to_string();
            let started = signature
                .split('_')
                .nth(1)
                .and_then(|t| t.parse().ok())
                .unwrap_or(0);
            Some(Instance {
                signature,
                pid,
                wl_socket,
                started,
                dir: entry.path(),
            })
        })
        .collect();
    out.sort_by(|a, b| (a.started, &a.signature).cmp(&(b.started, &b.signature)));
    out
}

/// Whether the instance's process still exists.
pub fn is_alive(instance: &Instance) -> bool {
    Path::new("/proc").join(instance.pid.to_string()).exists()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FindError {
    #[error(
        "no running Hyprland found under {0}/hypr. padlink needs a Hyprland session; \
         start it from inside one"
    )]
    NoneRunning(String),
    #[error(
        "this terminal points at a Hyprland instance that is no longer running \
         ({wanted}; running: {running}). It was probably set up for a nested test \
         instance that has ended. Open a new terminal and run the command there"
    )]
    NotRunning { wanted: String, running: String },
}

/// Pick the instance to use. A signature from the environment must match
/// exactly, so a stale variable fails instead of landing on another session.
/// Without one, the oldest live instance is used: that is the session the
/// user logged into, and anything nested inside it started later.
pub fn find(runtime_dir: &Path, signature: Option<&str>) -> Result<Instance, FindError> {
    find_among(list(runtime_dir), runtime_dir, signature, is_alive)
}

fn find_among(
    all: Vec<Instance>,
    runtime_dir: &Path,
    signature: Option<&str>,
    alive: impl Fn(&Instance) -> bool,
) -> Result<Instance, FindError> {
    let mut live = all.into_iter().filter(|i| alive(i));
    match signature.filter(|s| !s.is_empty()) {
        Some(wanted) => {
            let live: Vec<Instance> = live.collect();
            live.iter()
                .find(|i| i.signature == wanted)
                .cloned()
                .ok_or_else(|| FindError::NotRunning {
                    wanted: wanted.to_string(),
                    running: if live.is_empty() {
                        "none".into()
                    } else {
                        live.iter()
                            .map(|i| i.signature.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                })
        }
        None => live
            .next()
            .ok_or_else(|| FindError::NoneRunning(runtime_dir.display().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = "efb5_1794874075_297451947";
    const NESTED: &str = "efb5_1794878937_933109280";

    fn runtime(instances: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (sig, lock) in instances {
            let d = dir.path().join("hypr").join(sig);
            std::fs::create_dir_all(&d).unwrap();
            if !lock.is_empty() {
                std::fs::write(d.join("hyprland.lock"), lock).unwrap();
            }
        }
        dir
    }

    fn two() -> tempfile::TempDir {
        runtime(&[
            (NESTED, "165241\nwayland-2\n"),
            (LIVE, "2528\nwayland-1\n"),
            ("crashed_without_lock", ""),
        ])
    }

    #[test]
    fn instances_are_listed_oldest_first_and_lockless_ones_skipped() {
        let rt = two();
        let all = list(rt.path());
        assert_eq!(
            all.iter().map(|i| i.signature.as_str()).collect::<Vec<_>>(),
            [LIVE, NESTED]
        );
        assert_eq!((all[0].pid, all[0].wl_socket.as_str()), (2528, "wayland-1"));
        assert!(
            all[1]
                .request_socket()
                .ends_with(format!("hypr/{NESTED}/.socket.sock"))
        );
        assert!(all[1].event_socket().ends_with(".socket2.sock"));
    }

    #[test]
    fn a_missing_runtime_dir_lists_nothing() {
        assert!(list(Path::new("/nonexistent/padlink")).is_empty());
    }

    #[test]
    fn the_signature_from_the_environment_wins_and_must_match_exactly() {
        let rt = two();
        let all = || list(rt.path());
        let got = find_among(all(), rt.path(), Some(NESTED), |_| true).unwrap();
        assert_eq!(got.wl_socket, "wayland-2");

        let err = find_among(all(), rt.path(), Some("efb5_1794878937"), |_| true).unwrap_err();
        assert!(err.to_string().contains(LIVE), "{err}");
    }

    #[test]
    fn without_a_signature_the_oldest_live_instance_is_used() {
        let rt = two();
        let all = || list(rt.path());
        assert_eq!(
            find_among(all(), rt.path(), None, |_| true)
                .unwrap()
                .signature,
            LIVE
        );
        // An empty variable counts as unset.
        assert_eq!(
            find_among(all(), rt.path(), Some(""), |_| true)
                .unwrap()
                .signature,
            LIVE
        );
        // The login session crashed and left its directory: skip it.
        let got = find_among(all(), rt.path(), None, |i| i.signature != LIVE).unwrap();
        assert_eq!(got.signature, NESTED);
    }

    #[test]
    fn a_dead_instance_named_by_the_environment_is_an_error() {
        let rt = two();
        let err = find_among(list(rt.path()), rt.path(), Some(LIVE), |i| {
            i.signature != LIVE
        })
        .unwrap_err();
        assert!(matches!(err, FindError::NotRunning { .. }));
    }

    #[test]
    fn no_instance_at_all_says_so() {
        let rt = runtime(&[]);
        let err = find(rt.path(), None).unwrap_err();
        assert!(err.to_string().contains("no running Hyprland"), "{err}");
    }
}
