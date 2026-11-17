//! Thin owning wrappers around the FFmpeg objects the encoder uses, so that
//! every allocation is freed on every path. All `unsafe` FFmpeg calls that
//! allocate or free live here.

use std::ffi::{CStr, CString, c_int};
use std::ptr;

use ffmpeg_sys_next as sys;

use crate::Error;

/// `AVERROR(EAGAIN)` on Linux: the encoder or filter wants more input.
pub const EAGAIN: c_int = -11;

/// Turn a negative FFmpeg return code into an error that says what failed.
pub fn check(code: c_int, what: &str) -> Result<c_int, Error> {
    if code >= 0 {
        return Ok(code);
    }
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: the buffer is valid for its length and av_strerror terminates it.
    let text = unsafe {
        sys::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    Err(Error::Ffmpeg {
        what: what.to_string(),
        detail: text,
    })
}

pub fn cstr(s: &str) -> CString {
    CString::new(s).expect("no NUL in FFmpeg option strings")
}

macro_rules! owned {
    ($name:ident, $ty:ty, $free:expr) => {
        pub struct $name(pub *mut $ty);
        impl Drop for $name {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    // SAFETY: the pointer came from the matching FFmpeg
                    // allocator and is freed exactly once.
                    #[allow(clippy::redundant_closure_call)]
                    unsafe {
                        $free(&mut self.0)
                    };
                }
            }
        }
        // SAFETY: FFmpeg objects are not tied to the thread that made them;
        // the wrappers are only ever used from one thread at a time.
        unsafe impl Send for $name {}
    };
}

owned!(
    BufferRef,
    sys::AVBufferRef,
    |p: &mut *mut sys::AVBufferRef| { sys::av_buffer_unref(p) }
);
owned!(Frame, sys::AVFrame, |p: &mut *mut sys::AVFrame| {
    sys::av_frame_free(p)
});
owned!(Packet, sys::AVPacket, |p: &mut *mut sys::AVPacket| {
    sys::av_packet_free(p)
});
owned!(
    CodecContext,
    sys::AVCodecContext,
    |p: &mut *mut sys::AVCodecContext| sys::avcodec_free_context(p)
);
owned!(
    FilterGraph,
    sys::AVFilterGraph,
    |p: &mut *mut sys::AVFilterGraph| sys::avfilter_graph_free(p)
);
owned!(Sws, sys::SwsContext, |p: &mut *mut sys::SwsContext| {
    sys::sws_freeContext(*p);
    *p = ptr::null_mut();
});

impl BufferRef {
    /// A new reference to the same buffer.
    pub fn clone_ref(&self) -> Result<BufferRef, Error> {
        // SAFETY: self.0 is a live AVBufferRef.
        let p = unsafe { sys::av_buffer_ref(self.0) };
        if p.is_null() {
            return Err(Error::OutOfMemory);
        }
        Ok(BufferRef(p))
    }
}

impl Frame {
    pub fn new() -> Result<Frame, Error> {
        // SAFETY: plain allocation.
        let p = unsafe { sys::av_frame_alloc() };
        if p.is_null() {
            return Err(Error::OutOfMemory);
        }
        Ok(Frame(p))
    }

    /// Drop whatever the frame holds, keeping the frame itself for reuse.
    pub fn unref(&mut self) {
        // SAFETY: self.0 is a live AVFrame.
        unsafe { sys::av_frame_unref(self.0) }
    }
}

impl Packet {
    pub fn new() -> Result<Packet, Error> {
        // SAFETY: plain allocation.
        let p = unsafe { sys::av_packet_alloc() };
        if p.is_null() {
            return Err(Error::OutOfMemory);
        }
        Ok(Packet(p))
    }
}

/// Whether this FFmpeg build has an encoder with this name.
pub fn has_encoder(name: &str) -> bool {
    let name = cstr(name);
    // SAFETY: the name is a valid C string; the result is a static codec.
    !unsafe { sys::avcodec_find_encoder_by_name(name.as_ptr()) }.is_null()
}

/// Set a private option on an encoder, such as `rc_mode` or `preset`.
/// Unknown options are skipped: encoders differ between FFmpeg builds.
pub fn set_option(ctx: &CodecContext, key: &str, value: &str) {
    let (k, v) = (cstr(key), cstr(value));
    // SAFETY: ctx is a live codec context whose priv_data was allocated by
    // avcodec_alloc_context3 for its codec.
    unsafe {
        sys::av_opt_set((*ctx.0).priv_data, k.as_ptr(), v.as_ptr(), 0);
    }
}

/// Send FFmpeg's own log lines above `warning` to nowhere: padlink reports
/// failures itself, in plain words.
pub fn quiet_logs() {
    // SAFETY: setting a global integer.
    unsafe { sys::av_log_set_level(sys::AV_LOG_ERROR) }
}
