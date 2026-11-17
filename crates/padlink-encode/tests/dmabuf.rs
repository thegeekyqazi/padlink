//! Zero-copy input on this machine's GPU (R1.12): a buffer allocated with
//! GBM, as capture will allocate them, encoded without touching the CPU.
//!
//! Skipped with a message where there is no GPU encoder.

use std::os::fd::AsFd;

use gbm::{BufferObjectFlags, Format};
use opendisplay_proto::video::{NalType, SpsInfo, nal_units};
use padlink_encode::{Backend, Dmabuf, Encoder, Error, Settings};

const NODE: &str = "/dev/dri/renderD128";
const SIZE: (u32, u32) = (2360, 1640);

fn settings(backend: Backend) -> Settings {
    Settings {
        width: SIZE.0,
        height: SIZE.1,
        fps: 60,
        bitrate_bps: 25_000_000,
        backend,
        render_node: NODE.into(),
        codec_options: Vec::new(),
    }
}

fn gpu_encoder() -> Option<Encoder> {
    match Encoder::open(&settings(Backend::Vaapi)) {
        Ok(opened) => Some(opened.encoder),
        Err(e) => {
            eprintln!("SKIPPED: {e}");
            None
        }
    }
}

struct Buffer {
    fd: std::os::fd::OwnedFd,
    modifier: u64,
    offset: u32,
    stride: u32,
    _bo: gbm::BufferObject<()>,
}

impl Buffer {
    fn new(device: &gbm::Device<std::fs::File>) -> Buffer {
        let bo = device
            .create_buffer_object::<()>(
                SIZE.0,
                SIZE.1,
                Format::Xrgb8888,
                BufferObjectFlags::RENDERING,
            )
            .expect("allocate a GPU buffer");
        Buffer {
            fd: bo.fd().expect("export the buffer"),
            modifier: bo.modifier().into(),
            offset: bo.offset(0),
            stride: bo.stride(),
            _bo: bo,
        }
    }

    fn as_dmabuf(&self) -> Dmabuf<'_> {
        Dmabuf {
            fd: self.fd.as_fd(),
            fourcc: Format::Xrgb8888 as u32,
            modifier: self.modifier,
            offset: self.offset,
            stride: self.stride,
        }
    }
}

fn gbm_device() -> gbm::Device<std::fs::File> {
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .open(NODE)
        .unwrap();
    gbm::Device::new(file).expect("open GBM on the render node")
}

fn sps(data: &[u8]) -> SpsInfo {
    let nal = nal_units(data)
        .find(|n| NalType::of(n) == Some(NalType::Sps))
        .expect("keyframe has an SPS");
    SpsInfo::parse(nal).unwrap()
}

#[test]
fn a_gpu_buffer_is_encoded_at_the_right_size_without_a_copy() {
    let Some(mut encoder) = gpu_encoder() else {
        return;
    };
    assert!(encoder.takes_gpu_buffers());
    let device = gbm_device();
    // Two buffers in turn, as a capture with a small pool hands them over.
    let buffers = [Buffer::new(&device), Buffer::new(&device)];
    eprintln!(
        "buffer: stride {}, offset {}, modifier {:#x}",
        buffers[0].stride, buffers[0].offset, buffers[0].modifier
    );

    let first = encoder
        .encode_dmabuf(&buffers[0].as_dmabuf(), false)
        .unwrap()
        .expect("a packet for the first frame");
    assert!(first.keyframe);
    let info = sps(&first.data);
    assert_eq!((info.width, info.height), SIZE);

    for i in 1..30 {
        let packet = encoder
            .encode_dmabuf(&buffers[i % 2].as_dmabuf(), false)
            .unwrap()
            .unwrap_or_else(|| panic!("frame {i} was held back"));
        assert!(!packet.keyframe, "frame {i}");
    }
    let t = encoder.timings();
    eprintln!(
        "zero-copy: prepare {:.2} ms, codec {:.2} ms",
        t.prepare_ms, t.encode_ms
    );

    // A requested keyframe, and a repeat of the last picture.
    assert!(
        encoder
            .encode_dmabuf(&buffers[0].as_dmabuf(), true)
            .unwrap()
            .unwrap()
            .keyframe
    );
    assert!(encoder.repeat_as_keyframe().unwrap().unwrap().keyframe);
}

/// The two input paths can alternate on one encoder, which is what lets
/// capture fall back to shared memory mid-session.
#[test]
fn gpu_buffers_and_cpu_pixels_can_be_mixed_on_one_encoder() {
    let Some(mut encoder) = gpu_encoder() else {
        return;
    };
    let device = gbm_device();
    let buffer = Buffer::new(&device);
    let stride = SIZE.0 as usize * 4;
    let pixels = vec![0x40u8; stride * SIZE.1 as usize];

    assert!(
        encoder
            .encode_bgrx(&pixels, stride, false)
            .unwrap()
            .unwrap()
            .keyframe
    );
    assert!(
        !encoder
            .encode_dmabuf(&buffer.as_dmabuf(), false)
            .unwrap()
            .unwrap()
            .keyframe
    );
    assert!(
        !encoder
            .encode_bgrx(&pixels, stride, false)
            .unwrap()
            .unwrap()
            .keyframe
    );
    assert!(
        !encoder
            .encode_dmabuf(&buffer.as_dmabuf(), false)
            .unwrap()
            .unwrap()
            .keyframe
    );
}

#[test]
fn a_software_encoder_refuses_gpu_buffers_with_a_reason() {
    let Ok(opened) = Encoder::open(&settings(Backend::OpenH264)) else {
        eprintln!("SKIPPED: no OpenH264");
        return;
    };
    let mut encoder = opened.encoder;
    assert!(!encoder.takes_gpu_buffers());
    let device = gbm_device();
    let buffer = Buffer::new(&device);
    let err = encoder
        .encode_dmabuf(&buffer.as_dmabuf(), false)
        .unwrap_err();
    assert!(matches!(err, Error::NeedsGpuEncoder(_)), "{err}");
}

#[test]
fn an_unknown_pixel_format_is_refused_and_the_encoder_keeps_working() {
    let Some(mut encoder) = gpu_encoder() else {
        return;
    };
    let device = gbm_device();
    let buffer = Buffer::new(&device);
    let odd = Dmabuf {
        fourcc: u32::from_le_bytes(*b"XR30"),
        ..buffer.as_dmabuf()
    };
    let err = encoder.encode_dmabuf(&odd, false).unwrap_err();
    assert!(err.to_string().contains("XR30"), "{err}");
    assert!(
        encoder
            .encode_dmabuf(&buffer.as_dmabuf(), false)
            .unwrap()
            .is_some()
    );
}
