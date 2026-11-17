//! `padlink`: the command line for the padlink daemon.

mod doctor;
mod show;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use padlink_config::runtime::{ControlError, Request, ask, control_path, lock_path};
use padlink_hypr::{Hyprland, instance};

#[derive(Parser)]
#[command(
    version,
    about = "Use an iPad as an extended Hyprland monitor over USB"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show what padlink is doing: state, device, encoder, latency
    Status {
        /// Machine-readable output for scripts and bar widgets
        #[arg(long)]
        json: bool,
    },
    /// Extend the desktop onto the plugged-in iPad
    Connect,
    /// Remove the iPad's display and stay disconnected
    Disconnect,
    /// Remove virtual displays left behind by a daemon that crashed
    Clean,
    /// Follow the daemon's log
    Logs,
    /// Check everything padlink needs and say how to fix what is missing
    Doctor,
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("padlink: {why}");
            ExitCode::FAILURE
        }
    }
}

fn runtime_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set: run padlink inside your desktop session".into())
}

fn run(command: Command) -> Result<(), String> {
    let runtime = runtime_dir()?;
    let control = control_path(&runtime);
    let ask = |request| ask(&control, request).map_err(|e: ControlError| e.to_string());
    match command {
        Command::Status { json } => {
            let status = ask(Request::Status)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&status).expect("status serialises")
                );
            } else {
                print!("{}", show::status(&status));
            }
        }
        Command::Connect => {
            ask(Request::Connect)?;
            println!("Connecting. Keep the OpenDisplay app open on the iPad.");
            println!("`padlink status` shows progress.");
        }
        Command::Disconnect => {
            ask(Request::Disconnect)?;
            println!("Disconnected. The iPad's display is removed.");
        }
        Command::Clean => clean(&runtime)?,
        Command::Doctor => {
            let mut out = String::new();
            let ok = doctor::report(
                &doctor::checks(&runtime, padlink_config::default_path()),
                &mut out,
            );
            print!("{out}");
            if !ok {
                return Err("doctor found a problem".into());
            }
        }
        Command::Logs => {
            use std::os::unix::process::CommandExt;
            let error = std::process::Command::new("journalctl")
                .args([
                    "--user",
                    "--unit",
                    "padlinkd.service",
                    "--follow",
                    "--lines",
                    "100",
                ])
                .exec();
            return Err(format!("cannot run journalctl: {error}"));
        }
    }
    Ok(())
}

/// Whether a daemon holds the session lock right now.
pub(crate) fn daemon_running(runtime: &Path) -> bool {
    let Ok(file) = std::fs::File::open(lock_path(runtime)) else {
        return false;
    };
    // If the lock can be taken, nobody holds it. It is released on return.
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared).is_err()
}

/// R1.20: remove leftover `padlink-*` outputs. Refused while a daemon runs,
/// because its output is being captured and removing it under a capture can
/// crash Hyprland 0.56.
fn clean(runtime: &Path) -> Result<(), String> {
    if daemon_running(runtime) {
        return Err(
            "padlinkd is running, so its display is in use. Run `padlink disconnect` to \
             remove it; `clean` is for displays left behind after a crash"
                .into(),
        );
    }
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
    let instance = instance::find(runtime, signature.as_deref()).map_err(|e| e.to_string())?;
    let hypr = Hyprland::new(instance.request_socket());
    let leftovers = hypr.leftover_outputs().map_err(|e| e.to_string())?;
    if leftovers.is_empty() {
        println!("Nothing to clean: no padlink display is left over.");
        return Ok(());
    }
    for name in leftovers {
        hypr.remove_output(&name).map_err(|e| e.to_string())?;
        println!("Removed {name}.");
    }
    Ok(())
}
