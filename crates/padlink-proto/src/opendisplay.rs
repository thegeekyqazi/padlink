//! The OpenDisplay receiver (protocol version 3), sender side.
//!
//! The handshake, liveness and keyframe rules live in upstream's
//! `opendisplay-session::sender::SenderSession`; this file translates
//! between it and padlink's [`Receiver`] trait and applies the choices that
//! are padlink's own: where the display size comes from (R1.5), when
//! `streamConfig` is sent (R1.6), and stylus input acting as a pointer
//! (R1.16 as widened in docs/spec-deviations.md, D9).

use opendisplay_proto::control::{
    Hello, Pencil, PencilPhase, StreamConfig, Touch, TouchPhase, VideoCap,
};
use opendisplay_session::sender::{SenderAction, SenderConfig, SenderEvent, SenderSession};
use opendisplay_session::{CloseReason, Now as OdNow};

use crate::{
    Action, Codec, CodecLimit, DisplayRequest, LinkStats, Now, Pointer, PointerPhase,
    PointerSource, Receiver, StreamPlan,
};

/// Panel refresh rate assumed when the receiver does not state one.
const DEFAULT_PANEL_FPS: u32 = 60;

pub struct OpenDisplay {
    session: SenderSession,
    /// Whether the last `hello` carried `videoCaps`.
    advertised_caps: bool,
    /// The stylus is touching the surface. A `move` while it is not is a
    /// hover (PROTOCOL.md section 6.1).
    pen_down: bool,
}

impl OpenDisplay {
    pub fn new() -> Self {
        OpenDisplay {
            session: SenderSession::new(SenderConfig {
                // The UDP cursor channel must stay off on the USB binding
                // (PROTOCOL.md section 6.3), and padlink sends no cursor
                // messages yet.
                use_cursor_udp: false,
                ..SenderConfig::default()
            }),
            advertised_caps: false,
            pen_down: false,
        }
    }

    fn translate(&mut self, actions: Vec<SenderAction>) -> Vec<Action> {
        let mut out = Vec::with_capacity(actions.len());
        for action in actions {
            match action {
                SenderAction::Send(bytes) => out.push(Action::Send(bytes)),
                SenderAction::Hello(hello) => {
                    self.advertised_caps = hello.video_caps.is_some();
                    self.pen_down = false;
                    match display_request(&hello) {
                        Some(display) => out.push(Action::Display(display)),
                        None => out.push(Action::Close(
                            "the receiver did not report a usable screen size. \
                             Update the OpenDisplay app on the iPad"
                                .into(),
                        )),
                    }
                }
                SenderAction::RequestKeyframe => out.push(Action::Keyframe),
                SenderAction::Touch(t) => out.extend(touch(&t).map(Action::Pointer)),
                SenderAction::Pencil(p) => out.extend(self.pencil(&p).map(Action::Pointer)),
                SenderAction::Scroll(s) => {
                    if s.dx.is_finite() && s.dy.is_finite() {
                        out.push(Action::Scroll { dx: s.dx, dy: s.dy });
                    }
                }
                SenderAction::Stats(v) => out.push(Action::Stats(link_stats(&v))),
                SenderAction::ReceiverSleeping => out.push(Action::Sleeping),
                SenderAction::ReceiverClosing => out.push(Action::Closing),
                SenderAction::Close(reason) => out.push(Action::Close(close_text(reason))),
                SenderAction::Warn(text) => out.push(Action::Warn(text)),
                // Hover range entry and exit: used by the tablet device in
                // Phase 3 (R3.4), nothing to do for a pointer.
                SenderAction::Proximity(_) => {}
                // Never produced with the UDP cursor channel off.
                SenderAction::SendCursorDatagram(_) => {}
            }
        }
        out
    }

    fn pencil(&mut self, p: &Pencil) -> Option<Pointer> {
        let phase = match p.phase {
            PencilPhase::Down => {
                self.pen_down = true;
                PointerPhase::Down
            }
            PencilPhase::Up => {
                self.pen_down = false;
                PointerPhase::Up
            }
            PencilPhase::Move if self.pen_down => PointerPhase::Move,
            PencilPhase::Move | PencilPhase::Hover => PointerPhase::Hover,
        };
        pointer(phase, PointerSource::Stylus, p.x, p.y)
    }
}

impl Default for OpenDisplay {
    fn default() -> Self {
        Self::new()
    }
}

fn od_now(now: Now) -> OdNow {
    OdNow::new(now.mono, now.wall_ms)
}

fn pointer(phase: PointerPhase, source: PointerSource, x: f64, y: f64) -> Option<Pointer> {
    (x.is_finite() && y.is_finite()).then_some(Pointer {
        phase,
        source,
        x,
        y,
    })
}

fn touch(t: &Touch) -> Option<Pointer> {
    let phase = match t.phase {
        TouchPhase::Began => PointerPhase::Down,
        TouchPhase::Moved => PointerPhase::Move,
        TouchPhase::Ended => PointerPhase::Up,
        TouchPhase::Cancelled => PointerPhase::Cancel,
    };
    pointer(phase, PointerSource::Finger, t.x, t.y)
}

/// R1.5: the size comes from `hello.panel` when it is present and valid,
/// otherwise from `pixelsWide` and `pixelsHigh`. The deprecated top-level
/// `scale` is never read; the output's scale is the user's setting.
fn display_request(hello: &Hello) -> Option<DisplayRequest> {
    let (width, height) = match hello.panel_facts() {
        Some(panel) => (panel.pixels_wide, panel.pixels_high),
        None => (hello.pixels_wide, hello.pixels_high),
    };
    if width < 2 || height < 2 {
        return None;
    }
    Some(DisplayRequest {
        width,
        height,
        max_fps: hello
            .display_max_frame_rate
            .filter(|&fps| fps >= 1)
            .unwrap_or(DEFAULT_PANEL_FPS),
        device_name: hello.device.clone(),
        device_id: hello.id.clone(),
        codecs: hello
            .video_caps
            .as_ref()
            .map(|caps| caps.iter().filter_map(codec_limit).collect()),
    })
}

/// Unknown codecs are ignored (PROTOCOL.md section 6.5).
fn codec_limit(cap: &VideoCap) -> Option<CodecLimit> {
    let codec = if cap.codec.eq_ignore_ascii_case("h264") {
        Codec::H264
    } else if cap.codec.eq_ignore_ascii_case("hevc") {
        Codec::Hevc
    } else {
        return None;
    };
    Some(CodecLimit {
        codec,
        max_width: cap.max_width,
        max_height: cap.max_height,
        max_fps: cap.max_frame_rate,
        max_pixels_per_second: cap.max_pixels_per_second,
    })
}

fn link_stats(v: &serde_json::Value) -> LinkStats {
    let num = |key: &str| v.get(key).and_then(serde_json::Value::as_f64);
    LinkStats {
        e2e_p50_ms: num("e2e50"),
        e2e_p95_ms: num("e2e95"),
        fps: num("fps"),
        mbps: num("mbps"),
        rtt_ms: num("rtt"),
        stalls: num("stalls"),
        transport: v
            .get("transport")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}

fn close_text(reason: CloseReason) -> String {
    match reason {
        CloseReason::Timeout => {
            "the receiver went silent for more than 5 seconds. Check the cable, \
             and that the OpenDisplay app is still open"
                .into()
        }
        CloseReason::Framing(e) => format!(
            "the receiver sent data that is not valid OpenDisplay protocol ({e}). \
             Something else may be listening on that port"
        ),
        CloseReason::Peer(text) => text,
    }
}

impl Receiver for OpenDisplay {
    fn name(&self) -> &'static str {
        "OpenDisplay"
    }

    fn connected(&mut self, now: Now) {
        self.advertised_caps = false;
        self.pen_down = false;
        self.session.handle(od_now(now), SenderEvent::Connected);
    }

    fn received(&mut self, now: Now, bytes: &[u8]) -> Vec<Action> {
        let actions = self.session.handle(od_now(now), SenderEvent::Bytes(bytes));
        self.translate(actions)
    }

    fn tick(&mut self, now: Now) -> Vec<Action> {
        let actions = self.session.handle(od_now(now), SenderEvent::Tick);
        self.translate(actions)
    }

    fn disconnected(&mut self) {
        self.session.handle(
            OdNow::new(std::time::Duration::ZERO, 0.0),
            SenderEvent::Disconnected,
        );
    }

    fn announce_stream(&mut self, plan: &StreamPlan) -> Vec<Action> {
        // R1.6: only a receiver that advertised `videoCaps` expects it.
        if !self.advertised_caps || !self.session.is_connected() {
            return vec![];
        }
        let action = self.session.stream_config(StreamConfig {
            codec: plan.codec.token().into(),
            width: plan.width,
            height: plan.height,
            frames_per_second: plan.fps,
        });
        self.translate(vec![action])
    }

    fn needs_keyframe(&self) -> bool {
        self.session.needs_idr()
    }

    fn video_frame(
        &mut self,
        now: Now,
        annexb: &[u8],
        is_keyframe: bool,
        captured_wall_ms: Option<f64>,
    ) -> Vec<Action> {
        let actions =
            self.session
                .push_encoded_frame(od_now(now), annexb, is_keyframe, captured_wall_ms);
        self.translate(actions)
    }
}
