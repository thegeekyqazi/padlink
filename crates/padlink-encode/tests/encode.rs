//! Real encodes on this machine's FFmpeg and GPU.
//!
//! A backend that cannot open here (no VA-API driver, no libx264) is
//! reported and skipped rather than failed, so the suite runs anywhere; on
//! the reference laptop all three run.

use opendisplay_proto::video::{NalType, SpsInfo, nal_units};
use padlink_encode::{Backend, Encoder, Error, Kind, Packet, Settings};

/// The iPad panel, landscape.
const IPAD: (u32, u32) = (2360, 1640);

fn settings(backend: Backend, (width, height): (u32, u32)) -> Settings {
    Settings {
        width,
        height,
        fps: 60,
        bitrate_bps: 25_000_000,
        backend,
        render_node: "/dev/dri/renderD128".into(),
        codec_options: Vec::new(),
    }
}

/// Open one specific backend, or explain why this machine cannot.
fn open(backend: Backend, size: (u32, u32)) -> Option<Encoder> {
    match Encoder::open(&settings(backend, size)) {
        Ok(opened) => Some(opened.encoder),
        Err(e) => {
            eprintln!("SKIPPED {backend:?}: {e}");
            None
        }
    }
}

const ALL: [Backend; 3] = [Backend::Vaapi, Backend::X264, Backend::OpenH264];

/// A frame that differs from the previous one, with a row length longer
/// than the picture, as a Wayland buffer may have.
struct Pattern {
    width: usize,
    height: usize,
    stride: usize,
    n: u8,
    data: Vec<u8>,
}

impl Pattern {
    fn new((width, height): (u32, u32)) -> Self {
        let width = width as usize;
        Pattern {
            width,
            height: height as usize,
            stride: width * 4 + 64,
            n: 0,
            data: Vec::new(),
        }
    }

    /// A gradient background with a band that moves every frame, like a
    /// window being dragged: most of the picture is unchanged.
    fn next(&mut self) -> &[u8] {
        if self.data.is_empty() {
            self.data = vec![0u8; self.stride * self.height];
            for y in 0..self.height {
                let row = &mut self.data[y * self.stride..y * self.stride + self.width * 4];
                for (x, px) in row.chunks_exact_mut(4).enumerate() {
                    px[0] = (x / 8) as u8;
                    px[1] = (y / 8) as u8;
                    px[2] = ((x + y) / 16) as u8;
                }
            }
        }
        self.n = self.n.wrapping_add(7);
        let top = (self.n as usize * 5) % (self.height - 32);
        for y in top..top + 32 {
            let row = &mut self.data[y * self.stride..y * self.stride + self.width * 4];
            row.iter_mut().for_each(|b| *b = b.wrapping_add(self.n));
        }
        &self.data
    }
}

fn nal_types(p: &Packet) -> Vec<u8> {
    nal_units(&p.data).map(|n| n[0] & 0x1f).collect()
}

fn sps_of(p: &Packet) -> SpsInfo {
    let sps = nal_units(&p.data)
        .find(|n| NalType::of(n) == Some(NalType::Sps))
        .expect("keyframe has an SPS");
    SpsInfo::parse(sps).expect("SPS parses")
}

/// Encode `count` changing frames; every one must produce a packet.
fn run(enc: &mut Encoder, size: (u32, u32), count: usize) -> Vec<Packet> {
    let mut pattern = Pattern::new(size);
    (0..count)
        .map(|i| {
            let stride = pattern.stride;
            enc.encode_bgrx(pattern.next(), stride, false)
                .unwrap()
                .unwrap_or_else(|| panic!("frame {i} was held back by the encoder"))
        })
        .collect()
}

/// R1.15: the size signalled in the SPS of the first keyframe equals the
/// output size exactly. 1640 is not a multiple of 16, so the encoder pads to
/// 1648 and must signal the crop.
#[test]
fn the_stream_signals_exactly_the_output_size() {
    for backend in ALL {
        for size in [IPAD, (IPAD.1, IPAD.0)] {
            let Some(mut enc) = open(backend, size) else {
                continue;
            };
            let first = &run(&mut enc, size, 1)[0];
            let sps = sps_of(first);
            assert_eq!(
                (sps.width, sps.height),
                size,
                "{backend:?} at {size:?} signals the wrong size"
            );
        }
    }
}

#[test]
fn the_first_packet_is_a_keyframe_with_parameter_sets_first() {
    for backend in ALL {
        let Some(mut enc) = open(backend, IPAD) else {
            continue;
        };
        let first = &run(&mut enc, IPAD, 1)[0];
        assert!(first.keyframe, "{backend:?}");
        let types = nal_types(first);
        let (sps, pps, idr) = (
            types.iter().position(|&t| t == 7),
            types.iter().position(|&t| t == 8),
            types.iter().position(|&t| t == 5),
        );
        assert!(
            sps < pps && pps < idr && sps.is_some(),
            "{backend:?}: {types:?}"
        );
    }
}

/// SPEC.md section 4: keyframes only on connect, on request and on a size
/// change. Never periodically.
#[test]
fn there_are_no_periodic_keyframes() {
    for backend in ALL {
        let Some(mut enc) = open(backend, IPAD) else {
            continue;
        };
        // Five seconds of frames, well past any default keyframe interval.
        let packets = run(&mut enc, IPAD, 300);
        let keyframes = packets.iter().filter(|p| p.keyframe).count();
        assert_eq!(keyframes, 1, "{backend:?}");
        assert!(
            packets[1..].iter().all(|p| nal_types(p).contains(&1)),
            "{backend:?}"
        );
    }
}

#[test]
fn a_requested_keyframe_is_an_idr_with_parameter_sets() {
    for backend in ALL {
        let Some(mut enc) = open(backend, IPAD) else {
            continue;
        };
        run(&mut enc, IPAD, 5);
        let mut pattern = Pattern::new(IPAD);
        let stride = pattern.stride;
        let forced = enc
            .encode_bgrx(pattern.next(), stride, true)
            .unwrap()
            .unwrap();
        assert!(forced.keyframe, "{backend:?}");
        let types = nal_types(&forced);
        assert!(
            types.contains(&7) && types.contains(&8),
            "{backend:?}: {types:?}"
        );
        assert_eq!(sps_of(&forced).width, IPAD.0);
        // And the stream carries on with ordinary frames.
        assert!(!run(&mut enc, IPAD, 1)[0].keyframe, "{backend:?}");
    }
}

/// SPEC.md section 8: a static screen produces no frames, so the last one
/// is re-encoded as a keyframe when a receiver needs one.
#[test]
fn the_last_frame_can_be_repeated_as_a_keyframe() {
    for backend in ALL {
        let Some(mut enc) = open(backend, IPAD) else {
            continue;
        };
        assert!(
            matches!(enc.repeat_as_keyframe(), Err(Error::NothingToRepeat)),
            "{backend:?}"
        );
        run(&mut enc, IPAD, 3);
        let again = enc.repeat_as_keyframe().unwrap().unwrap();
        assert!(again.keyframe, "{backend:?}");
        assert!(nal_types(&again).contains(&7), "{backend:?}");
        // Twice in a row works too.
        assert!(enc.repeat_as_keyframe().unwrap().unwrap().keyframe);
    }
}

/// PROTOCOL.md section 5.1: 4-byte start codes only.
#[test]
fn every_start_code_is_four_bytes() {
    for backend in ALL {
        let Some(mut enc) = open(backend, IPAD) else {
            continue;
        };
        for p in run(&mut enc, IPAD, 4) {
            assert_eq!(&p.data[..4], &[0, 0, 0, 1], "{backend:?}");
            // A 3-byte code would show up as 00 00 01 not preceded by 00.
            let stray = p.data.windows(4).any(|w| w[0] != 0 && w[1..] == [0, 0, 1]);
            assert!(!stray, "{backend:?} left a 3-byte start code");
        }
    }
}

#[test]
fn a_buffer_that_is_too_small_is_refused_not_read_past() {
    for backend in ALL {
        let Some(mut enc) = open(backend, IPAD) else {
            continue;
        };
        let stride = IPAD.0 as usize * 4;
        let short = vec![0u8; stride * (IPAD.1 as usize - 1)];
        assert!(matches!(
            enc.encode_bgrx(&short, stride, false),
            Err(Error::ShortFrame { .. })
        ));
        // A row length shorter than the picture is refused as well.
        let full = vec![0u8; stride * IPAD.1 as usize];
        assert!(enc.encode_bgrx(&full, stride - 4, false).is_err());
        // The encoder still works afterwards.
        assert!(enc.encode_bgrx(&full, stride, false).unwrap().is_some());
    }
}

/// R1.13: automatic selection prefers the GPU and reports what it skipped.
#[test]
fn auto_prefers_the_gpu_and_says_why_it_skipped_anything() {
    let opened = Encoder::open(&settings(Backend::Auto, IPAD)).expect("some encoder opens");
    let kind = opened.encoder.kind();
    eprintln!("auto chose: {}", opened.encoder.describe());
    match kind {
        Kind::Vaapi => assert!(opened.skipped.is_empty()),
        _ => {
            assert_eq!(opened.skipped[0].0, Kind::Vaapi);
            assert!(!opened.skipped[0].1.is_empty());
        }
    }
    assert!(
        opened
            .encoder
            .describe()
            .contains("2360x1640 at 60 fps, 25.0 Mbit/s")
    );
}

#[test]
fn a_gpu_that_is_not_there_falls_back_to_software_with_a_reason() {
    let mut s = settings(Backend::Auto, IPAD);
    s.render_node = "/dev/dri/renderD999".into();
    let opened = Encoder::open(&s).expect("software encoder opens");
    assert_ne!(opened.encoder.kind(), Kind::Vaapi);
    let (kind, why) = &opened.skipped[0];
    assert_eq!(*kind, Kind::Vaapi);
    assert!(why.contains("renderD999"), "{why}");

    // Asking for the GPU specifically is an error instead.
    s.backend = Backend::Vaapi;
    assert!(Encoder::open(&s).is_err());
}
