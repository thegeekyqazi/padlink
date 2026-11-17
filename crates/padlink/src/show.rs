//! `padlink status` for people (R1.22).

use std::fmt::Write;

use padlink_config::runtime::{Latency, State, Status};

fn latency(l: &Latency) -> String {
    format!("{:.0} ms typical, {:.0} ms worst 5%", l.p50_ms, l.p95_ms)
}

pub fn status(s: &Status) -> String {
    let mut out = String::new();
    let mut line = |label: &str, value: &str| {
        let _ = writeln!(out, "{label:<19}{value}");
    };
    line("State", s.state.label());
    if let Some(m) = &s.message {
        line("Last event", m);
    }
    if s.state != State::Streaming {
        return out;
    }
    let text = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    line(
        "Device",
        &format!("{} over {}", text(&s.device), text(&s.transport)),
    );
    line(
        "Display",
        &format!("{} as {}", text(&s.resolution), text(&s.output)),
    );
    line("Encoder", &text(&s.encoder));
    line("Capture", &text(&s.capture));
    if let (Some(fps), Some(mbps)) = (s.fps, s.bitrate_mbps) {
        line("Stream", &format!("{fps:.0} fps, {mbps:.1} Mbit/s"));
    }
    if let Some(l) = &s.end_to_end {
        line("Latency on iPad", &latency(l));
    }
    if let Some(l) = &s.capture_to_encoded {
        line("Capture to encoded", &latency(l));
    }
    if let Some(l) = &s.encoded_to_sent {
        line("Encoded to sent", &latency(l));
    }
    if s.frames_replaced > 0 {
        line(
            "Frames skipped",
            &format!("{} (the encoder was busy)", s.frames_replaced),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_shows_the_state_and_the_last_event_only() {
        let s = Status {
            state: State::Attached,
            message: Some("nothing is listening on port 9000".into()),
            ..Status::default()
        };
        let text = status(&s);
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("waiting for the OpenDisplay app"), "{text}");
        assert!(
            text.contains("Last event         nothing is listening"),
            "{text}"
        );
    }

    #[test]
    fn streaming_shows_every_figure_of_r1_22() {
        let s = Status {
            state: State::Streaming,
            device: Some("iPad".into()),
            transport: Some("USB".into()),
            output: Some("padlink-ba601e".into()),
            encoder: Some("h264_vaapi (GPU)".into()),
            capture: Some("wlr-screencopy, shared memory".into()),
            resolution: Some("2360x1640".into()),
            fps: Some(59.8),
            bitrate_mbps: Some(4.25),
            capture_to_encoded: Some(Latency {
                p50_ms: 13.5,
                p95_ms: 14.0,
            }),
            encoded_to_sent: Some(Latency {
                p50_ms: 0.1,
                p95_ms: 0.1,
            }),
            end_to_end: Some(Latency {
                p50_ms: 16.0,
                p95_ms: 18.0,
            }),
            frames_replaced: 3,
            ..Status::default()
        };
        let text = status(&s);
        for want in [
            "State              streaming",
            "Device             iPad over USB",
            "Display            2360x1640 as padlink-ba601e",
            "Encoder            h264_vaapi (GPU)",
            "Capture            wlr-screencopy, shared memory",
            "Stream             60 fps, 4.2 Mbit/s",
            "Latency on iPad    16 ms typical, 18 ms worst 5%",
            "Capture to encoded 14 ms typical, 14 ms worst 5%",
            "Frames skipped     3 (the encoder was busy)",
        ] {
            assert!(text.contains(want), "missing `{want}` in:\n{text}");
        }
    }

    #[test]
    fn figures_that_are_not_known_yet_are_left_out() {
        let s = Status {
            state: State::Streaming,
            ..Status::default()
        };
        let text = status(&s);
        assert!(!text.contains("Latency"), "{text}");
        assert!(!text.contains("Frames skipped"), "{text}");
        assert!(text.contains("Device             - over -"), "{text}");
    }
}
