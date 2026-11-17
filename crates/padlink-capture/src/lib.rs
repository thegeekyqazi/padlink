//! Capture of one output, for padlink.
//!
//! Frames come from the compositor's capture protocol and only when the
//! output has new damage: a static screen produces nothing (SPEC.md
//! section 4). The first frame of a capture always arrives.
//!
//! Only Wayland capture protocols are used. KMS capture cannot see a
//! headless output, and the desktop portal shows a picker (section 8).
//!
//! This crate currently implements `wlr-screencopy` into shared memory, the
//! path that is safe on Hyprland below 0.57.

mod pool;
mod wlr;

use std::path::PathBuf;
use std::time::Instant;

pub use pool::{GpuBuffer, GpuLease, Lease};
pub use wlr::{capture, protocols};

/// Where a frame's pixels are.
pub enum Pixels {
    /// In shared memory: 32-bit, in the byte order blue, green, red, unused
    /// (`XRGB8888` on little-endian machines).
    Cpu(Lease),
    /// Still on the GPU, for an encoder that can take them from there.
    Gpu(GpuLease),
}

/// One captured frame.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Length of a row in bytes; may be longer than `width * 4`.
    pub stride: u32,
    pub pixels: Pixels,
    /// When the compositor finished the copy, for latency figures.
    pub captured_at: Instant,
    /// The same moment as wall-clock milliseconds since the Unix epoch, for
    /// the receiver's end-to-end measurement.
    pub captured_wall_ms: f64,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The Wayland socket of the compositor, such as
    /// `/run/user/1000/wayland-1`.
    pub wayland_socket: PathBuf,
    /// The output to capture, by its name: `padlink-ba601e`.
    pub output: String,
    /// Draw the pointer into the picture.
    pub cursor: bool,
    /// Capture into GPU buffers allocated on this render node, so the
    /// encoder can take frames without a copy (R1.12). `None` captures into
    /// shared memory, which every encoder can read. If the compositor or
    /// the GPU refuses, capture falls back to shared memory by itself.
    pub gpu_buffers: Option<PathBuf>,
}

/// Which protocol and buffer type a capture uses, for the log and `status`
/// (R1.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    WlrShm,
    WlrDmabuf,
}

impl Path {
    pub fn label(self) -> &'static str {
        match self {
            Path::WlrShm => "wlr-screencopy, shared memory",
            Path::WlrDmabuf => "wlr-screencopy, zero-copy GPU buffers",
        }
    }
}

/// Why a capture ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The stop flag was set. Every capture object has been destroyed and
    /// the compositor has acknowledged it, so the output may now be removed
    /// (R1.18).
    Stopped,
    /// The output went away underneath the capture.
    OutputRemoved,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot connect to the Wayland compositor at {path}: {detail}")]
    Connect { path: PathBuf, detail: String },

    #[error(
        "the compositor does not offer `{0}`, which padlink needs to capture the screen. \
         It is provided by Hyprland and other wlroots-based compositors"
    )]
    MissingProtocol(&'static str),

    #[error("there is no output named {wanted}. Outputs: {known}")]
    NoOutput { wanted: String, known: String },

    #[error(
        "the compositor offers the picture in pixel format {0}, which padlink cannot \
         read yet. Please report this with your GPU and compositor version"
    )]
    UnsupportedFormat(String),

    #[error("the compositor refused every capture request for {0}")]
    AlwaysFails(String),

    #[error("lost the connection to the compositor: {0}")]
    Wayland(String),

    #[error("could not allocate a capture buffer: {0}")]
    Buffer(#[from] std::io::Error),
}

/// Whether a frame is entirely black, judged from a grid of sample pixels.
///
/// With `ecosystem:enforce_permissions` on and no permission rule, Hyprland
/// answers a capture with black frames instead of an error (section 8), so
/// an all-black first frame is worth a warning.
pub fn looks_black(pixels: &[u8], width: u32, height: u32, stride: u32) -> bool {
    let (w, h, stride) = (width as usize, height as usize, stride as usize);
    if w == 0 || h == 0 {
        return true;
    }
    let step_y = (h / 48).max(1);
    let step_x = (w / 64).max(1);
    (0..h).step_by(step_y).all(|y| {
        (0..w).step_by(step_x).all(|x| {
            let i = y * stride + x * 4;
            pixels.get(i..i + 3).is_none_or(|bgr| bgr == [0, 0, 0])
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_black_frame_is_recognised_and_one_lit_pixel_on_the_grid_is_not() {
        let (w, h, stride) = (640u32, 480u32, 640 * 4 + 16);
        let mut pixels = vec![0u8; (stride * h) as usize];
        assert!(looks_black(&pixels, w, h, stride));
        // The unused fourth byte does not count.
        pixels.iter_mut().skip(3).step_by(4).for_each(|a| *a = 0xff);
        assert!(looks_black(&pixels, w, h, stride));
        // Sample points are every 10th column and row here.
        let lit = (240 * stride + 320 * 4) as usize;
        pixels[lit + 1] = 9;
        assert!(!looks_black(&pixels, w, h, stride));
    }

    #[test]
    fn an_empty_or_short_frame_does_not_panic() {
        assert!(looks_black(&[], 0, 0, 0));
        assert!(looks_black(&[0; 16], 640, 480, 2560));
    }
}
