//! padlinkd: the padlink daemon.
//!
//! This first version runs one session: it finds the iPad through usbmuxd,
//! dials the receiver, extends the desktop onto it and tears everything down
//! on Ctrl+C or SIGTERM. Config file, control socket, auto-connect and
//! reconnect come next.

mod session;
mod slot;
mod stats;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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

    // R1.20: outputs left behind by a daemon that was killed.
    for name in hypr.leftover_outputs().map_err(|e| e.to_string())? {
        info!("removing leftover virtual display {name}");
        if let Err(e) = hypr.remove_output(&name) {
            warn!("{e}");
        }
    }

    let usbmuxd = Path::new(DEFAULT_SOCKET);
    let device = wait_for_device(usbmuxd, &stop)?;
    let Some(device) = device else { return Ok(()) };
    info!(
        "iPad on USB (…{}), {} Mbit/s link",
        device.short_id(),
        device.speed_bps.unwrap_or(0) / 1_000_000
    );

    if config.capture == padlink_config::Capture::Ext {
        warn!("`capture = \"ext\"` is not built yet; using wlr-screencopy");
    }
    let config = SessionConfig {
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
        render_node: render_node(&config),
        cursor: true,
        wayland_socket: runtime.join(&instance.wl_socket),
    };

    let Some(transport) = dial(usbmuxd, &device, &stop)? else {
        return Ok(());
    };
    info!("connected to the receiver over USB");
    let session = Session::new(OpenDisplay::new(), transport, &config, &hypr, dialect)
        .map_err(|e| format!("cannot set up the connection: {e}"))?;
    match session.run(&stop) {
        SessionEnd::Failed(why) => Err(why),
        SessionEnd::Disconnected(why) => {
            info!("disconnected: {why}");
            Ok(())
        }
        SessionEnd::ReceiverSleeping => {
            info!("the iPad was locked or the app left the foreground");
            Ok(())
        }
        SessionEnd::ReceiverClosed => {
            info!("the OpenDisplay app was closed");
            Ok(())
        }
        SessionEnd::Stopped => Ok(()),
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

/// The first iPad on a cable, waiting for one to be plugged in.
fn wait_for_device(usbmuxd: &Path, stop: &AtomicBool) -> Result<Option<Device>, String> {
    let mut told = false;
    while !stop.load(Ordering::Relaxed) {
        let devices = padlink_usbmux::list_devices(usbmuxd).map_err(|e| e.to_string())?;
        if let Some(device) = devices.into_iter().find(|d| d.transport == Transport::Usb) {
            return Ok(Some(device));
        }
        if !std::mem::replace(&mut told, true) {
            info!("no iPad on USB yet: plug it in and unlock it");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(None)
}

/// R1.3: dial port 9000, backing off from 0.5 s to 3 s while the receiver
/// app is not open.
fn dial(
    usbmuxd: &Path,
    device: &Device,
    stop: &AtomicBool,
) -> Result<Option<std::os::unix::net::UnixStream>, String> {
    let mut wait = Duration::from_millis(500);
    let mut told = false;
    while !stop.load(Ordering::Relaxed) {
        match padlink_usbmux::connect(usbmuxd, device.id, OPENDISPLAY_PORT) {
            Ok(stream) => return Ok(Some(stream)),
            Err(e @ padlink_usbmux::Error::Refused { .. }) => {
                if !std::mem::replace(&mut told, true) {
                    info!("{e}. Waiting for it");
                }
            }
            Err(e) => return Err(e.to_string()),
        }
        // Sleep in short steps so Ctrl+C is prompt.
        let until = std::time::Instant::now() + wait;
        while std::time::Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        wait = (wait * 3 / 2).min(Duration::from_secs(3));
    }
    Ok(None)
}
