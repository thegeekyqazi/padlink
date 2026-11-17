//! What padlink needs from a receiver, independent of the wire format.
//!
//! The daemon talks to a [`Receiver`]: it feeds in bytes and time, and gets
//! back [`Action`]s (bytes to write, a display to create, pointer input, a
//! request for a keyframe). Nothing here owns a socket or a clock, so the
//! whole conversation can be tested without a device.
//!
//! [`OpenDisplay`] is the only implementation: the sender side of the
//! OpenDisplay protocol, version 3, built on the upstream `opendisplay-proto`
//! and `opendisplay-session` crates (GPL-3.0). A different receiver app would
//! be a second implementation of the same trait.

mod opendisplay;
mod plan;

use std::time::Duration;

pub use opendisplay::OpenDisplay;
pub use plan::{PlanError, plan_stream};

/// TCP port the OpenDisplay receiver listens on.
pub const OPENDISPLAY_PORT: u16 = 9000;

/// The caller's clocks at the moment of a call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Now {
    /// Monotonic time since any fixed origin, such as process start.
    pub mono: Duration,
    /// Wall-clock milliseconds since the Unix epoch. The receiver uses it to
    /// work out end-to-end latency.
    pub wall_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    /// The lowercase token used on the wire and in config files.
    pub fn token(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }
}

/// One set of limits the receiver's decoder accepts for a codec. All limits
/// in one entry apply together; several entries for the same codec are
/// alternatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecLimit {
    pub codec: Codec,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_fps: Option<u32>,
    pub max_pixels_per_second: Option<u64>,
}

/// The display the receiver wants, in its current orientation. A receiver
/// sends a new one when it rotates.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRequest {
    /// Panel size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// The panel's refresh rate; the stream never needs to exceed it.
    pub max_fps: u32,
    pub device_name: Option<String>,
    /// Stable per-install identity of the receiver.
    pub device_id: Option<String>,
    /// `None` when the receiver advertised no codec list, which means plain
    /// H.264 at up to 60 fps and no stream announcement.
    pub codecs: Option<Vec<CodecLimit>>,
}

/// The stream the daemon decided to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamPlan {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    /// Contact began: press.
    Down,
    /// Moved while in contact: drag.
    Move,
    /// Contact ended: release.
    Up,
    /// The gesture was abandoned: release without treating it as a click.
    Cancel,
    /// A stylus moved above the surface without touching it.
    Hover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerSource {
    Finger,
    Stylus,
}

/// A finger or stylus position, normalised to the stream: (0, 0) is the top
/// left corner and (1, 1) the bottom right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub phase: PointerPhase,
    pub source: PointerSource,
    pub x: f64,
    pub y: f64,
}

/// Figures the receiver measured itself. Missing ones are `None`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LinkStats {
    /// Capture to display, median and 95th percentile, in milliseconds.
    pub e2e_p50_ms: Option<f64>,
    pub e2e_p95_ms: Option<f64>,
    pub fps: Option<f64>,
    pub mbps: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub stalls: Option<f64>,
    /// The receiver's own name for the link, such as "USB".
    pub transport: Option<String>,
}

/// What the daemon must do after an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Write these bytes to the receiver, whole and in order.
    Send(Vec<u8>),
    /// Create the virtual display, or rebuild it if one exists (rotation).
    /// The stream restarts with a keyframe.
    Display(DisplayRequest),
    /// The next encoded frame must be a keyframe with its parameter sets.
    Keyframe,
    Pointer(Pointer),
    /// Scroll deltas in stream pixels, natural-scrolling sign (content
    /// follows the fingers).
    Scroll {
        dx: f64,
        dy: f64,
    },
    Stats(LinkStats),
    /// The receiver locked or went to the background and will be back:
    /// tear the display down and keep dialing.
    Sleeping,
    /// The receiver app quit: tear down and wait for it to reopen.
    Closing,
    /// The connection is unusable. The text says why.
    Close(String),
    /// Worth one log line; nothing to do.
    Warn(String),
}

/// One receiver connection. Calls must be made from one thread, in the
/// order things happen.
pub trait Receiver {
    /// A short name for logs and `status`.
    fn name(&self) -> &'static str;

    /// The transport connected. Resets all per-connection state.
    fn connected(&mut self, now: Now);

    /// Bytes arrived from the receiver, in any chunking.
    fn received(&mut self, now: Now, bytes: &[u8]) -> Vec<Action>;

    /// Call a few times a second: drives the liveness beat and timeout.
    fn tick(&mut self, now: Now) -> Vec<Action>;

    /// The transport closed.
    fn disconnected(&mut self);

    /// Tell the receiver which stream follows. Must be called before the
    /// first frame and after every change of size, rate or codec.
    fn announce_stream(&mut self, plan: &StreamPlan) -> Vec<Action>;

    /// Whether the next frame handed to [`Receiver::video_frame`] must be a
    /// keyframe. Other frames are dropped until one arrives.
    fn needs_keyframe(&self) -> bool;

    /// An encoded frame is ready. `annexb` uses 4-byte start codes and, for a
    /// keyframe, starts with the parameter sets. `captured_wall_ms` is when
    /// the frame was captured, on the same clock as [`Now::wall_ms`].
    fn video_frame(
        &mut self,
        now: Now,
        annexb: &[u8],
        is_keyframe: bool,
        captured_wall_ms: Option<f64>,
    ) -> Vec<Action>;
}
