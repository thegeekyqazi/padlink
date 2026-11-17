//! Hyprland for padlink.

pub mod instance;
pub mod layout;

pub use instance::{FindError, Instance};
pub use layout::{Placement, Rect, ScaleError, logical_size, place, valid_scales};
