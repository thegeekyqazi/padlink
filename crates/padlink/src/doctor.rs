//! `padlink doctor` (R2.10): read-only checks, in the order things are
//! needed, stopping at the first failure with what to do about it.
//!
//! It creates no output, takes no lock and sends nothing to the iPad.

use std::path::Path;

use padlink_config::Config;
use padlink_encode::{Backend, Encoder, Kind, Settings};
use padlink_hypr::{Hyprland, instance};
use padlink_proto::OPENDISPLAY_PORT;
use padlink_usbmux::{DEFAULT_SOCKET, Transport};

pub enum Outcome {
    /// Fine. The text is what was found.
    Pass(String),
    /// Works, but the user should know.
    Note(String),
    /// Cannot work. `fix` is the next step.
    Fail { problem: String, fix: String },
}

pub struct Check {
    pub name: &'static str,
    pub run: Box<dyn Fn() -> Outcome>,
}

/// Run the checks in order, writing one line each, and stop after the first
/// failure. Returns whether everything passed.
pub fn report(checks: &[Check], out: &mut impl std::fmt::Write) -> bool {
    for check in checks {
        match (check.run)() {
            Outcome::Pass(found) => {
                let _ = writeln!(out, "ok    {}: {found}", check.name);
            }
            Outcome::Note(text) => {
                let _ = writeln!(out, "note  {}: {text}", check.name);
            }
            Outcome::Fail { problem, fix } => {
                let _ = writeln!(out, "FAIL  {}: {problem}", check.name);
                let _ = writeln!(out, "      Fix: {fix}");
                let _ = writeln!(
                    out,
                    "\nStopped at the first problem. Fix it and run `padlink doctor` again."
                );
                return false;
            }
        }
    }
    let _ = writeln!(out, "\nEverything padlink needs is in place.");
    true
}

fn fail(problem: impl Into<String>, fix: impl Into<String>) -> Outcome {
    Outcome::Fail {
        problem: problem.into(),
        fix: fix.into(),
    }
}

/// The checks for this machine, in R2.10's order.
pub fn checks(runtime: &Path, config_path: Option<std::path::PathBuf>) -> Vec<Check> {
    let usbmuxd = Path::new(DEFAULT_SOCKET);
    let runtime = runtime.to_path_buf();
    let find_hypr = {
        let runtime = runtime.clone();
        move || {
            let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
            instance::find(&runtime, signature.as_deref())
        }
    };
    let render_node = || -> std::path::PathBuf {
        let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("renderD"))
            })
            .collect();
        nodes.sort();
        nodes
            .into_iter()
            .next()
            .unwrap_or_else(|| "/dev/dri/renderD128".into())
    };

    let mut list: Vec<Check> = Vec::new();
    let mut add =
        |name: &'static str, run: Box<dyn Fn() -> Outcome>| list.push(Check { name, run });

    add(
        "config file",
        Box::new(move || match &config_path {
            None => Outcome::Note("no config directory found; using defaults".into()),
            Some(path) => match Config::load(path) {
                Ok(_) if path.exists() => Outcome::Pass(path.display().to_string()),
                Ok(_) => Outcome::Pass(format!("defaults ({} does not exist)", path.display())),
                Err(e) => fail(e.to_string(), "correct or remove the line it names"),
            },
        }),
    );
    add(
        "usbmuxd",
        Box::new(move || match padlink_usbmux::list_devices(usbmuxd) {
            Ok(_) => Outcome::Pass("running".into()),
            Err(e) => fail(e.to_string(), "sudo systemctl start usbmuxd"),
        }),
    );
    add(
        "iPad on USB",
        Box::new(move || {
            let devices = padlink_usbmux::list_devices(usbmuxd).unwrap_or_default();
            match devices.iter().find(|d| d.transport == Transport::Usb) {
                Some(d) => Outcome::Pass(format!(
                    "found (…{}), {} Mbit/s link",
                    d.short_id(),
                    d.speed_bps.unwrap_or(0) / 1_000_000
                )),
                None => fail(
                    "usbmuxd sees no iPad on a cable",
                    "plug the iPad in, unlock it and tap Trust if it asks",
                ),
            }
        }),
    );
    add(
        "receiver app",
        Box::new(move || {
            let devices = padlink_usbmux::list_devices(usbmuxd).unwrap_or_default();
            let Some(d) = devices.iter().find(|d| d.transport == Transport::Usb) else {
                return fail("the iPad went away", "plug it back in");
            };
            // Connecting and closing without a byte is harmless: the
            // receiver drops a newcomer that stays silent.
            match padlink_usbmux::connect(usbmuxd, d.id, OPENDISPLAY_PORT) {
                Ok(_) => Outcome::Pass(format!("listening on port {OPENDISPLAY_PORT}")),
                Err(e) => fail(
                    e.to_string(),
                    "open the OpenDisplay app on the iPad and leave it in the foreground",
                ),
            }
        }),
    );
    add("Hyprland", {
        let find_hypr = find_hypr.clone();
        Box::new(move || {
            let instance = match find_hypr() {
                Ok(i) => i,
                Err(e) => return fail(e.to_string(), "run padlink inside a Hyprland session"),
            };
            let hypr = Hyprland::new(instance.request_socket());
            match (hypr.version(), hypr.dialect()) {
                (Ok(v), Ok(dialect)) if (v.major, v.minor) >= (0, 55) => Outcome::Pass(format!(
                    "{v}, {dialect:?} config, capture through {}",
                    if v >= padlink_hypr::EXT_CAPTURE_SAFE_FROM {
                        "wlr-screencopy (ext-image-copy-capture is not built yet)"
                    } else {
                        "wlr-screencopy"
                    }
                )),
                (Ok(v), Ok(_)) => fail(
                    format!("Hyprland {v} is older than padlink supports"),
                    "update Hyprland to 0.55 or newer",
                ),
                (Err(e), _) | (_, Err(e)) => fail(e.to_string(), "check that Hyprland is running"),
            }
        })
    });
    add("capture permission", {
        let find_hypr = find_hypr.clone();
        Box::new(move || {
            let Ok(instance) = find_hypr() else {
                return fail("Hyprland went away", "start it again");
            };
            match Hyprland::new(instance.request_socket()).enforces_permissions() {
                Ok(false) => Outcome::Pass("permissions are not enforced".into()),
                Ok(true) => Outcome::Note(
                    "`ecosystem:enforce_permissions` is on. Without a rule Hyprland gives \
                     padlink black frames instead of an error. Allow screen capture for the \
                     padlinkd binary in your Hyprland config (legacy syntax: \
                     `permission = /path/to/padlinkd, screencopy, allow`), then reload"
                        .into(),
                ),
                Err(e) => fail(e.to_string(), "check that Hyprland is running"),
            }
        })
    });
    add("Wayland protocols", {
        let (find_hypr, runtime) = (find_hypr.clone(), runtime.clone());
        Box::new(move || {
            let Ok(instance) = find_hypr() else {
                return fail("Hyprland went away", "start it again");
            };
            let names = match padlink_capture::protocols(&runtime.join(&instance.wl_socket)) {
                Ok(names) => names,
                Err(e) => return fail(e.to_string(), "check that Hyprland is running"),
            };
            let needed = [
                "wl_shm",
                "zwlr_screencopy_manager_v1",
                "zwlr_virtual_pointer_manager_v1",
            ];
            let missing: Vec<&str> = needed
                .into_iter()
                .filter(|n| !names.iter().any(|have| have == n))
                .collect();
            if missing.is_empty() {
                Outcome::Pass("screen capture and virtual pointer are offered".into())
            } else {
                fail(
                    format!("the compositor does not offer {}", missing.join(", ")),
                    "padlink needs Hyprland; other compositors are not supported",
                )
            }
        })
    });
    add(
        "GPU access",
        Box::new(move || {
            let node = render_node();
            match std::fs::File::options().read(true).write(true).open(&node) {
                Ok(_) => Outcome::Pass(node.display().to_string()),
                Err(e) => fail(
                    format!("cannot open {}: {e}", node.display()),
                    "add your user to the `render` or `video` group and log in again",
                ),
            }
        }),
    );
    add(
        "hardware encoder",
        Box::new(move || {
            let settings = Settings {
                width: 2360,
                height: 1640,
                fps: 60,
                bitrate_bps: 25_000_000,
                backend: Backend::Vaapi,
                render_node: render_node(),
                codec_options: Vec::new(),
            };
            match Encoder::open(&settings) {
                Ok(_) => Outcome::Pass("h264_vaapi opens at 2360x1640".into()),
                Err(e) => Outcome::Note(format!(
                    "the GPU cannot encode H.264 ({e}). padlink will encode on the CPU. \
                     For AMD and Intel on Fedora: sudo dnf install mesa-va-drivers-freeworld"
                )),
            }
        }),
    );
    add(
        "software encoders",
        Box::new(|| {
            let have: Vec<&str> = [(Kind::X264, "x264"), (Kind::OpenH264, "OpenH264")]
                .into_iter()
                .filter(|(kind, _)| padlink_encode::is_built_in(*kind))
                .map(|(_, name)| name)
                .collect();
            let gpu = padlink_encode::is_built_in(Kind::Vaapi);
            match (have.is_empty(), gpu) {
                (false, _) => Outcome::Pass(have.join(", ")),
                (true, true) => Outcome::Note(
                    "none: there is no fallback if the GPU encoder fails. \
                     sudo dnf install libavcodec-freeworld adds x264"
                        .into(),
                ),
                (true, false) => fail(
                    "this FFmpeg has no H.264 encoder at all",
                    "sudo dnf install libavcodec-freeworld mesa-va-drivers-freeworld",
                ),
            }
        }),
    );
    add("leftover displays", {
        let find_hypr = find_hypr.clone();
        Box::new(move || {
            let Ok(instance) = find_hypr() else {
                return fail("Hyprland went away", "start it again");
            };
            let leftovers = Hyprland::new(instance.request_socket())
                .leftover_outputs()
                .unwrap_or_default();
            if leftovers.is_empty() || crate::daemon_running(&runtime) {
                Outcome::Pass(if leftovers.is_empty() {
                    "none".into()
                } else {
                    format!("{} is in use by the running daemon", leftovers.join(", "))
                })
            } else {
                Outcome::Note(format!(
                    "{} was left behind by a crash; `padlink clean` removes it",
                    leftovers.join(", ")
                ))
            }
        })
    });
    add(
        "autostart",
        Box::new(|| {
            let active = std::process::Command::new("systemctl")
                .args(["--user", "is-active", "--quiet", "graphical-session.target"])
                .status()
                .is_ok_and(|s| s.success());
            if active {
                Outcome::Pass(
                    "graphical-session.target is active; the user unit can start with it".into(),
                )
            } else {
                Outcome::Note(
                    "your Hyprland does not start graphical-session.target, so the systemd \
                     unit will not start on login. Add this line to your Hyprland config \
                     yourself: exec-once = systemctl --user start padlinkd"
                        .into(),
                )
            }
        }),
    );
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &'static str, outcome: fn() -> Outcome) -> Check {
        Check {
            name,
            run: Box::new(outcome),
        }
    }

    #[test]
    fn all_passing_checks_are_listed_and_the_report_says_so() {
        let checks = [
            check("usbmuxd", || Outcome::Pass("running".into())),
            check("autostart", || {
                Outcome::Note("add the exec-once line".into())
            }),
        ];
        let mut out = String::new();
        assert!(report(&checks, &mut out));
        assert_eq!(
            out,
            "ok    usbmuxd: running\nnote  autostart: add the exec-once line\n\n\
             Everything padlink needs is in place.\n"
        );
    }

    #[test]
    fn reporting_stops_at_the_first_failure_and_gives_the_fix() {
        let checks = [
            check("usbmuxd", || Outcome::Pass("running".into())),
            check("iPad on USB", || fail("usbmuxd sees no iPad", "plug it in")),
            check("receiver app", || panic!("must not run after a failure")),
        ];
        let mut out = String::new();
        assert!(!report(&checks, &mut out));
        assert!(
            out.contains("FAIL  iPad on USB: usbmuxd sees no iPad\n      Fix: plug it in\n"),
            "{out}"
        );
        assert!(out.contains("Stopped at the first problem"), "{out}");
        assert!(!out.contains("receiver app"), "{out}");
    }

    #[test]
    fn the_checks_come_in_the_order_of_r2_10() {
        let dir = std::env::temp_dir();
        let names: Vec<&str> = checks(&dir, None).iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "config file",
                "usbmuxd",
                "iPad on USB",
                "receiver app",
                "Hyprland",
                "capture permission",
                "Wayland protocols",
                "GPU access",
                "hardware encoder",
                "software encoders",
                "leftover displays",
                "autostart",
            ]
        );
    }
}
