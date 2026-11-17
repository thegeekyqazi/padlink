//! Hyprland for padlink: find the running instance, create and configure
//! the headless output, read the layout back, and remove the output again.
//!
//! Nothing here edits Hyprland's config files (R1.10); every change is a
//! runtime request over the IPC socket.

pub mod instance;
pub mod ipc;
pub mod layout;

pub use instance::{FindError, Instance};
pub use ipc::{
    Dialect, Error, Focus, Hyprland, Monitor, OUTPUT_PREFIX, OutputSpec, Version, output_name,
};
pub use layout::{Placement, Rect, ScaleError, logical_size, place, valid_scales};

/// From this version on `ext-image-copy-capture` is safe when a display
/// disappears; below it only `wlr-screencopy` is (SPEC.md section 8).
pub const EXT_CAPTURE_SAFE_FROM: Version = Version {
    major: 0,
    minor: 57,
    patch: 0,
};
