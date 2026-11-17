//! Desktop notifications (R2.9): one each for connected, disconnected and
//! error, with the reason.
//!
//! Sent with `notify-send`, so there is no D-Bus code here; if it is not
//! installed nothing happens. Never blocks the daemon.

use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Connected,
    Disconnected,
    Error,
}

/// The `notify-send` arguments for a notification.
pub fn arguments(kind: Kind, body: &str) -> Vec<String> {
    let (summary, urgency, icon) = match kind {
        Kind::Connected => ("iPad display connected", "low", "video-display"),
        Kind::Disconnected => ("iPad display disconnected", "low", "video-display"),
        Kind::Error => ("padlink could not use the iPad", "critical", "dialog-error"),
    };
    [
        "--app-name=padlink",
        &format!("--urgency={urgency}"),
        &format!("--icon={icon}"),
        // Replace the previous padlink notification instead of stacking.
        "--hint=string:x-canonical-private-synchronous:padlink",
        summary,
        body,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Show a notification if they are enabled. Development runs in the nested
/// test bench stay quiet: they would otherwise pop up on the real desktop.
pub fn send(enabled: bool, kind: Kind, body: &str) {
    if !enabled || std::env::var_os("PADLINK_TESTBENCH").is_some() {
        return;
    }
    let _ = Command::new("notify-send")
        .args(arguments(kind, body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        // Reap it without waiting here.
        .map(|mut child| std::thread::spawn(move || child.wait()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_its_own_summary_and_errors_are_urgent() {
        let connected = arguments(Kind::Connected, "iPad, 2360x1640");
        assert!(connected.contains(&"iPad display connected".to_string()));
        assert!(connected.contains(&"--urgency=low".to_string()));
        assert_eq!(connected.last().unwrap(), "iPad, 2360x1640");

        let error = arguments(Kind::Error, "no encoder could be opened");
        assert!(error.contains(&"--urgency=critical".to_string()));
        assert!(error.contains(&"padlink could not use the iPad".to_string()));

        assert!(
            arguments(Kind::Disconnected, "the cable was unplugged")
                .contains(&"iPad display disconnected".to_string())
        );
    }

    #[test]
    fn disabled_notifications_do_nothing() {
        // Must return without spawning anything; there is nothing to
        // observe beyond it not panicking.
        send(false, Kind::Error, "ignored");
    }
}
