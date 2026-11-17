//! From receiver input to pointer operations (R1.16, R1.17). Pure: no
//! Wayland here, so the rules can be tested exactly.

use padlink_proto::{Pointer, PointerPhase};

/// One thing to do with the virtual pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    /// Move to this position on the output, in its pixels.
    Motion { x: u32, y: u32 },
    /// Press (`true`) or release the left button.
    Button(bool),
    /// Scroll by this many logical pixels. Positive scrolls content up and
    /// left, as `wl_pointer.axis` defines it.
    Axis { vertical: f64, horizontal: f64 },
}

/// Turns receiver events into pointer operations for one output, and
/// remembers whether the button is held.
#[derive(Debug)]
pub struct Mapper {
    width: u32,
    height: u32,
    scale: f64,
    pressed: bool,
}

impl Mapper {
    /// `width` and `height` are the output's mode in pixels, `scale` its
    /// Hyprland scale.
    pub fn new(width: u32, height: u32, scale: f64) -> Self {
        Mapper {
            width,
            height,
            scale: if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                1.0
            },
            pressed: false,
        }
    }

    /// The motion extents: positions run from 0 to these.
    pub fn extents(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    /// A finger or stylus event. The position is normalised to the picture:
    /// (0, 0) top left, (1, 1) bottom right; anything outside is clamped to
    /// the edge.
    pub fn pointer(&mut self, p: Pointer) -> Vec<Op> {
        let motion = Op::Motion {
            x: (p.x.clamp(0.0, 1.0) * f64::from(self.width)).round() as u32,
            y: (p.y.clamp(0.0, 1.0) * f64::from(self.height)).round() as u32,
        };
        match p.phase {
            // Move first, so the press lands where the finger is.
            PointerPhase::Down if !self.pressed => {
                self.pressed = true;
                vec![motion, Op::Button(true)]
            }
            PointerPhase::Down | PointerPhase::Move | PointerPhase::Hover => vec![motion],
            PointerPhase::Up if self.pressed => {
                self.pressed = false;
                vec![motion, Op::Button(false)]
            }
            PointerPhase::Up => vec![motion],
            // An abandoned gesture: let go where the pointer already is.
            PointerPhase::Cancel => self.release(),
        }
    }

    /// A two-finger scroll. The receiver sends picture pixels with the
    /// natural-scrolling sign (content follows the fingers); a pointer axis
    /// counts logical pixels the other way round.
    pub fn scroll(&self, dx: f64, dy: f64) -> Vec<Op> {
        if !(dx.is_finite() && dy.is_finite()) || (dx == 0.0 && dy == 0.0) {
            return vec![];
        }
        vec![Op::Axis {
            vertical: -dy / self.scale,
            horizontal: -dx / self.scale,
        }]
    }

    /// Let go of the button if it is held (R1.17). Called when the session
    /// ends or the connection drops, so a drag never sticks.
    pub fn release(&mut self) -> Vec<Op> {
        if std::mem::take(&mut self.pressed) {
            vec![Op::Button(false)]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use padlink_proto::PointerSource;

    fn ipad() -> Mapper {
        Mapper::new(2360, 1640, 2.0)
    }

    fn at(phase: PointerPhase, x: f64, y: f64) -> Pointer {
        Pointer {
            phase,
            source: PointerSource::Finger,
            x,
            y,
        }
    }

    use PointerPhase::*;

    #[test]
    fn a_tap_moves_then_presses_then_releases_at_the_finger() {
        let mut m = ipad();
        assert_eq!(
            m.pointer(at(Down, 0.5, 0.5)),
            [Op::Motion { x: 1180, y: 820 }, Op::Button(true)]
        );
        assert!(m.is_pressed());
        assert_eq!(
            m.pointer(at(Up, 0.5, 0.5)),
            [Op::Motion { x: 1180, y: 820 }, Op::Button(false)]
        );
        assert!(!m.is_pressed());
    }

    #[test]
    fn a_drag_keeps_the_button_down_while_moving() {
        let mut m = ipad();
        m.pointer(at(Down, 0.1, 0.1));
        assert_eq!(
            m.pointer(at(Move, 0.2, 0.3)),
            [Op::Motion { x: 472, y: 492 }]
        );
        assert!(m.is_pressed());
        assert_eq!(m.pointer(at(Up, 0.2, 0.3))[1], Op::Button(false));
    }

    #[test]
    fn corners_map_to_the_corners_and_outside_is_clamped() {
        let mut m = ipad();
        assert_eq!(m.pointer(at(Hover, 0.0, 0.0)), [Op::Motion { x: 0, y: 0 }]);
        assert_eq!(
            m.pointer(at(Hover, 1.0, 1.0)),
            [Op::Motion { x: 2360, y: 1640 }]
        );
        assert_eq!(
            m.pointer(at(Hover, -0.3, 7.0)),
            [Op::Motion { x: 0, y: 1640 }]
        );
        assert_eq!(m.extents(), (2360, 1640));
    }

    #[test]
    fn portrait_uses_the_rotated_size() {
        let mut m = Mapper::new(1640, 2360, 2.0);
        assert_eq!(
            m.pointer(at(Hover, 0.5, 0.25)),
            [Op::Motion { x: 820, y: 590 }]
        );
    }

    #[test]
    fn a_hovering_stylus_moves_the_pointer_without_pressing() {
        let mut m = ipad();
        assert_eq!(
            m.pointer(at(Hover, 0.25, 0.25)),
            [Op::Motion { x: 590, y: 410 }]
        );
        assert!(!m.is_pressed());
    }

    #[test]
    fn repeated_downs_and_stray_ups_never_double_press_or_double_release() {
        let mut m = ipad();
        assert_eq!(m.pointer(at(Up, 0.5, 0.5)).len(), 1);
        m.pointer(at(Down, 0.5, 0.5));
        assert_eq!(
            m.pointer(at(Down, 0.6, 0.5)),
            [Op::Motion { x: 1416, y: 820 }]
        );
        m.pointer(at(Up, 0.6, 0.5));
        assert_eq!(m.pointer(at(Up, 0.6, 0.5)).len(), 1);
    }

    #[test]
    fn a_cancelled_gesture_releases_without_moving() {
        let mut m = ipad();
        m.pointer(at(Down, 0.5, 0.5));
        assert_eq!(m.pointer(at(Cancel, 0.9, 0.9)), [Op::Button(false)]);
        assert!(m.pointer(at(Cancel, 0.9, 0.9)).is_empty());
    }

    /// R1.17.
    #[test]
    fn release_lets_go_of_a_held_button_exactly_once() {
        let mut m = ipad();
        assert!(m.release().is_empty());
        m.pointer(at(Down, 0.5, 0.5));
        assert_eq!(m.release(), [Op::Button(false)]);
        assert!(m.release().is_empty());
        assert!(!m.is_pressed());
    }

    #[test]
    fn scroll_flips_the_sign_and_converts_to_logical_pixels() {
        let m = ipad();
        // Fingers move down 40 picture pixels: content follows, which a
        // pointer axis expresses as -20 logical pixels at scale 2.
        assert_eq!(
            m.scroll(0.0, 40.0),
            [Op::Axis {
                vertical: -20.0,
                horizontal: -0.0
            }]
        );
        assert_eq!(
            m.scroll(-10.0, 0.0),
            [Op::Axis {
                vertical: -0.0,
                horizontal: 5.0
            }]
        );
        assert!(m.scroll(0.0, 0.0).is_empty());
        assert!(m.scroll(f64::NAN, 1.0).is_empty());
    }

    #[test]
    fn a_nonsense_scale_is_treated_as_one() {
        let m = Mapper::new(100, 100, 0.0);
        assert_eq!(
            m.scroll(0.0, 8.0),
            [Op::Axis {
                vertical: -8.0,
                horizontal: -0.0
            }]
        );
    }
}
