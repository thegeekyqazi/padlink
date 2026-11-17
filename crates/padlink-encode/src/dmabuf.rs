//! Zero-copy input: a GPU buffer from the compositor goes straight into the
//! VA-API video processor, with no download and no upload (R1.12).
//!
//! The buffer arrives as a dmabuf. FFmpeg sees it as a `DRM_PRIME` frame;
//! `hwmap` imports it as a VA-API surface on the encoder's own VA-API
//! device, and `scale_vaapi` converts it to NV12. This is the pipeline
//! `wf-recorder` uses and Spike 4 proved on this GPU.

use std::ffi::c_int;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr;

use ffmpeg_sys_next as sys;

use crate::{Error, Settings, ff};

/// One single-plane GPU buffer holding a 32-bit RGB picture.
#[derive(Debug, Clone, Copy)]
pub struct Dmabuf<'a> {
    pub fd: BorrowedFd<'a>,
    /// The DRM format code, such as `XR24`.
    pub fourcc: u32,
    pub modifier: u64,
    /// Where the pixels start in the buffer, and the length of a row.
    pub offset: u32,
    pub stride: u32,
}

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// The FFmpeg name of a DRM format padlink can take, if it is one.
fn pixel_format(code: u32) -> Option<sys::AVPixelFormat> {
    use sys::AVPixelFormat::*;
    const XR24: u32 = fourcc(b"XR24");
    const AR24: u32 = fourcc(b"AR24");
    const XB24: u32 = fourcc(b"XB24");
    const AB24: u32 = fourcc(b"AB24");
    match code {
        XR24 => Some(AV_PIX_FMT_BGR0),
        AR24 => Some(AV_PIX_FMT_BGRA),
        XB24 => Some(AV_PIX_FMT_RGB0),
        AB24 => Some(AV_PIX_FMT_RGBA),
        _ => None,
    }
}

/// `XR24` and the like, for messages.
pub fn fourcc_name(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

/// The filter graph for one buffer format.
pub(crate) struct DmabufSource {
    pub fourcc: u32,
    frame: ff::Frame,
    src: *mut sys::AVFilterContext,
    sink: *mut sys::AVFilterContext,
    /// Describes the incoming buffers to FFmpeg.
    frames: ff::BufferRef,
    _graph: ff::FilterGraph,
}

/// Allocate a filter in `graph`, attach the GPU, then initialise it.
///
/// # Safety
/// `graph` must be a live filter graph and `device` a live device reference.
unsafe fn filter(
    graph: &ff::FilterGraph,
    device: &ff::BufferRef,
    kind: &str,
    name: &str,
    args: Option<&str>,
    before_init: impl FnOnce(*mut sys::AVFilterContext) -> Result<(), Error>,
) -> Result<*mut sys::AVFilterContext, Error> {
    let (kind_c, name_c) = (ff::cstr(kind), ff::cstr(name));
    let args_c = args.map(ff::cstr);
    unsafe {
        let def = sys::avfilter_get_by_name(kind_c.as_ptr());
        if def.is_null() {
            return Err(Error::Ffmpeg {
                what: format!("find the `{kind}` filter"),
                detail: "this FFmpeg build does not have it".into(),
            });
        }
        let ctx = sys::avfilter_graph_alloc_filter(graph.0, def, name_c.as_ptr());
        if ctx.is_null() {
            return Err(Error::OutOfMemory);
        }
        (*ctx).hw_device_ctx = sys::av_buffer_ref(device.0);
        before_init(ctx)?;
        ff::check(
            sys::avfilter_init_str(ctx, args_c.as_ref().map_or(ptr::null(), |a| a.as_ptr())),
            &format!("set up the `{kind}` filter"),
        )?;
        Ok(ctx)
    }
}

impl DmabufSource {
    /// Build `drm_prime -> hwmap -> scale_vaapi(nv12)` for buffers of this
    /// format. `vaapi` is the device the encoder runs on.
    pub fn new(s: &Settings, vaapi: &ff::BufferRef, code: u32) -> Result<Self, Error> {
        let sw_format = pixel_format(code).ok_or_else(|| Error::Ffmpeg {
            what: "take the compositor's GPU buffer".into(),
            detail: format!(
                "its pixel format {} is not one padlink reads",
                fourcc_name(code)
            ),
        })?;
        let node = ff::cstr(&s.render_node.to_string_lossy());
        let (w, h) = (s.width as c_int, s.height as c_int);

        // SAFETY: each pointer is checked before use; the DRM device and
        // frames context are owned by the wrappers and outlive the graph
        // through their own references.
        unsafe {
            let mut drm = ff::BufferRef(ptr::null_mut());
            ff::check(
                sys::av_hwdevice_ctx_create(
                    &mut drm.0,
                    sys::AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
                    node.as_ptr(),
                    ptr::null_mut(),
                    0,
                ),
                &format!("open the GPU at {}", s.render_node.display()),
            )?;
            let frames = ff::BufferRef(sys::av_hwframe_ctx_alloc(drm.0));
            if frames.0.is_null() {
                return Err(Error::OutOfMemory);
            }
            let f = (*frames.0).data as *mut sys::AVHWFramesContext;
            (*f).format = sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME;
            (*f).sw_format = sw_format;
            (*f).width = w;
            (*f).height = h;
            ff::check(sys::av_hwframe_ctx_init(frames.0), "describe GPU buffers")?;

            let graph = ff::FilterGraph(sys::avfilter_graph_alloc());
            if graph.0.is_null() {
                return Err(Error::OutOfMemory);
            }
            let src = filter(&graph, vaapi, "buffer", "in", None, |ctx| {
                // A hardware format must come with its frames context,
                // before the filter is initialised.
                let par = sys::av_buffersrc_parameters_alloc();
                if par.is_null() {
                    return Err(Error::OutOfMemory);
                }
                (*par).format = sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;
                (*par).width = w;
                (*par).height = h;
                (*par).time_base = sys::AVRational {
                    num: 1,
                    den: s.fps.max(1) as c_int,
                };
                (*par).hw_frames_ctx = frames.0;
                let code = sys::av_buffersrc_parameters_set(ctx, par);
                sys::av_free(par.cast());
                ff::check(code, "describe GPU buffers to the converter").map(drop)
            })?;
            let map = filter(&graph, vaapi, "hwmap", "import", Some("mode=read"), |_| {
                Ok(())
            })?;
            let convert = filter(
                &graph,
                vaapi,
                "scale_vaapi",
                "convert",
                Some("format=nv12"),
                |_| Ok(()),
            )?;
            let sink = filter(&graph, vaapi, "buffersink", "out", None, |_| Ok(()))?;
            for (a, b) in [(src, map), (map, convert), (convert, sink)] {
                ff::check(sys::avfilter_link(a, 0, b, 0), "connect the GPU filters")?;
            }
            ff::check(
                sys::avfilter_graph_config(graph.0, ptr::null_mut()),
                "configure zero-copy colour conversion (GPU buffer import)",
            )?;

            Ok(DmabufSource {
                fourcc: code,
                frame: ff::Frame::new()?,
                src,
                sink,
                frames,
                _graph: graph,
            })
        }
    }

    /// Import `buf` and convert it; the NV12 picture lands in `out`.
    pub fn convert(
        &mut self,
        s: &Settings,
        buf: &Dmabuf,
        out: &mut ff::Frame,
    ) -> Result<(), Error> {
        // SAFETY: the descriptor is allocated zeroed, filled, and handed to
        // an AVBuffer that frees it; the frame owns that buffer and a
        // reference to the frames context. FFmpeg does not close the fd: it
        // stays the caller's, who keeps it open for the duration of the call.
        unsafe {
            let size = std::mem::size_of::<sys::AVDRMFrameDescriptor>();
            let desc = sys::av_mallocz(size) as *mut sys::AVDRMFrameDescriptor;
            if desc.is_null() {
                return Err(Error::OutOfMemory);
            }
            (*desc).nb_objects = 1;
            (*desc).objects[0].fd = buf.fd.as_raw_fd();
            (*desc).objects[0].size = 0;
            (*desc).objects[0].format_modifier = buf.modifier;
            (*desc).nb_layers = 1;
            (*desc).layers[0].format = buf.fourcc;
            (*desc).layers[0].nb_planes = 1;
            (*desc).layers[0].planes[0].object_index = 0;
            (*desc).layers[0].planes[0].offset = buf.offset as isize;
            (*desc).layers[0].planes[0].pitch = buf.stride as isize;

            let frame = self.frame.0;
            sys::av_frame_unref(frame);
            (*frame).buf[0] = sys::av_buffer_create(desc.cast(), size, None, ptr::null_mut(), 0);
            if (*frame).buf[0].is_null() {
                sys::av_free(desc.cast());
                return Err(Error::OutOfMemory);
            }
            (*frame).data[0] = desc.cast();
            (*frame).format = sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;
            (*frame).width = s.width as c_int;
            (*frame).height = s.height as c_int;
            (*frame).hw_frames_ctx = sys::av_buffer_ref(self.frames.0);

            // The filter takes the frame's references; ours is left empty.
            ff::check(
                sys::av_buffersrc_add_frame_flags(self.src, frame, 0),
                "import a GPU buffer",
            )?;
            ff::check(
                sys::av_buffersink_get_frame(self.sink, out.0),
                "convert a GPU buffer",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_codes_have_names_and_known_ones_map_to_pixel_formats() {
        let xr24 = fourcc(b"XR24");
        assert_eq!(xr24, 0x3432_5258);
        assert_eq!(fourcc_name(xr24), "XR24");
        assert_eq!(
            pixel_format(xr24),
            Some(sys::AVPixelFormat::AV_PIX_FMT_BGR0)
        );
        assert_eq!(
            pixel_format(fourcc(b"AR24")),
            Some(sys::AVPixelFormat::AV_PIX_FMT_BGRA)
        );
        // 10-bit and YUV formats are not taken.
        assert_eq!(pixel_format(fourcc(b"XR30")), None);
        assert_eq!(pixel_format(fourcc(b"NV12")), None);
        assert_eq!(fourcc_name(0), "????");
    }
}
