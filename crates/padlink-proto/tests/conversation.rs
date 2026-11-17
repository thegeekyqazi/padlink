//! The sender side of protocol version 3, driven by a scripted receiver at
//! byte level (R1.4, R1.5, R1.6, R1.16).
//!
//! Frames are built and taken apart by hand here: four length bytes,
//! big-endian, then the payload (PROTOCOL.md section 3).

use std::time::Duration;

use padlink_proto::{
    Action, Codec, DisplayRequest, Now, OpenDisplay, Pointer, PointerPhase, PointerSource,
    Receiver, StreamPlan,
};
use serde_json::{Value, json};

/// The reference iPad's real `hello`, as captured in docs/spikes.md.
const IPAD_HELLO: &str = r#"{"type":"hello","pv":3,"device":"iPad","id":"322135D1",
  "pixelsWide":2360,"pixelsHigh":1640,"scale":2,
  "panel":{"pixelsWide":2360,"pixelsHigh":1640,"scale":2},
  "displayMaxFrameRate":60,"cursorPort":9001,
  "videoCaps":[{"codec":"h264","maxFrameRate":60},
               {"codec":"hevc","maxWidth":2360,"maxHeight":2360,"maxFrameRate":60}]}"#;

fn at(ms: u64) -> Now {
    Now {
        mono: Duration::from_millis(ms),
        wall_ms: 1_700_000_000_000.0 + ms as f64,
    }
}

fn frame(json: &str) -> Vec<u8> {
    let mut out = (json.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(json.as_bytes());
    out
}

/// Split bytes the sender wrote back into payloads.
fn payloads(bytes: &[u8]) -> Vec<&[u8]> {
    let mut rest = bytes;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        out.push(&rest[4..4 + len]);
        rest = &rest[4 + len..];
    }
    out
}

/// Every control message among `actions`, parsed.
fn sent(actions: &[Action]) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Send(bytes) => Some(payloads(bytes)),
            _ => None,
        })
        .flatten()
        .filter_map(|p| serde_json::from_slice(p).ok())
        .collect()
}

fn sent_types(actions: &[Action]) -> Vec<String> {
    sent(actions)
        .iter()
        .map(|v| v["type"].as_str().unwrap_or("?").to_string())
        .collect()
}

fn display(actions: &[Action]) -> Option<&DisplayRequest> {
    actions.iter().find_map(|a| match a {
        Action::Display(d) => Some(d),
        _ => None,
    })
}

/// A session that has seen the iPad's `hello`.
fn greeted() -> OpenDisplay {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    r.received(at(1), &frame(IPAD_HELLO));
    r
}

const IPAD_PLAN: StreamPlan = StreamPlan {
    codec: Codec::H264,
    width: 2360,
    height: 1640,
    fps: 60,
};

// ---- R1.4: handshake -------------------------------------------------------

#[test]
fn nothing_is_sent_before_the_receiver_says_hello() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    assert!(r.tick(at(100)).is_empty());
    assert!(
        r.video_frame(at(100), &[0, 0, 0, 1, 0x65], true, None)
            .is_empty()
    );
    assert!(r.announce_stream(&IPAD_PLAN).is_empty());
}

#[test]
fn hello_is_answered_with_welcome_and_asks_for_a_display_and_a_keyframe() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    let actions = r.received(at(1), &frame(IPAD_HELLO));

    let welcome = &sent(&actions)[0];
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["pv"], 3);
    assert!(display(&actions).is_some());
    assert!(actions.contains(&Action::Keyframe));
    assert!(r.needs_keyframe());
}

#[test]
fn hello_split_across_reads_is_reassembled() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    let bytes = frame(IPAD_HELLO);
    let (a, b) = bytes.split_at(7);
    assert!(r.received(at(1), a).is_empty());
    assert!(display(&r.received(at(2), b)).is_some());
}

// ---- R1.5: sizing ----------------------------------------------------------

#[test]
fn display_size_comes_from_panel_when_present() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    // The deprecated fields disagree with the panel; the panel wins.
    let hello = r#"{"type":"hello","pv":3,"pixelsWide":1180,"pixelsHigh":820,"scale":3,
        "panel":{"pixelsWide":2360,"pixelsHigh":1640,"scale":2}}"#;
    let actions = r.received(at(1), &frame(hello));
    let d = display(&actions).unwrap();
    assert_eq!((d.width, d.height), (2360, 1640));
}

#[test]
fn display_size_falls_back_to_pixels_wide_and_high() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    let hello = r#"{"type":"hello","pixelsWide":2360,"pixelsHigh":1640,"scale":2}"#;
    let actions = r.received(at(1), &frame(hello));
    let d = display(&actions).unwrap();
    assert_eq!((d.width, d.height, d.max_fps), (2360, 1640, 60));
    assert_eq!(d.codecs, None);
}

#[test]
fn an_invalid_panel_is_ignored_as_a_whole() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    let hello = r#"{"type":"hello","pixelsWide":2360,"pixelsHigh":1640,"scale":2,
        "panel":{"pixelsWide":9999,"pixelsHigh":"tall","scale":2}}"#;
    let actions = r.received(at(1), &frame(hello));
    assert_eq!(display(&actions).unwrap().width, 2360);
}

#[test]
fn the_real_ipad_hello_yields_its_panel_identity_and_both_codecs() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    let actions = r.received(at(1), &frame(IPAD_HELLO));
    let d = display(&actions).unwrap();
    assert_eq!((d.width, d.height, d.max_fps), (2360, 1640, 60));
    assert_eq!(d.device_name.as_deref(), Some("iPad"));
    assert_eq!(d.device_id.as_deref(), Some("322135D1"));
    let codecs: Vec<Codec> = d.codecs.as_ref().unwrap().iter().map(|c| c.codec).collect();
    assert_eq!(codecs, [Codec::H264, Codec::Hevc]);
}

#[test]
fn a_hello_without_any_size_closes_with_a_reason() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    let actions = r.received(at(1), &frame(r#"{"type":"hello","pv":3}"#));
    assert!(display(&actions).is_none());
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Close(why) if why.contains("screen size")))
    );
}

#[test]
fn a_second_hello_rebuilds_the_display_in_the_new_orientation() {
    let mut r = greeted();
    r.video_frame(at(5), &[0, 0, 0, 1, 0x65], true, None);
    assert!(!r.needs_keyframe());

    let rotated = r#"{"type":"hello","pv":3,"pixelsWide":1640,"pixelsHigh":2360,"scale":2,
        "panel":{"pixelsWide":1640,"pixelsHigh":2360,"scale":2}}"#;
    let actions = r.received(at(10), &frame(rotated));
    let d = display(&actions).unwrap();
    assert_eq!((d.width, d.height), (1640, 2360));
    assert!(r.needs_keyframe());
}

// ---- R1.6: streamConfig ----------------------------------------------------

#[test]
fn stream_config_is_sent_when_the_receiver_advertised_video_caps() {
    let mut r = greeted();
    let config = &sent(&r.announce_stream(&IPAD_PLAN))[0];
    assert_eq!(config["type"], "streamConfig");
    assert_eq!(config["codec"], "h264");
    assert_eq!(config["width"], 2360);
    assert_eq!(config["height"], 1640);
    assert_eq!(config["framesPerSecond"], 60);
}

#[test]
fn stream_config_is_not_sent_to_a_receiver_without_video_caps() {
    let mut r = OpenDisplay::new();
    r.connected(at(0));
    r.received(
        at(1),
        &frame(r#"{"type":"hello","pixelsWide":2360,"pixelsHigh":1640,"scale":2}"#),
    );
    assert!(r.announce_stream(&IPAD_PLAN).is_empty());
}

// ---- R1.4: liveness, keyframes, lifecycle ----------------------------------

#[test]
fn ping_is_answered_with_pong_echoing_its_timestamp() {
    let mut r = greeted();
    let actions = r.received(at(50), &frame(r#"{"type":"ping","t":12345.5}"#));
    let pong = &sent(&actions)[0];
    assert_eq!(pong["type"], "pong");
    assert_eq!(pong["t"], 12345.5);
    assert_eq!(pong["mt"], 1_700_000_000_050.0);
}

#[test]
fn the_sender_beats_a_ping_about_every_two_seconds() {
    let mut r = greeted();
    assert!(r.tick(at(1_000)).is_empty());
    assert_eq!(sent_types(&r.tick(at(2_100))), ["ping"]);
    assert!(r.tick(at(2_200)).is_empty());
    // The receiver keeps talking, so the session stays alive.
    r.received(at(4_000), &frame(r#"{"type":"ping","t":1}"#));
    assert_eq!(sent_types(&r.tick(at(4_200))), ["ping"]);
}

#[test]
fn five_seconds_of_silence_closes_the_session_with_a_reason() {
    let mut r = greeted();
    let actions = r.tick(at(5_200));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Close(why) if why.contains("silent"))),
        "{actions:?}"
    );
}

#[test]
fn kf_asks_for_a_keyframe_once_one_has_been_sent() {
    let mut r = greeted();
    r.video_frame(at(5), &[0, 0, 0, 1, 0x65], true, None);
    assert!(!r.needs_keyframe());

    let actions = r.received(at(10), &frame(r#"{"type":"kf"}"#));
    assert_eq!(actions, [Action::Keyframe]);
    assert!(r.needs_keyframe());
    // A repeat while one is already pending asks for nothing more.
    assert!(r.received(at(11), &frame(r#"{"type":"kf"}"#)).is_empty());
}

#[test]
fn sleeping_and_closing_are_reported() {
    let mut r = greeted();
    assert_eq!(
        r.received(at(10), &frame(r#"{"type":"sleeping"}"#)),
        [Action::Sleeping]
    );
    assert_eq!(
        r.received(at(11), &frame(r#"{"type":"closing"}"#)),
        [Action::Closing]
    );
}

#[test]
fn unknown_types_and_fields_are_ignored() {
    let mut r = greeted();
    let actions = r.received(at(10), &frame(r#"{"type":"hologram","depth":3}"#));
    assert!(
        actions.iter().all(|a| matches!(a, Action::Warn(_))),
        "{actions:?}"
    );
    // A known type with a field from the future still works.
    let actions = r.received(
        at(11),
        &frame(r#"{"type":"touch","phase":"began","x":0.5,"y":0.5,"force":0.9}"#),
    );
    assert!(matches!(actions[..], [Action::Pointer(_)]));
}

#[test]
fn a_garbage_length_closes_the_session() {
    let mut r = greeted();
    let actions = r.received(at(10), &[0xff, 0xff, 0xff, 0xff, 1, 2, 3]);
    assert!(actions.iter().any(|a| matches!(a, Action::Close(_))));
}

#[test]
fn stats_expose_the_end_to_end_figures() {
    let mut r = greeted();
    let actions = r.received(
        at(10),
        &frame(r#"{"type":"stats","e2e50":18,"e2e95":21,"fps":52,"mbps":17.5,"rtt":2,"stalls":0,"transport":"USB"}"#),
    );
    let [Action::Stats(s)] = &actions[..] else {
        panic!("{actions:?}")
    };
    assert_eq!((s.e2e_p50_ms, s.e2e_p95_ms), (Some(18.0), Some(21.0)));
    assert_eq!(s.fps, Some(52.0));
    assert_eq!(s.transport.as_deref(), Some("USB"));
}

// ---- video -----------------------------------------------------------------

#[test]
fn frames_are_dropped_until_a_keyframe_then_flow() {
    let mut r = greeted();
    assert!(
        r.video_frame(at(5), &[0, 0, 0, 1, 0x41], false, None)
            .is_empty()
    );

    let idr = [
        0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
    ];
    let actions = r.video_frame(at(6), &idr, true, Some(1_700_000_000_004.0));
    let [Action::Send(bytes)] = &actions[..] else {
        panic!("{actions:?}")
    };
    let payload = payloads(bytes)[0];
    // Telemetry prefix first, then the access unit untouched.
    let start = payload.windows(4).position(|w| w == [0, 0, 0, 1]).unwrap();
    let telemetry: Value = serde_json::from_slice(&payload[..start]).unwrap();
    assert_eq!(
        telemetry,
        json!({"cap": 1_700_000_000_004.0, "snd": 1_700_000_000_006.0})
    );
    assert_eq!(&payload[start..], &idr);

    assert_eq!(
        r.video_frame(at(7), &[0, 0, 0, 1, 0x41], false, None).len(),
        1
    );
}

#[test]
fn a_reconnect_starts_from_scratch() {
    let mut r = greeted();
    r.video_frame(at(5), &[0, 0, 0, 1, 0x65], true, None);
    r.disconnected();
    r.connected(at(100));
    assert!(r.needs_keyframe());
    assert!(
        r.video_frame(at(101), &[0, 0, 0, 1, 0x65], true, None)
            .is_empty()
    );
    assert!(r.announce_stream(&IPAD_PLAN).is_empty());
}

// ---- R1.16: input ----------------------------------------------------------

fn pointers(r: &mut OpenDisplay, messages: &[&str]) -> Vec<(PointerPhase, PointerSource)> {
    messages
        .iter()
        .flat_map(|m| r.received(at(20), &frame(m)))
        .map(|a| match a {
            Action::Pointer(Pointer { phase, source, .. }) => (phase, source),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn touch_phases_become_pointer_phases() {
    let mut r = greeted();
    let got = pointers(
        &mut r,
        &[
            r#"{"type":"touch","phase":"began","x":0.25,"y":0.75}"#,
            r#"{"type":"touch","phase":"moved","x":0.3,"y":0.7}"#,
            r#"{"type":"touch","phase":"ended","x":0.3,"y":0.7}"#,
            r#"{"type":"touch","phase":"cancelled","x":0.3,"y":0.7}"#,
        ],
    );
    use PointerPhase::*;
    assert_eq!(
        got.iter().map(|g| g.0).collect::<Vec<_>>(),
        [Down, Move, Up, Cancel]
    );
    assert!(got.iter().all(|g| g.1 == PointerSource::Finger));
}

#[test]
fn touch_coordinates_pass_through_unchanged() {
    let mut r = greeted();
    let actions = r.received(
        at(20),
        &frame(r#"{"type":"touch","phase":"began","x":0.25,"y":0.75}"#),
    );
    assert_eq!(
        actions,
        [Action::Pointer(Pointer {
            phase: PointerPhase::Down,
            source: PointerSource::Finger,
            x: 0.25,
            y: 0.75
        })]
    );
}

/// D9: the stylus arrives as `pencil` and drives the pointer too.
#[test]
fn pencil_drives_the_pointer_and_a_move_in_the_air_is_a_hover() {
    let mut r = greeted();
    let pencil = |phase: &str| {
        format!(
            r#"{{"type":"pencil","phase":"{phase}","x":0.5,"y":0.5,"pressure":1,
                "azimuth":1.96,"altitude":1.27,"rotation":0,"t":1.0}}"#
        )
    };
    let script = ["move", "hover", "down", "move", "up", "move"].map(pencil);
    let got = pointers(
        &mut r,
        &script.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    use PointerPhase::*;
    assert_eq!(
        got.iter().map(|g| g.0).collect::<Vec<_>>(),
        [Hover, Hover, Down, Move, Up, Hover]
    );
    assert!(got.iter().all(|g| g.1 == PointerSource::Stylus));
}

#[test]
fn proximity_is_accepted_silently() {
    let mut r = greeted();
    let actions = r.received(
        at(20),
        &frame(r#"{"type":"proximity","entering":true,"x":0.5,"y":0.5}"#),
    );
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn scroll_keeps_the_protocols_sign() {
    let mut r = greeted();
    let actions = r.received(at(20), &frame(r#"{"type":"scroll","dx":-3.5,"dy":12}"#));
    assert_eq!(actions, [Action::Scroll { dx: -3.5, dy: 12.0 }]);
}
