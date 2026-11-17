//! Video encoding for padlink.
//!
//! One [`Encoder`], several backends, chosen at run time (R1.13):
//!
//! 1. `h264_vaapi`: the GPU encodes. Pixels are uploaded to a VA-API surface
//!    and converted to NV12 by the VA-API video processor, so the CPU does
//!    no colour conversion.
//! 2. `libx264`, when this FFmpeg build has it.
//! 3. `libopenh264`, which Fedora's `ffmpeg-free` always has.
//!
//! Every backend is configured for latency: no B-frames, no lookahead, one
//! keyframe at the start and then only on request (SPEC.md section 4).
//!
//! The pipeline follows `wf-recorder` and Sunshine's `vaapi.cpp`; the
//! software path follows OpenDisplay's `od-sender/src/encoder/ffmpeg.rs`.

// FFmpeg is a C library: this crate is where padlink's unsafe code for it
// lives. Each block states why it is sound.
#![allow(unsafe_code)]

mod ff;
mod packet;

use std::ffi::c_int;
use std::path::PathBuf;
use std::ptr;
use std::time::Instant;

use ffmpeg_sys_next as sys;

pub use packet::Packet;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("FFmpeg could not {what}: {detail}")]
    Ffmpeg { what: String, detail: String },

    #[error("out of memory in the video encoder")]
    OutOfMemory,

    #[error(
        "this FFmpeg build has no `{0}` encoder. On Fedora install `libavcodec-freeworld` \
         from RPM Fusion for x264, or use `encoder = \"openh264\"`"
    )]
    Missing(&'static str),

    #[error(
        "no video encoder could be opened. Tried: {0}. For hardware encoding on AMD or \
         Intel, install `mesa-va-drivers-freeworld` and check `vainfo` lists H264 with \
         VAEntrypointEncSlice"
    )]
    NothingWorks(String),

    #[error(
        "the frame is {got} bytes but {width}x{height} with a row length of {stride} needs {need}"
    )]
    ShortFrame {
        got: usize,
        need: usize,
        width: u32,
        height: u32,
        stride: usize,
    },

    #[error("there is no previous frame to repeat")]
    NothingToRepeat,
}

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The first that opens, in the order above.
    Auto,
    Vaapi,
    X264,
    OpenH264,
}

/// What is actually running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Vaapi,
    X264,
    OpenH264,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Vaapi => "h264_vaapi (GPU)",
            Kind::X264 => "x264 (CPU)",
            Kind::OpenH264 => "OpenH264 (CPU)",
        }
    }

    fn codec_name(self) -> &'static str {
        match self {
            Kind::Vaapi => "h264_vaapi",
            Kind::X264 => "libx264",
            Kind::OpenH264 => "libopenh264",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub backend: Backend,
    /// The GPU to encode on, such as `/dev/dri/renderD128`.
    pub render_node: PathBuf,
    /// Extra private options for the codec, applied after padlink's own, for
    /// tuning without a rebuild: `("compression_level", "0")`.
    pub codec_options: Vec<(String, String)>,
}

/// Where the time for the last frame went, in milliseconds (R1.24).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Timings {
    /// Getting the pixels into the form the codec reads: upload and colour
    /// conversion on the GPU, or colour conversion on the CPU.
    pub prepare_ms: f64,
    /// The codec itself, from handing over the frame to having the packet.
    pub encode_ms: f64,
}

/// An open encoder, and why any earlier choice was passed over (R1.13 asks
/// for the choice and the reason to be logged).
pub struct Opened {
    pub encoder: Encoder,
    pub skipped: Vec<(Kind, String)>,
}

pub struct Encoder {
    kind: Kind,
    settings: Settings,
    source: Source,
    codec: ff::CodecContext,
    packet: ff::Packet,
    /// The last picture handed to the codec, kept so it can be sent again as
    /// a keyframe when the screen is static (SPEC.md section 8).
    last: ff::Frame,
    has_last: bool,
    pts: i64,
    params: packet::ParameterSets,
    timings: Timings,
}

/// How CPU pixels become a frame the codec accepts.
enum Source {
    /// Upload to a VA-API surface, convert to NV12 on the GPU.
    Vaapi {
        // Field order is drop order: the graph must go before the device.
        staging: ff::Frame,
        src: *mut sys::AVFilterContext,
        sink: *mut sys::AVFilterContext,
        _graph: ff::FilterGraph,
        _device: ff::BufferRef,
    },
    /// Convert to planar YUV on the CPU.
    Software { sws: ff::Sws, yuv: ff::Frame },
}

// SAFETY: the raw filter pointers belong to the graph stored beside them,
// and an Encoder is only used from one thread at a time.
unsafe impl Send for Encoder {}

/// `AV_BUFFERSRC_FLAG_KEEP_REF`: the filter takes its own reference and
/// leaves ours, so the staging frame can be reused.
const BUFFERSRC_KEEP_REF: c_int = 8;

/// `SWS_FAST_BILINEAR`. No scaling happens, so the cheapest filter is right.
const SWS_FAST_BILINEAR: c_int = 1;

impl Encoder {
    /// Open the encoder the settings ask for. With [`Backend::Auto`] each
    /// backend is tried in turn and the reasons for passing one over are
    /// returned alongside the encoder.
    pub fn open(settings: &Settings) -> Result<Opened, Error> {
        ff::quiet_logs();
        let order: &[Kind] = match settings.backend {
            Backend::Auto => &[Kind::Vaapi, Kind::X264, Kind::OpenH264],
            Backend::Vaapi => &[Kind::Vaapi],
            Backend::X264 => &[Kind::X264],
            Backend::OpenH264 => &[Kind::OpenH264],
        };
        let mut skipped = Vec::new();
        for &kind in order {
            match Encoder::open_kind(kind, settings) {
                Ok(encoder) => return Ok(Opened { encoder, skipped }),
                Err(e) if order.len() == 1 => return Err(e),
                Err(e) => skipped.push((kind, e.to_string())),
            }
        }
        Err(Error::NothingWorks(
            skipped
                .iter()
                .map(|(k, why)| format!("{} ({why})", k.label()))
                .collect::<Vec<_>>()
                .join("; "),
        ))
    }

    fn open_kind(kind: Kind, s: &Settings) -> Result<Encoder, Error> {
        if !ff::has_encoder(kind.codec_name()) {
            return Err(Error::Missing(kind.codec_name()));
        }
        let (source, hw_frames) = match kind {
            Kind::Vaapi => {
                let (source, frames) = vaapi_source(s)?;
                (source, Some(frames))
            }
            Kind::X264 | Kind::OpenH264 => (software_source(s)?, None),
        };
        let codec = open_codec(kind, s, hw_frames)?;
        Ok(Encoder {
            kind,
            settings: s.clone(),
            source,
            codec,
            packet: ff::Packet::new()?,
            last: ff::Frame::new()?,
            has_last: false,
            pts: 0,
            params: packet::ParameterSets::default(),
            timings: Timings::default(),
        })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Stage timings of the most recent frame.
    pub fn timings(&self) -> Timings {
        self.timings
    }

    /// One line for the log and `padlink status`.
    pub fn describe(&self) -> String {
        format!(
            "{} {}x{} at {} fps, {:.1} Mbit/s",
            self.kind.label(),
            self.settings.width,
            self.settings.height,
            self.settings.fps,
            f64::from(self.settings.bitrate_bps) / 1e6
        )
    }

    /// Encode one frame of 32-bit pixels in the byte order blue, green, red,
    /// unused (`XRGB8888` on Wayland, `bgr0` in FFmpeg). `stride` is the
    /// length of a row in bytes. Returns `None` if the codec is still
    /// holding the frame, which the latency settings are meant to prevent.
    pub fn encode_bgrx(
        &mut self,
        data: &[u8],
        stride: usize,
        force_keyframe: bool,
    ) -> Result<Option<Packet>, Error> {
        let (w, h) = (self.settings.width as usize, self.settings.height as usize);
        let need = stride.saturating_mul(h.saturating_sub(1)) + w * 4;
        if stride < w * 4 || data.len() < need {
            return Err(Error::ShortFrame {
                got: data.len(),
                need,
                width: self.settings.width,
                height: self.settings.height,
                stride,
            });
        }

        let started = Instant::now();
        self.last.unref();
        self.has_last = false;
        match &mut self.source {
            Source::Vaapi {
                staging, src, sink, ..
            } => {
                // SAFETY: staging owns a writable w x h BGR0 buffer (made
                // writable here), rows are copied within both bounds, and
                // src/sink are live filters of the graph stored beside them.
                unsafe {
                    ff::check(
                        sys::av_frame_make_writable(staging.0),
                        "prepare a frame for upload",
                    )?;
                    copy_rows(data, stride, staging.0, w, h);
                    ff::check(
                        sys::av_buffersrc_add_frame_flags(*src, staging.0, BUFFERSRC_KEEP_REF),
                        "upload a frame to the GPU",
                    )?;
                    ff::check(
                        sys::av_buffersink_get_frame(*sink, self.last.0),
                        "convert a frame on the GPU",
                    )?;
                }
            }
            Source::Software { sws, yuv } => {
                let src_planes = [data.as_ptr(), ptr::null(), ptr::null(), ptr::null()];
                let src_strides = [stride as c_int, 0, 0, 0];
                // SAFETY: yuv owns a writable w x h YUV420P buffer; the
                // source plane was checked above to cover h rows of stride.
                unsafe {
                    ff::check(
                        sys::av_frame_make_writable(yuv.0),
                        "prepare a frame for conversion",
                    )?;
                    sys::sws_scale(
                        sws.0,
                        src_planes.as_ptr(),
                        src_strides.as_ptr(),
                        0,
                        h as c_int,
                        (*yuv.0).data.as_ptr(),
                        (*yuv.0).linesize.as_ptr(),
                    );
                    ff::check(sys::av_frame_ref(self.last.0, yuv.0), "keep the last frame")?;
                }
            }
        }
        self.has_last = true;
        self.timings.prepare_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.send_last(force_keyframe)
    }

    /// Encode the previous picture again as a keyframe. Used when a receiver
    /// connects or asks for a keyframe while the screen is not changing.
    pub fn repeat_as_keyframe(&mut self) -> Result<Option<Packet>, Error> {
        if !self.has_last {
            return Err(Error::NothingToRepeat);
        }
        self.send_last(true)
    }

    fn send_last(&mut self, force_keyframe: bool) -> Result<Option<Packet>, Error> {
        let started = Instant::now();
        let result = self.send_last_inner(force_keyframe);
        self.timings.encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        result
    }

    fn send_last_inner(&mut self, force_keyframe: bool) -> Result<Option<Packet>, Error> {
        // SAFETY: last holds a picture in the codec's format; codec and
        // packet are live. The packet's bytes are copied before it is unref'd.
        unsafe {
            (*self.last.0).pts = self.pts;
            (*self.last.0).pict_type = if force_keyframe {
                sys::AVPictureType::AV_PICTURE_TYPE_I
            } else {
                sys::AVPictureType::AV_PICTURE_TYPE_NONE
            };
            self.pts += 1;
            ff::check(
                sys::avcodec_send_frame(self.codec.0, self.last.0),
                "hand a frame to the encoder",
            )?;
            let code = sys::avcodec_receive_packet(self.codec.0, self.packet.0);
            if code == ff::EAGAIN {
                return Ok(None);
            }
            ff::check(code, "read the encoded frame")?;
            let bytes = std::slice::from_raw_parts(
                (*self.packet.0).data,
                (*self.packet.0).size.max(0) as usize,
            );
            let packet = self.params.packetize(bytes);
            sys::av_packet_unref(self.packet.0);
            Ok(Some(packet))
        }
    }
}

/// Copy `h` rows of `w` BGRx pixels into a frame, honouring both strides.
///
/// # Safety
/// `frame` must own a writable BGR0 buffer of at least `w` x `h`, and
/// `data` must cover `h` rows of `stride` bytes (the last may stop at `w * 4`).
unsafe fn copy_rows(data: &[u8], stride: usize, frame: *mut sys::AVFrame, w: usize, h: usize) {
    unsafe {
        let dst = (*frame).data[0];
        let dst_stride = (*frame).linesize[0] as usize;
        for row in 0..h {
            ptr::copy_nonoverlapping(
                data.as_ptr().add(row * stride),
                dst.add(row * dst_stride),
                w * 4,
            );
        }
    }
}

/// A frame with its own buffer in the given pixel format.
fn alloc_frame(format: sys::AVPixelFormat, s: &Settings) -> Result<ff::Frame, Error> {
    let frame = ff::Frame::new()?;
    // SAFETY: the frame is freshly allocated; get_buffer fills it for the
    // format and size set here.
    unsafe {
        (*frame.0).format = format as c_int;
        (*frame.0).width = s.width as c_int;
        (*frame.0).height = s.height as c_int;
        ff::check(sys::av_frame_get_buffer(frame.0, 0), "allocate a frame")?;
    }
    Ok(frame)
}

/// Build `bgr0 -> hwupload -> scale_vaapi(nv12)` on the render node and
/// return it with the pool of NV12 surfaces the codec will read from.
fn vaapi_source(s: &Settings) -> Result<(Source, ff::BufferRef), Error> {
    let node = ff::cstr(&s.render_node.to_string_lossy());
    let mut device = ff::BufferRef(ptr::null_mut());
    // SAFETY: device.0 receives a new reference on success; the node is a
    // valid C string.
    unsafe {
        ff::check(
            sys::av_hwdevice_ctx_create(
                &mut device.0,
                sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                node.as_ptr(),
                ptr::null_mut(),
                0,
            ),
            &format!("open VA-API on {}", s.render_node.display()),
        )?;
    }

    // SAFETY: plain allocation, checked below.
    let graph = ff::FilterGraph(unsafe { sys::avfilter_graph_alloc() });
    if graph.0.is_null() {
        return Err(Error::OutOfMemory);
    }
    let make = |filter: &str, name: &str, args: Option<&str>| {
        let (filter_c, name_c) = (ff::cstr(filter), ff::cstr(name));
        let args_c = args.map(ff::cstr);
        // SAFETY: the graph is live; the strings outlive the calls; ctx is
        // owned by the graph afterwards. The filter is allocated first and
        // initialised second so the GPU can be attached in between:
        // hwupload refuses to initialise without one.
        unsafe {
            let def = sys::avfilter_get_by_name(filter_c.as_ptr());
            if def.is_null() {
                return Err(Error::Ffmpeg {
                    what: format!("find the `{filter}` filter"),
                    detail: "this FFmpeg build does not have it".into(),
                });
            }
            let ctx = sys::avfilter_graph_alloc_filter(graph.0, def, name_c.as_ptr());
            if ctx.is_null() {
                return Err(Error::OutOfMemory);
            }
            (*ctx).hw_device_ctx = device.clone_ref()?.into_raw();
            ff::check(
                sys::avfilter_init_str(ctx, args_c.as_ref().map_or(ptr::null(), |a| a.as_ptr())),
                &format!("set up the `{filter}` filter"),
            )?;
            Ok(ctx)
        }
    };
    let src = make(
        "buffer",
        "in",
        Some(&format!(
            "video_size={}x{}:pix_fmt=bgr0:time_base=1/{}:pixel_aspect=1/1",
            s.width,
            s.height,
            s.fps.max(1)
        )),
    )?;
    let upload = make("hwupload", "upload", None)?;
    let convert = make("scale_vaapi", "convert", Some("format=nv12"))?;
    let sink = make("buffersink", "out", None)?;

    // SAFETY: all four contexts belong to the graph.
    let frames = unsafe {
        for (a, b) in [(src, upload), (upload, convert), (convert, sink)] {
            ff::check(sys::avfilter_link(a, 0, b, 0), "connect the GPU filters")?;
        }
        ff::check(
            sys::avfilter_graph_config(graph.0, ptr::null_mut()),
            "configure GPU colour conversion (BGRx upload and NV12 conversion)",
        )?;
        let frames = sys::av_buffersink_get_hw_frames_ctx(sink);
        if frames.is_null() {
            return Err(Error::Ffmpeg {
                what: "get GPU surfaces from the converter".into(),
                detail: "the filter produced no hardware frames".into(),
            });
        }
        ff::BufferRef(frames).clone_ref_and_forget()?
    };

    let staging = alloc_frame(sys::AVPixelFormat::AV_PIX_FMT_BGR0, s)?;
    Ok((
        Source::Vaapi {
            staging,
            src,
            sink,
            _graph: graph,
            _device: device,
        },
        frames,
    ))
}

fn software_source(s: &Settings) -> Result<Source, Error> {
    let (w, h) = (s.width as c_int, s.height as c_int);
    // SAFETY: plain allocation with valid sizes and formats, checked below.
    let sws = ff::Sws(unsafe {
        sys::sws_getContext(
            w,
            h,
            sys::AVPixelFormat::AV_PIX_FMT_BGR0,
            w,
            h,
            sys::AVPixelFormat::AV_PIX_FMT_YUV420P,
            SWS_FAST_BILINEAR,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        )
    });
    if sws.0.is_null() {
        return Err(Error::OutOfMemory);
    }
    Ok(Source::Software {
        sws,
        yuv: alloc_frame(sys::AVPixelFormat::AV_PIX_FMT_YUV420P, s)?,
    })
}

fn open_codec(
    kind: Kind,
    s: &Settings,
    hw_frames: Option<ff::BufferRef>,
) -> Result<ff::CodecContext, Error> {
    let name = ff::cstr(kind.codec_name());
    let fps = s.fps.max(1) as c_int;
    let bitrate = i64::from(s.bitrate_bps);
    // SAFETY: the codec exists (checked by the caller); the context is
    // freshly allocated and only plain fields are written before open.
    unsafe {
        let codec = sys::avcodec_find_encoder_by_name(name.as_ptr());
        let ctx = ff::CodecContext(sys::avcodec_alloc_context3(codec));
        if ctx.0.is_null() {
            return Err(Error::OutOfMemory);
        }
        let c = &mut *ctx.0;
        c.width = s.width as c_int;
        c.height = s.height as c_int;
        c.time_base = sys::AVRational { num: 1, den: fps };
        c.framerate = sys::AVRational { num: fps, den: 1 };
        c.bit_rate = bitrate;
        c.rc_max_rate = bitrate;
        // A tenth of a second of buffer: the encoder may not save up bits and
        // then spend them on one huge frame, which would arrive late.
        c.rc_buffer_size = (bitrate / 10).clamp(1, i64::from(c_int::MAX)) as c_int;
        c.max_b_frames = 0;
        // No periodic keyframes: only the first frame and explicit requests.
        // OpenH264 reads 0 as "no period"; the others take a huge interval.
        c.gop_size = if kind == Kind::OpenH264 {
            0
        } else {
            c_int::MAX
        };
        c.flags |= sys::AV_CODEC_FLAG_LOW_DELAY as c_int;

        match kind {
            Kind::Vaapi => {
                c.pix_fmt = sys::AVPixelFormat::AV_PIX_FMT_VAAPI;
                c.hw_frames_ctx = hw_frames.expect("VA-API needs surfaces").into_raw();
                c.profile = 100; // High
                ff::set_option(&ctx, "rc_mode", "CBR");
                // One frame in flight: the packet for a frame is returned
                // before the next frame is accepted.
                ff::set_option(&ctx, "async_depth", "1");
                // An infinite GOP is expressed as "IDR only on request".
                ff::set_option(&ctx, "idr_interval", "0");
            }
            Kind::X264 => {
                c.pix_fmt = sys::AVPixelFormat::AV_PIX_FMT_YUV420P;
                ff::set_option(&ctx, "preset", "ultrafast");
                ff::set_option(&ctx, "tune", "zerolatency");
                // A requested keyframe must be an IDR, not a plain I-frame.
                ff::set_option(&ctx, "forced-idr", "1");
                ff::set_option(&ctx, "x264-params", "keyint=infinite:repeat-headers=1");
            }
            Kind::OpenH264 => {
                c.pix_fmt = sys::AVPixelFormat::AV_PIX_FMT_YUV420P;
                ff::set_option(&ctx, "allow_skip_frames", "0");
            }
        }

        for (key, value) in &s.codec_options {
            ff::set_option(&ctx, key, value);
        }

        ff::check(
            sys::avcodec_open2(ctx.0, codec, ptr::null_mut()),
            &format!(
                "open the {} encoder at {}x{}",
                kind.codec_name(),
                s.width,
                s.height
            ),
        )?;
        Ok(ctx)
    }
}

impl ff::BufferRef {
    /// Give the reference to FFmpeg, which will free it.
    fn into_raw(mut self) -> *mut sys::AVBufferRef {
        std::mem::replace(&mut self.0, ptr::null_mut())
    }

    /// Take a new reference to a buffer FFmpeg still owns, without freeing
    /// the borrowed one.
    fn clone_ref_and_forget(self) -> Result<ff::BufferRef, Error> {
        let owned = self.clone_ref();
        std::mem::forget(self);
        owned
    }
}
