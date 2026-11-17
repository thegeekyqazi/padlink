//! Choosing the stream for a display (R1.14, R1.15).
//!
//! The stream is always exactly the size of the virtual display: padlink
//! never scales, so a pixel on the output is a pixel on the iPad. What the
//! receiver's limits decide is whether that size is allowed at all, and the
//! frame rate.

use crate::{Codec, CodecLimit, DisplayRequest, StreamPlan};

/// Frame rate a receiver accepts when it says nothing else.
const DEFAULT_DECODER_FPS: u32 = 60;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error(
        "the receiver does not accept {codec} video. It offers: {offered}. \
         Set `codec` in the config to one of those"
    )]
    CodecNotOffered {
        codec: &'static str,
        offered: String,
    },

    #[error(
        "the receiver cannot decode {codec} at {width}x{height}. \
         Its limit is {limit}. Use a smaller mode in the config"
    )]
    TooLarge {
        codec: &'static str,
        width: u32,
        height: u32,
        limit: String,
    },

    #[error(
        "the display size {width}x{height} is not usable: both sides must be even and at least 2"
    )]
    BadSize { width: u32, height: u32 },
}

/// Pick the stream for `display`: `codec` at the display's size, at the
/// highest frame rate that the caller's cap, the panel and the decoder all
/// allow.
pub fn plan_stream(
    display: &DisplayRequest,
    codec: Codec,
    fps_cap: u32,
) -> Result<StreamPlan, PlanError> {
    let (width, height) = (display.width, display.height);
    if width < 2 || height < 2 || width % 2 != 0 || height % 2 != 0 {
        return Err(PlanError::BadSize { width, height });
    }
    let wanted = fps_cap.max(1).min(display.max_fps.max(1));

    let fps = match &display.codecs {
        // No codec list: the legacy contract, H.264 at up to 60.
        None if codec == Codec::H264 => wanted.min(DEFAULT_DECODER_FPS),
        None => {
            return Err(PlanError::CodecNotOffered {
                codec: codec.token(),
                offered: "h264".into(),
            });
        }
        Some(limits) => {
            let mine: Vec<&CodecLimit> = limits.iter().filter(|l| l.codec == codec).collect();
            if mine.is_empty() {
                let mut offered: Vec<&str> = limits.iter().map(|l| l.codec.token()).collect();
                offered.dedup();
                return Err(PlanError::CodecNotOffered {
                    codec: codec.token(),
                    offered: if offered.is_empty() {
                        "nothing padlink knows".into()
                    } else {
                        offered.join(", ")
                    },
                });
            }
            // Entries are alternatives: take the one that allows the highest rate.
            mine.iter()
                .filter_map(|l| fps_within(l, width, height, wanted))
                .max()
                .ok_or_else(|| PlanError::TooLarge {
                    codec: codec.token(),
                    width,
                    height,
                    limit: describe(&mine),
                })?
        }
    };

    Ok(StreamPlan {
        codec,
        width,
        height,
        fps,
    })
}

/// The rate this entry allows at this size, or `None` if the size is outside it.
fn fps_within(limit: &CodecLimit, width: u32, height: u32, wanted: u32) -> Option<u32> {
    if limit.max_width.is_some_and(|m| width > m) || limit.max_height.is_some_and(|m| height > m) {
        return None;
    }
    let mut fps = wanted.min(limit.max_fps.unwrap_or(DEFAULT_DECODER_FPS));
    if let Some(pps) = limit.max_pixels_per_second {
        let per_frame = u64::from(width) * u64::from(height);
        fps = fps.min(u32::try_from(pps / per_frame).unwrap_or(u32::MAX));
    }
    (fps >= 1).then_some(fps)
}

fn describe(limits: &[&CodecLimit]) -> String {
    let dim = |v: Option<u32>| v.map_or("any".to_string(), |v| v.to_string());
    limits
        .iter()
        .map(|l| {
            let mut s = format!("{}x{}", dim(l.max_width), dim(l.max_height));
            if let Some(pps) = l.max_pixels_per_second {
                s.push_str(&format!(" within {pps} pixels per second"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(codec: Codec) -> CodecLimit {
        CodecLimit {
            codec,
            max_width: None,
            max_height: None,
            max_fps: None,
            max_pixels_per_second: None,
        }
    }

    /// The reference iPad's `hello` from docs/spikes.md.
    fn ipad() -> DisplayRequest {
        DisplayRequest {
            width: 2360,
            height: 1640,
            max_fps: 60,
            device_name: Some("iPad".into()),
            device_id: None,
            codecs: Some(vec![
                CodecLimit {
                    max_fps: Some(60),
                    ..limit(Codec::H264)
                },
                CodecLimit {
                    max_width: Some(2360),
                    max_height: Some(2360),
                    max_fps: Some(60),
                    ..limit(Codec::Hevc)
                },
            ]),
        }
    }

    #[test]
    fn the_reference_ipad_gets_its_panel_size_at_60() {
        let plan = plan_stream(&ipad(), Codec::H264, 60).unwrap();
        assert_eq!(
            plan,
            StreamPlan {
                codec: Codec::H264,
                width: 2360,
                height: 1640,
                fps: 60
            }
        );
        assert_eq!(
            plan_stream(&ipad(), Codec::Hevc, 60).unwrap().codec,
            Codec::Hevc
        );
    }

    #[test]
    fn the_stream_is_always_the_display_size() {
        let mut d = ipad();
        (d.width, d.height) = (1640, 2360);
        let plan = plan_stream(&d, Codec::H264, 60).unwrap();
        assert_eq!((plan.width, plan.height), (1640, 2360));
    }

    #[test]
    fn frame_rate_is_the_lowest_of_config_panel_and_decoder() {
        assert_eq!(plan_stream(&ipad(), Codec::H264, 30).unwrap().fps, 30);
        assert_eq!(plan_stream(&ipad(), Codec::H264, 240).unwrap().fps, 60);

        let mut slow_panel = ipad();
        slow_panel.max_fps = 50;
        assert_eq!(plan_stream(&slow_panel, Codec::H264, 60).unwrap().fps, 50);

        let mut slow_decoder = ipad();
        slow_decoder.codecs = Some(vec![CodecLimit {
            max_fps: Some(24),
            ..limit(Codec::H264)
        }]);
        assert_eq!(plan_stream(&slow_decoder, Codec::H264, 60).unwrap().fps, 24);
    }

    #[test]
    fn a_fast_panel_does_not_raise_the_rate_past_what_the_decoder_promised() {
        let mut d = ipad();
        d.max_fps = 120;
        d.codecs = Some(vec![limit(Codec::H264)]);
        assert_eq!(plan_stream(&d, Codec::H264, 120).unwrap().fps, 60);
        d.codecs = Some(vec![CodecLimit {
            max_fps: Some(120),
            ..limit(Codec::H264)
        }]);
        assert_eq!(plan_stream(&d, Codec::H264, 120).unwrap().fps, 120);
    }

    #[test]
    fn pixel_throughput_lowers_the_rate() {
        let mut d = ipad();
        // Enough for 40 frames of 2360x1640 a second.
        d.codecs = Some(vec![CodecLimit {
            max_pixels_per_second: Some(2360 * 1640 * 40),
            ..limit(Codec::H264)
        }]);
        assert_eq!(plan_stream(&d, Codec::H264, 60).unwrap().fps, 40);
    }

    #[test]
    fn entries_for_one_codec_are_alternatives_and_the_best_fitting_one_wins() {
        let mut d = ipad();
        d.codecs = Some(vec![
            // Too small for the display.
            CodecLimit {
                max_width: Some(1920),
                max_height: Some(1080),
                max_fps: Some(120),
                ..limit(Codec::H264)
            },
            CodecLimit {
                max_width: Some(2732),
                max_height: Some(2732),
                max_fps: Some(30),
                ..limit(Codec::H264)
            },
            CodecLimit {
                max_width: Some(2732),
                max_height: Some(2732),
                max_fps: Some(48),
                ..limit(Codec::H264)
            },
        ]);
        assert_eq!(plan_stream(&d, Codec::H264, 60).unwrap().fps, 48);
    }

    #[test]
    fn a_display_larger_than_the_decoder_is_an_error_that_names_the_limit() {
        let mut d = ipad();
        (d.width, d.height) = (2360, 1640);
        d.codecs = Some(vec![CodecLimit {
            max_width: Some(1920),
            max_height: Some(1080),
            ..limit(Codec::H264)
        }]);
        let err = plan_stream(&d, Codec::H264, 60).unwrap_err();
        assert!(matches!(err, PlanError::TooLarge { .. }));
        let text = err.to_string();
        assert!(
            text.contains("2360x1640") && text.contains("1920x1080"),
            "{text}"
        );
    }

    #[test]
    fn asking_for_a_codec_the_receiver_lacks_lists_what_it_has() {
        let mut d = ipad();
        d.codecs = Some(vec![limit(Codec::H264)]);
        let err = plan_stream(&d, Codec::Hevc, 60).unwrap_err();
        assert_eq!(
            err,
            PlanError::CodecNotOffered {
                codec: "hevc",
                offered: "h264".into()
            }
        );
    }

    #[test]
    fn no_codec_list_means_h264_at_up_to_60() {
        let mut d = ipad();
        d.codecs = None;
        d.max_fps = 120;
        assert_eq!(plan_stream(&d, Codec::H264, 120).unwrap().fps, 60);
        assert!(plan_stream(&d, Codec::Hevc, 60).is_err());
    }

    #[test]
    fn odd_or_tiny_sizes_are_rejected() {
        let mut d = ipad();
        d.width = 2361;
        assert!(matches!(
            plan_stream(&d, Codec::H264, 60),
            Err(PlanError::BadSize { .. })
        ));
        d.width = 0;
        assert!(plan_stream(&d, Codec::H264, 60).is_err());
    }
}
