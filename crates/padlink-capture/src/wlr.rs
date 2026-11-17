//! Continuous capture of one output through `wlr-screencopy-unstable-v1`
//! into shared-memory buffers.
//!
//! Per frame: `capture_output`, then the compositor describes the buffer it
//! wants (`buffer`, and `buffer_done` from version 3), then
//! `copy_with_damage` into a free buffer, then `ready` or `failed`. One frame
//! is in flight at a time. `copy_with_damage` completes only once the output
//! has new damage, so a static screen yields no frames.
//!
//! Adapted from OpenDisplay's `od-sender/src/wayland/screencopy.rs` and
//! `capture.rs` (GPL-3.0).

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memmap2::Mmap;
use tracing::{debug, info};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use crate::pool::{self, Recycler};
use crate::{End, Error, Frame, Options, Path};

/// Buffers that can be busy at once: one attached to the compositor, one
/// waiting in the encoder's latest-wins slot, one being encoded, and one
/// spare so the compositor never waits for the encoder.
const MAX_BUFFERS: usize = 4;

/// Failed copies in a row, with the output still present, before giving up.
const MAX_CONSECUTIVE_FAILURES: u32 = 30;

/// How often the loop looks at the stop flag while nothing happens.
const WAKE: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
struct FrameState {
    /// `buffer`: format, width, height, stride.
    shm: Option<(WEnum<wl_shm::Format>, u32, u32, u32)>,
    buffer_done: bool,
    ready: bool,
    failed: bool,
}

struct State {
    shm: Option<wl_shm::WlShm>,
    manager: Option<(ZwlrScreencopyManagerV1, u32)>,
    /// Registry name to the output and its name.
    outputs: HashMap<u32, (wl_output::WlOutput, String)>,
    /// Registry name of the captured output, once chosen.
    target: Option<u32>,
    output_removed: bool,
    frame: FrameState,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                // Version 4 is the first that reports the output's name.
                "wl_output" if version >= 4 => {
                    let output = registry.bind(name, 4, qh, name);
                    state.outputs.insert(name, (output, String::new()));
                }
                "zwlr_screencopy_manager_v1" => {
                    let v = version.min(3);
                    state.manager = Some((registry.bind(name, v, qh, ()), v));
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                state.outputs.remove(&name);
                if state.target == Some(name) {
                    state.output_removed = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(output) = state.outputs.get_mut(global) {
                output.1 = name;
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        let f = &mut state.frame;
        match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => f.shm = Some((format, width, height, stride)),
            Event::BufferDone => f.buffer_done = true,
            Event::Ready { .. } => f.ready = true,
            Event::Failed => f.failed = true,
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    ZwlrScreencopyManagerV1
);

/// One mapped shared-memory buffer.
struct ShmBuffer {
    map: Arc<Mmap>,
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    layout: (u32, u32, u32),
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        (width, height, stride): (u32, u32, u32),
        format: wl_shm::Format,
    ) -> Result<Self, Error> {
        let size = stride as usize * height as usize;
        let fd = rustix::fs::memfd_create("padlink-capture", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(std::io::Error::from)?;
        let file = File::from(fd);
        file.set_len(size as u64)?;
        let map = pool::map(&file, size)?;
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            qh,
            (),
        );
        Ok(ShmBuffer {
            map,
            pool,
            buffer,
            layout: (width, height, stride),
        })
    }
}

impl Drop for ShmBuffer {
    /// A lease may still be reading the mapping; it holds its own reference.
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}

/// What to do after a frame failed.
#[derive(Debug, PartialEq, Eq)]
enum AfterFailure {
    Retry,
    OutputGone,
    GiveUp,
}

fn after_failure(output_removed: bool, consecutive: u32) -> AfterFailure {
    if output_removed {
        AfterFailure::OutputGone
    } else if consecutive >= MAX_CONSECUTIVE_FAILURES {
        AfterFailure::GiveUp
    } else {
        AfterFailure::Retry
    }
}

fn lost(e: impl std::fmt::Display) -> Error {
    Error::Wayland(e.to_string())
}

/// Dispatch pending events, then wait up to `timeout` for more.
fn dispatch(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    timeout: Duration,
) -> Result<(), Error> {
    queue.dispatch_pending(state).map_err(lost)?;
    conn.flush().map_err(lost)?;
    let Some(guard) = conn.prepare_read() else {
        queue.dispatch_pending(state).map_err(lost)?;
        return Ok(());
    };
    let readable = {
        let fd = guard.connection_fd();
        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        let timeout = rustix::event::Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        rustix::event::poll(&mut fds, Some(&timeout)).is_ok_and(|n| n > 0)
    };
    if readable {
        // A dead connection must end the caller's loop, not spin on it.
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(lost(e)),
        }
        queue.dispatch_pending(state).map_err(lost)?;
    }
    Ok(())
}

/// Capture `options.output` until `stop` is set or the output goes away.
/// Each frame is handed to `on_frame`, which should take what it needs and
/// return at once: the capture loop is blocked while it runs.
///
/// `on_start` is called once, before the first frame, with the path in use.
///
/// When this returns, every capture object has been destroyed and the
/// compositor has processed that, so the caller may remove the output
/// (R1.18). Never remove the output while this is still running.
pub fn capture(
    options: &Options,
    stop: &AtomicBool,
    on_start: impl FnOnce(Path),
    mut on_frame: impl FnMut(Frame),
) -> Result<End, Error> {
    let connect_error = |detail: String| Error::Connect {
        path: options.wayland_socket.clone(),
        detail,
    };
    let stream =
        UnixStream::connect(&options.wayland_socket).map_err(|e| connect_error(e.to_string()))?;
    let conn = Connection::from_socket(stream).map_err(|e| connect_error(e.to_string()))?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State {
        shm: None,
        manager: None,
        outputs: HashMap::new(),
        target: None,
        output_removed: false,
        frame: FrameState::default(),
    };
    // Once for the globals, once more for the outputs' names.
    queue.roundtrip(&mut state).map_err(lost)?;
    queue.roundtrip(&mut state).map_err(lost)?;

    let shm = state.shm.clone().ok_or(Error::MissingProtocol("wl_shm"))?;
    let (manager, version) = state
        .manager
        .clone()
        .ok_or(Error::MissingProtocol("zwlr_screencopy_manager_v1"))?;
    let (global, output) = state
        .outputs
        .iter()
        .find(|(_, (_, name))| *name == options.output)
        .map(|(global, (output, _))| (*global, output.clone()))
        .ok_or_else(|| {
            let mut known: Vec<&str> = state.outputs.values().map(|o| o.1.as_str()).collect();
            known.sort_unstable();
            Error::NoOutput {
                wanted: options.output.clone(),
                known: known.join(", "),
            }
        })?;
    state.target = Some(global);
    info!(
        "capturing {} with wlr-screencopy version {version}",
        options.output
    );
    on_start(Path::WlrShm);

    let recycler = Recycler::default();
    let mut buffers: Vec<ShmBuffer> = Vec::new();
    let mut failures = 0u32;
    let stopped = || stop.load(Ordering::Relaxed);

    let end = 'capture: loop {
        // The output may have gone while waiting: never ask for a frame then.
        dispatch(&conn, &mut queue, &mut state, Duration::ZERO)?;
        if state.output_removed {
            break End::OutputRemoved;
        }
        if stopped() {
            break End::Stopped;
        }

        state.frame = FrameState::default();
        let frame = manager.capture_output(i32::from(options.cursor), &output, &qh, ());
        // Runs on every way out of this iteration.
        let frame = scopeguard(frame);

        // 1. The buffer the compositor wants.
        while !stopped()
            && !state.frame.failed
            && !state.output_removed
            && !(state.frame.shm.is_some() && (state.frame.buffer_done || version < 3))
        {
            dispatch(&conn, &mut queue, &mut state, WAKE)?;
        }
        if stopped() {
            break End::Stopped;
        }
        let Some((format, width, height, stride)) = state.frame.shm.filter(|_| !state.frame.failed)
        else {
            failures += 1;
            match after_failure(state.output_removed, failures) {
                AfterFailure::Retry => continue,
                AfterFailure::OutputGone => break End::OutputRemoved,
                AfterFailure::GiveUp => return Err(Error::AlwaysFails(options.output.clone())),
            }
        };
        let format = match format {
            WEnum::Value(f @ (wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888)) => f,
            other => return Err(Error::UnsupportedFormat(format!("{other:?}"))),
        };

        // 2. A free buffer of that shape.
        let layout = (width, height, stride);
        if buffers.first().is_some_and(|b| b.layout != layout) {
            recycler.reset();
            buffers.clear();
        }
        let index = loop {
            if let Some(i) = recycler.take() {
                break i;
            }
            if buffers.len() < MAX_BUFFERS {
                buffers.push(ShmBuffer::new(&shm, &qh, layout, format)?);
                debug!(
                    "capture buffer {} allocated at {width}x{height}",
                    buffers.len()
                );
                break buffers.len() - 1;
            }
            // Every buffer is lent out: the encoder is behind.
            if stopped() {
                break 'capture End::Stopped;
            }
            recycler.wait(WAKE);
        };

        // 3. The copy, which completes when the output has new damage.
        frame.copy_with_damage(&buffers[index].buffer);
        while !stopped() && !state.frame.ready && !state.frame.failed && !state.output_removed {
            dispatch(&conn, &mut queue, &mut state, WAKE)?;
        }
        if state.frame.ready && !state.frame.failed {
            failures = 0;
            let captured_at = Instant::now();
            let captured_wall_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
            on_frame(Frame {
                width,
                height,
                stride,
                pixels: recycler.lease(
                    index,
                    buffers[index].map.clone(),
                    stride as usize * height as usize,
                ),
                captured_at,
                captured_wall_ms,
            });
        } else {
            recycler.release(index);
            if stopped() {
                break End::Stopped;
            }
            failures += 1;
            match after_failure(state.output_removed, failures) {
                AfterFailure::Retry => {}
                AfterFailure::OutputGone => break End::OutputRemoved,
                AfterFailure::GiveUp => return Err(Error::AlwaysFails(options.output.clone())),
            }
        }
    };

    // Teardown (R1.18): destroy everything, then one round trip so the
    // compositor has processed the destruction before the caller acts on it.
    buffers.clear();
    manager.destroy();
    output.release();
    let _ = queue.roundtrip(&mut state);
    info!("capture of {} ended: {end:?}", options.output);
    Ok(end)
}

/// Destroys the frame object when dropped.
struct FrameGuard(ZwlrScreencopyFrameV1);

impl Drop for FrameGuard {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

impl std::ops::Deref for FrameGuard {
    type Target = ZwlrScreencopyFrameV1;

    fn deref(&self) -> &ZwlrScreencopyFrameV1 {
        &self.0
    }
}

fn scopeguard(frame: ZwlrScreencopyFrameV1) -> FrameGuard {
    FrameGuard(frame)
}

/// The names of the protocols the compositor offers, for `padlink doctor`.
pub fn protocols(wayland_socket: &std::path::Path) -> Result<Vec<String>, Error> {
    struct Names(Vec<String>);
    impl Dispatch<wl_registry::WlRegistry, ()> for Names {
        fn event(
            names: &mut Self,
            _: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { interface, .. } = event {
                names.0.push(interface);
            }
        }
    }
    let connect_error = |detail: String| Error::Connect {
        path: wayland_socket.to_path_buf(),
        detail,
    };
    let stream = UnixStream::connect(wayland_socket).map_err(|e| connect_error(e.to_string()))?;
    let conn = Connection::from_socket(stream).map_err(|e| connect_error(e.to_string()))?;
    let mut queue = conn.new_event_queue::<Names>();
    conn.display().get_registry(&queue.handle(), ());
    let mut names = Names(Vec::new());
    queue.roundtrip(&mut names).map_err(lost)?;
    Ok(names.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_end_the_capture_only_when_the_output_is_gone_or_hopeless() {
        assert_eq!(after_failure(false, 1), AfterFailure::Retry);
        assert_eq!(after_failure(true, 1), AfterFailure::OutputGone);
        assert_eq!(
            after_failure(false, MAX_CONSECUTIVE_FAILURES),
            AfterFailure::GiveUp
        );
    }
}
