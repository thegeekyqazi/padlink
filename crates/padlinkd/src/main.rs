//! padlinkd: the padlink daemon.
//!
//! It holds the session lock, listens on the control socket for the
//! `padlink` CLI, and drives the state machine: wait for an iPad on USB,
//! dial the receiver until its app is open, run a session, and start again
//! when the session ends. SIGINT, SIGTERM and SIGHUP tear the display down
//! and exit.

mod control;
mod machine;
mod session;
mod slot;
mod stats;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use control::Shared;
use machine::{Command, Ended, Event, Machine, Phase};
use padlink_config::runtime::State;
use padlink_config::{Config, Position};
use padlink_encode::Backend;
use padlink_hypr::{Hyprland, Placement, instance, output_name};
use padlink_proto::{OPENDISPLAY_PORT, OpenDisplay};
use padlink_usbmux::{DEFAULT_SOCKET, Device, Transport};
use session::{Session, SessionConfig, SessionEnd};
use tracing::{error, info, warn};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PADLINK_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            error!("{why}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    // Before anything else: a broken config must stop the daemon with a
    // message, not half-way through setting up a display.
    let config = load_config()?;
    let stop = Arc::new(AtomicBool::new(false));
    // R1.19: the same teardown on every way of being asked to stop.
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, stop.clone())
            .map_err(|e| format!("cannot install signal handlers: {e}"))?;
    }

    let runtime = PathBuf::from(
        std::env::var("XDG_RUNTIME_DIR")
            .map_err(|_| "XDG_RUNTIME_DIR is not set: run padlinkd inside your desktop session")?,
    );
    // R1.21: one daemon per session.
    let _lock = control::acquire_lock(&runtime)?;

    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
    let instance = instance::find(&runtime, signature.as_deref()).map_err(|e| e.to_string())?;
    let hypr = Hyprland::new(instance.request_socket());
    let version = hypr.version().map_err(|e| e.to_string())?;
    let dialect = hypr.dialect().map_err(|e| e.to_string())?;
    info!(
        "Hyprland {version} ({dialect:?} config), instance {}",
        instance.wl_socket
    );
    if hypr.enforces_permissions().unwrap_or(false) {
        warn!(
            "Hyprland enforces permissions: without a screencopy permission rule for \
             padlinkd the iPad will show a black picture"
        );
    }
    if config.capture == padlink_config::Capture::Ext {
        warn!("`capture = \"ext\"` is not built yet; using wlr-screencopy");
    }

    // R1.20: outputs left behind by a daemon that was killed. Safe here
    // because the lock proves no other daemon is using them.
    for name in hypr.leftover_outputs().map_err(|e| e.to_string())? {
        info!("removing leftover virtual display {name}");
        if let Err(e) = hypr.remove_output(&name) {
            warn!("{e}");
        }
    }

    let shared = Shared::new(config.autoconnect);
    control::serve(&runtime, shared.clone())?;

    let usbmuxd = Path::new(DEFAULT_SOCKET);
    let wayland_socket = runtime.join(&instance.wl_socket);
    let mut machine = Machine::new(config.autoconnect);
    let mut dial_at: Option<Instant> = None;
    let mut device: Option<Device> = None;
    let mut message: Option<String> = None;
    let mut usbmuxd_down = false;
    let mut paused_because: Option<String> = None;

    while !stop.load(Ordering::Relaxed) {
        // What the user wants.
        let command = match (shared.wanted(), machine.phase()) {
            (false, phase) if phase != Phase::Paused => {
                // Keep the reason when padlink paused itself.
                message = paused_because.take();
                machine.on(Event::UserDisconnect)
            }
            (true, Phase::Paused) => machine.on(Event::UserConnect),
            _ => Command::Nothing,
        };
        schedule(command, &mut dial_at);

        // What is plugged in.
        let present = match padlink_usbmux::list_devices(usbmuxd) {
            Ok(devices) => {
                usbmuxd_down = false;
                devices.into_iter().find(|d| d.transport == Transport::Usb)
            }
            Err(e) => {
                if !std::mem::replace(&mut usbmuxd_down, true) {
                    warn!("{e}");
                }
                message = Some(e.to_string());
                None
            }
        };
        if present.as_ref().map(|d| d.id) != device.as_ref().map(|d| d.id) {
            if device.take().is_some() {
                info!("the iPad was unplugged");
                schedule(machine.on(Event::DeviceDetached), &mut dial_at);
            }
            if let Some(d) = present {
                info!(
                    "iPad on USB (…{}), {} Mbit/s link",
                    d.short_id(),
                    d.speed_bps.unwrap_or(0) / 1_000_000
                );
                device = Some(d);
                message = None;
                schedule(machine.on(Event::DeviceAttached), &mut dial_at);
            }
        }

        shared.reset(
            match machine.phase() {
                Phase::Paused => State::Paused,
                Phase::Idle => State::Idle,
                Phase::Attached | Phase::Connected => State::Attached,
            },
            message.clone(),
        );

        let due = dial_at.is_some_and(|at| Instant::now() >= at);
        let (Phase::Attached, true, Some(d)) = (machine.phase(), due, &device) else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        dial_at = None;

        // R1.3: dial; a refusal means the receiver app is not open.
        let transport = match padlink_usbmux::connect(usbmuxd, d.id, OPENDISPLAY_PORT) {
            Ok(transport) => transport,
            Err(e) => {
                let text = e.to_string();
                if message.as_deref() != Some(&text) {
                    info!("{text}");
                }
                message = Some(text);
                schedule(machine.on(Event::DialRefused), &mut dial_at);
                continue;
            }
        };
        machine.on(Event::DialSucceeded);
        info!("connected to the receiver over USB");

        let session_config = session_config(&config, d, &wayland_socket, &instance.event_socket());
        let session = Session::new(
            OpenDisplay::new(),
            transport,
            &session_config,
            &hypr,
            dialect,
            shared.clone(),
        )
        .map_err(|e| format!("cannot set up the connection: {e}"))?;
        let end = session.run(|| stop.load(Ordering::Relaxed) || !shared.wanted());

        let (ended, why) = match end {
            SessionEnd::Stopped => continue,
            SessionEnd::ReceiverSleeping => (
                Ended::WillReturn,
                "the iPad was locked or the app left the foreground".to_string(),
            ),
            SessionEnd::ReceiverClosed => (
                Ended::AppClosed,
                "the OpenDisplay app was closed".to_string(),
            ),
            SessionEnd::Disconnected(why) => (Ended::WillReturn, why),
            SessionEnd::Failed(why) => {
                error!("{why}");
                (Ended::Failed, why)
            }
            SessionEnd::GaveUp(why) => {
                error!("{why}");
                // Stay disconnected; the top of the loop pauses the machine.
                shared.set_wanted(false);
                paused_because = Some(why);
                continue;
            }
        };
        info!("disconnected: {why}");
        message = Some(why);
        schedule(machine.on(Event::SessionEnded(ended)), &mut dial_at);
    }
    info!("padlinkd stopped");
    Ok(())
}

/// Turn a machine command into the time of the next dial.
fn schedule(command: Command, dial_at: &mut Option<Instant>) {
    if let Command::Dial(after) = command {
        *dial_at = Some(Instant::now() + after);
    }
}

fn session_config(
    config: &Config,
    device: &Device,
    wayland_socket: &Path,
    hypr_events: &Path,
) -> SessionConfig {
    SessionConfig {
        output_name: output_name(&device.short_id()),
        scale: config.scale,
        placement: match config.position {
            Position::Right => Placement::Right,
            Position::Left => Placement::Left,
            Position::Above => Placement::Above,
            Position::Below => Placement::Below,
            Position::At(x, y) => Placement::At(x, y),
        },
        fps: config.fps,
        bitrate_bps: config.bitrate_bps(),
        encoder: match config.encoder {
            padlink_config::Encoder::Auto => Backend::Auto,
            padlink_config::Encoder::Vaapi => Backend::Vaapi,
            padlink_config::Encoder::X264 => Backend::X264,
            padlink_config::Encoder::Openh264 => Backend::OpenH264,
        },
        render_node: render_node(config),
        cursor: true,
        wayland_socket: wayland_socket.to_path_buf(),
        hypr_events: hypr_events.to_path_buf(),
    }
}

/// The config file named by `--config <path>`, or the default location.
fn load_config() -> Result<Config, String> {
    let mut args = std::env::args().skip(1);
    let path = match (args.next().as_deref(), args.next()) {
        (None, _) => padlink_config::default_path()
            .ok_or("cannot find the config directory: neither XDG_CONFIG_HOME nor HOME is set")?,
        (Some("--config"), Some(path)) => PathBuf::from(path),
        _ => return Err("usage: padlinkd [--config <path>]".into()),
    };
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    if path.exists() {
        info!("config: {}", path.display());
    } else {
        info!("config: defaults ({} does not exist)", path.display());
    }
    Ok(config)
}

/// The GPU to encode on: the configured one, or the first render node.
fn render_node(config: &Config) -> PathBuf {
    if let Some(node) = &config.render_node {
        return node.clone();
    }
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
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
}
