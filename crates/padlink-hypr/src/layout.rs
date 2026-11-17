//! Where the virtual display goes and which scales are allowed (R1.8).
//!
//! Pure arithmetic on the monitor layout, in Hyprland's logical
//! coordinates: a monitor occupies `pixels / scale` units.

/// A monitor's area in the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Where to put the virtual display relative to the existing monitors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Right of the rightmost monitor, top edges aligned. The default.
    Right,
    /// Left of the leftmost monitor, top edges aligned.
    Left,
    /// Above the topmost monitor, left edges aligned.
    Above,
    /// Below the lowest monitor, left edges aligned.
    Below,
    /// An exact layout position.
    At(i32, i32),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ScaleError {
    #[error("scale {0} is not a positive number")]
    NotPositive(f64),
    #[error(
        "scale {scale} does not divide {width}x{height} into whole pixels \
         (it would give {logical_w}x{logical_h}). Scales that work: {valid}"
    )]
    Uneven {
        scale: f64,
        width: u32,
        height: u32,
        logical_w: String,
        logical_h: String,
        valid: String,
    },
}

/// How far a division may be from a whole number and still count as whole.
/// Covers scales like 1.6 that have no exact binary form.
const WHOLE: f64 = 1e-6;

fn whole(v: f64) -> Option<u32> {
    let r = v.round();
    ((v - r).abs() < WHOLE && r >= 1.0).then_some(r as u32)
}

/// The logical size of a `width` x `height` mode at `scale`, or an error
/// that lists scales that would work. Hyprland silently rounds a scale that
/// leaves a fraction of a pixel, so it is rejected here first.
pub fn logical_size(width: u32, height: u32, scale: f64) -> Result<(u32, u32), ScaleError> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(ScaleError::NotPositive(scale));
    }
    let (lw, lh) = (f64::from(width) / scale, f64::from(height) / scale);
    match (whole(lw), whole(lh)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(ScaleError::Uneven {
            scale,
            width,
            height,
            logical_w: trim(lw),
            logical_h: trim(lh),
            valid: valid_scales(width, height)
                .iter()
                .map(|s| trim(*s))
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// Scales between 1 and 4, in steps of 0.05, that divide the mode evenly.
pub fn valid_scales(width: u32, height: u32) -> Vec<f64> {
    (20..=80)
        .map(|i| f64::from(i) / 20.0)
        .filter(|s| whole(f64::from(width) / s).is_some() && whole(f64::from(height) / s).is_some())
        .collect()
}

/// A number without trailing zeros: 2, 1.25, 1573.33.
fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The layout position for a display of logical `size`, given the other
/// monitors. With no other monitor it goes to the origin.
pub fn place(placement: Placement, others: &[Rect], size: (u32, u32)) -> (i32, i32) {
    let (w, h) = (size.0 as i32, size.1 as i32);
    match placement {
        Placement::At(x, y) => (x, y),
        Placement::Right => others
            .iter()
            .max_by_key(|r| r.x + r.w)
            .map_or((0, 0), |r| (r.x + r.w, r.y)),
        Placement::Left => others
            .iter()
            .min_by_key(|r| r.x)
            .map_or((0, 0), |r| (r.x - w, r.y)),
        Placement::Above => others
            .iter()
            .min_by_key(|r| r.y)
            .map_or((0, 0), |r| (r.x, r.y - h)),
        Placement::Below => others
            .iter()
            .max_by_key(|r| r.y + r.h)
            .map_or((0, 0), |r| (r.x, r.y + r.h)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference laptop panel: 1920x1080 at scale 1, at 957x0.
    const LAPTOP: Rect = Rect {
        x: 957,
        y: 0,
        w: 1920,
        h: 1080,
    };
    /// The iPad output at scale 2.
    const IPAD: (u32, u32) = (1180, 820);

    #[test]
    fn default_scale_two_gives_1180_by_820() {
        assert_eq!(logical_size(2360, 1640, 2.0), Ok((1180, 820)));
        assert_eq!(logical_size(2360, 1640, 1.0), Ok((2360, 1640)));
    }

    #[test]
    fn fractional_scales_that_divide_evenly_are_accepted() {
        assert_eq!(logical_size(2360, 1640, 1.25), Ok((1888, 1312)));
        assert_eq!(logical_size(2360, 1640, 1.6), Ok((1475, 1025)));
        assert_eq!(logical_size(2360, 1640, 2.5), Ok((944, 656)));
    }

    #[test]
    fn an_uneven_scale_is_rejected_and_the_message_lists_ones_that_work() {
        let err = logical_size(2360, 1640, 1.5).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("scale 1.5 does not divide 2360x1640"),
            "{text}"
        );
        assert!(text.contains("1573.33x1093.33"), "{text}");
        assert!(
            text.contains("Scales that work: 1, 1.25, 1.6, 2, 2.5, 4"),
            "{text}"
        );
    }

    #[test]
    fn scale_three_does_not_fit_the_ipad() {
        assert!(logical_size(2360, 1640, 3.0).is_err());
    }

    #[test]
    fn zero_negative_and_nan_scales_are_rejected() {
        for bad in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                logical_size(2360, 1640, bad),
                Err(ScaleError::NotPositive(_))
            ));
        }
    }

    #[test]
    fn valid_scales_all_pass_validation() {
        for s in valid_scales(2360, 1640) {
            assert!(logical_size(2360, 1640, s).is_ok(), "{s}");
        }
        // Portrait has the same set.
        assert_eq!(valid_scales(1640, 2360), valid_scales(2360, 1640));
    }

    #[test]
    fn right_of_the_reference_laptop_is_2877_by_0() {
        assert_eq!(place(Placement::Right, &[LAPTOP], IPAD), (2877, 0));
    }

    #[test]
    fn each_side_of_a_single_monitor() {
        assert_eq!(place(Placement::Left, &[LAPTOP], IPAD), (957 - 1180, 0));
        assert_eq!(place(Placement::Above, &[LAPTOP], IPAD), (957, -820));
        assert_eq!(place(Placement::Below, &[LAPTOP], IPAD), (957, 1080));
        assert_eq!(place(Placement::At(-40, 12), &[LAPTOP], IPAD), (-40, 12));
    }

    #[test]
    fn with_several_monitors_the_outermost_one_is_the_anchor() {
        let external = Rect {
            x: 2877,
            y: -200,
            w: 2560,
            h: 1440,
        };
        let both = [LAPTOP, external];
        // Right of the external monitor, aligned to its top.
        assert_eq!(place(Placement::Right, &both, IPAD), (5437, -200));
        // Left of the laptop, aligned to its top.
        assert_eq!(place(Placement::Left, &both, IPAD), (-223, 0));
        // Above the monitor that reaches highest, below the one that reaches lowest.
        assert_eq!(place(Placement::Above, &both, IPAD), (2877, -1020));
        assert_eq!(place(Placement::Below, &both, IPAD), (2877, 1240));
    }

    #[test]
    fn with_no_other_monitor_the_display_goes_to_the_origin() {
        for p in [
            Placement::Right,
            Placement::Left,
            Placement::Above,
            Placement::Below,
        ] {
            assert_eq!(place(p, &[], IPAD), (0, 0));
        }
    }
}
