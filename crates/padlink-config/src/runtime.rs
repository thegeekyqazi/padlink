//! What the daemon and the CLI share at run time: where the lock file and
//! the control socket live, and the messages that cross the socket.
//!
//! The control protocol is one JSON object per line in each direction.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// `$XDG_RUNTIME_DIR/padlink`.
pub fn runtime_dir(xdg_runtime_dir: &Path) -> PathBuf {
    xdg_runtime_dir.join("padlink")
}

/// Held with a kernel file lock by the one daemon of this session (R1.21).
pub fn lock_path(xdg_runtime_dir: &Path) -> PathBuf {
    runtime_dir(xdg_runtime_dir).join("lock")
}

pub fn control_path(xdg_runtime_dir: &Path) -> PathBuf {
    runtime_dir(xdg_runtime_dir).join("control")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    Status,
    /// Start connecting to the iPad (and keep reconnecting).
    Connect,
    /// Tear the display down and stay disconnected until `Connect`.
    Disconnect,
}

/// What the daemon is doing, in the terms of SPEC.md section 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The user asked to stay disconnected.
    Paused,
    /// No iPad on USB.
    #[default]
    Idle,
    /// An iPad is plugged in; dialing until the receiver app is open.
    Attached,
    /// Connected, waiting for the receiver's `hello`.
    Handshake,
    /// The display exists and frames flow.
    Streaming,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Paused => "disconnected (run `padlink connect`)",
            State::Idle => "waiting for an iPad on USB",
            State::Attached => "iPad plugged in, waiting for the OpenDisplay app",
            State::Handshake => "connected, waiting for the iPad to describe its screen",
            State::Streaming => "streaming",
        }
    }
}

/// A percentile pair in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Latency {
    pub p50_ms: f64,
    pub p95_ms: f64,
}

/// Everything `padlink status` shows (R1.22). Fields that do not apply in
/// the current state are absent.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub state: State,
    /// Why the last attempt failed or ended, in plain words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_mbps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_to_encoded: Option<Latency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoded_to_sent: Option<Latency>,
    /// Capture to display, as measured by the receiver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_to_end: Option<Latency>,
    /// Number of receiver statistics messages in this session (R1.22).
    pub receiver_stats_sequence: u64,
    /// Age at the last status publication; absent before receiver telemetry arrives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_stats_age_ms: Option<f64>,
    /// Frames replaced by a newer one before they were encoded.
    pub frames_replaced: u64,
}

/// The daemon's answer to any request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub status: Status,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error(
        "padlinkd is not running. Start it with `systemctl --user start padlinkd`, \
         or run `padlinkd` in a terminal"
    )]
    NotRunning,
    #[error("padlinkd did not answer: {0}")]
    NoAnswer(String),
    #[error("padlinkd refused: {0}")]
    Refused(String),
}

/// Send one request to the daemon and return its status.
pub fn ask(control: &Path, request: Request) -> Result<Status, ControlError> {
    let mut stream = UnixStream::connect(control).map_err(|_| ControlError::NotRunning)?;
    let no_answer = |e: std::io::Error| ControlError::NoAnswer(e.to_string());
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(no_answer)?;
    let mut line = serde_json::to_string(&request).expect("requests serialise");
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(no_answer)?;
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .map_err(no_answer)?;
    let reply: Reply =
        serde_json::from_str(&answer).map_err(|e| ControlError::NoAnswer(e.to_string()))?;
    if reply.ok {
        Ok(reply.status)
    } else {
        Err(ControlError::Refused(
            reply.error.unwrap_or_else(|| "no reason given".into()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_one_small_json_object() {
        assert_eq!(
            serde_json::to_string(&Request::Status).unwrap(),
            r#"{"cmd":"status"}"#
        );
        let back: Request = serde_json::from_str(r#"{"cmd":"disconnect"}"#).unwrap();
        assert_eq!(back, Request::Disconnect);
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"explode"}"#).is_err());
    }

    #[test]
    fn an_idle_status_is_short_and_round_trips() {
        let json = serde_json::to_string(&Status::default()).unwrap();
        assert_eq!(
            json,
            r#"{"state":"idle","receiver_stats_sequence":0,"frames_replaced":0}"#
        );
        assert_eq!(
            serde_json::from_str::<Status>(&json).unwrap(),
            Status::default()
        );
    }

    #[test]
    fn a_streaming_status_carries_the_figures() {
        let status = Status {
            state: State::Streaming,
            device: Some("iPad".into()),
            transport: Some("USB".into()),
            encoder: Some("h264_vaapi (GPU)".into()),
            resolution: Some("2360x1640".into()),
            fps: Some(59.8),
            end_to_end: Some(Latency {
                p50_ms: 16.0,
                p95_ms: 18.0,
            }),
            ..Status::default()
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains(r#""state":"streaming""#), "{json}");
        assert!(
            json.contains(r#""end_to_end":{"p50_ms":16.0,"p95_ms":18.0}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), status);
    }

    #[test]
    fn a_status_from_a_newer_daemon_with_extra_fields_still_parses() {
        let status: Status =
            serde_json::from_str(r#"{"state":"attached","hologram":true}"#).unwrap();
        assert_eq!(status.state, State::Attached);
    }

    #[test]
    fn no_daemon_is_reported_with_how_to_start_it() {
        let err = ask(Path::new("/nonexistent/padlink/control"), Request::Status).unwrap_err();
        assert!(matches!(err, ControlError::NotRunning));
        assert!(err.to_string().contains("systemctl --user start padlinkd"));
    }

    #[test]
    fn paths_sit_under_the_runtime_dir() {
        let rt = Path::new("/run/user/1000");
        assert_eq!(lock_path(rt), Path::new("/run/user/1000/padlink/lock"));
        assert_eq!(
            control_path(rt),
            Path::new("/run/user/1000/padlink/control")
        );
    }

    #[test]
    fn every_state_has_a_plain_description() {
        for s in [
            State::Paused,
            State::Idle,
            State::Attached,
            State::Handshake,
            State::Streaming,
        ] {
            assert!(!s.label().is_empty());
        }
    }
}
