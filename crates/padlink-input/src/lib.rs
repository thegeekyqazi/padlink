//! Input from the receiver, injected into the compositor.
//!
//! Finger, stylus and scroll events are mapped onto the virtual output by
//! [`Mapper`] and sent through `zwlr_virtual_pointer_v1` by an [`Injector`],
//! which owns a thread: a Wayland event queue cannot move between threads
//! (SPEC.md section 4).
//!
//! The virtual pointer is bound to the output, so a position in the picture
//! lands on the same spot of the output wherever it sits in the layout.
//!
//! Adapted from OpenDisplay's `od-sender/src/wayland/input.rs` (GPL-3.0).

mod map;

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use map::{Mapper, Op};
use padlink_proto::Pointer;
use tracing::{debug, warn};
use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// The left mouse button, from `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;

/// How long start-up and shutdown may take before the thread is left alone.
const PATIENCE: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot connect to the Wayland compositor at {path}: {detail}")]
    Connect { path: PathBuf, detail: String },

    #[error(
        "the compositor does not offer `zwlr_virtual_pointer_manager_v1`, which padlink \
         needs to turn touches into pointer input. Hyprland provides it"
    )]
    NoVirtualPointer,

    #[error("there is no output named {0} to send input to")]
    NoOutput(String),

    #[error("lost the connection to the compositor: {0}")]
    Wayland(String),

    #[error("the input thread did not start in time")]
    Timeout,
}

/// Where input goes.
#[derive(Debug, Clone)]
pub struct Target {
    pub wayland_socket: PathBuf,
    /// The virtual output's name.
    pub output: String,
    /// Its mode in pixels and its scale.
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

enum Command {
    Pointer(Pointer),
    Scroll { dx: f64, dy: f64 },
    Stop,
}

/// A virtual pointer on one output. Dropping it releases a held button and
/// destroys the pointer (R1.17).
pub struct Injector {
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
}

impl Injector {
    /// Connect and create the pointer. Returns once it is usable.
    pub fn start(target: Target) -> Result<Injector, Error> {
        let (commands, rx) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("padlink-input".into())
            .spawn(move || {
                if let Err(e) = run(&target, &rx, &ready_tx) {
                    // Before `ready` this reaches `start`; after it, the log.
                    if ready_tx.send(Err(e)).is_err() {
                        warn!("input stopped working");
                    }
                }
                let _ = done_tx.send(());
            })
            .map_err(|e| Error::Wayland(e.to_string()))?;
        match ready.recv_timeout(PATIENCE) {
            Ok(Ok(())) => Ok(Injector {
                commands,
                thread: Some(thread),
                done,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Timeout),
        }
    }

    /// A finger or stylus event. Never blocks.
    pub fn pointer(&self, pointer: Pointer) {
        let _ = self.commands.send(Command::Pointer(pointer));
    }

    /// A scroll, in picture pixels with the receiver's sign. Never blocks.
    pub fn scroll(&self, dx: f64, dy: f64) {
        let _ = self.commands.send(Command::Scroll { dx, dy });
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        // Wait for the button release to reach the compositor, but never
        // hang shutdown on a compositor that stopped answering.
        if self.done.recv_timeout(PATIENCE).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    outputs: HashMap<u32, (wl_output::WlOutput, String)>,
    manager: Option<(ZwlrVirtualPointerManagerV1, u32)>,
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
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_output" if version >= 4 => {
                    let output = registry.bind(name, 4, qh, name);
                    state.outputs.insert(name, (output, String::new()));
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    let v = version.min(2);
                    state.manager = Some((registry.bind(name, v, qh, ()), v));
                }
                _ => {}
            }
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

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_seat::WlSeat,
    ZwlrVirtualPointerManagerV1,
    ZwlrVirtualPointerV1
);

fn lost(e: impl std::fmt::Display) -> Error {
    Error::Wayland(e.to_string())
}

fn connect(socket: &Path) -> Result<Connection, Error> {
    let fail = |detail: String| Error::Connect {
        path: socket.to_path_buf(),
        detail,
    };
    let stream = UnixStream::connect(socket).map_err(|e| fail(e.to_string()))?;
    Connection::from_socket(stream).map_err(|e| fail(e.to_string()))
}

fn run(
    target: &Target,
    commands: &mpsc::Receiver<Command>,
    ready: &mpsc::Sender<Result<(), Error>>,
) -> Result<(), Error> {
    let conn = connect(&target.wayland_socket)?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).map_err(lost)?;
    queue.roundtrip(&mut state).map_err(lost)?;

    let (manager, version) = state.manager.clone().ok_or(Error::NoVirtualPointer)?;
    let output = state
        .outputs
        .values()
        .find(|(_, name)| *name == target.output)
        .map(|(output, _)| output.clone())
        .ok_or_else(|| Error::NoOutput(target.output.clone()))?;
    let pointer = if version >= 2 {
        manager.create_virtual_pointer_with_output(state.seat.as_ref(), Some(&output), &qh, ())
    } else {
        warn!(
            "this compositor's virtual pointer cannot be tied to one output; \
             touches may land on the wrong monitor"
        );
        manager.create_virtual_pointer(state.seat.as_ref(), &qh, ())
    };
    queue.roundtrip(&mut state).map_err(lost)?;
    let _ = ready.send(Ok(()));

    let mut mapper = Mapper::new(target.width, target.height, target.scale);
    let started = Instant::now();
    let apply = |ops: Vec<Op>, extents: (u32, u32)| {
        let time = started.elapsed().as_millis() as u32;
        for op in ops {
            match op {
                Op::Motion { x, y } => pointer.motion_absolute(time, x, y, extents.0, extents.1),
                Op::Button(down) => pointer.button(
                    time,
                    BTN_LEFT,
                    if down {
                        wl_pointer::ButtonState::Pressed
                    } else {
                        wl_pointer::ButtonState::Released
                    },
                ),
                Op::Axis {
                    vertical,
                    horizontal,
                } => {
                    pointer.axis_source(wl_pointer::AxisSource::Finger);
                    if vertical != 0.0 {
                        pointer.axis(time, wl_pointer::Axis::VerticalScroll, vertical);
                    }
                    if horizontal != 0.0 {
                        pointer.axis(time, wl_pointer::Axis::HorizontalScroll, horizontal);
                    }
                }
            }
            // One frame per operation: a press must be seen after the move
            // that put the pointer under the finger.
            pointer.frame();
        }
    };

    let mut result = Ok(());
    for command in commands {
        let ops = match command {
            Command::Stop => break,
            Command::Pointer(p) => mapper.pointer(p),
            Command::Scroll { dx, dy } => mapper.scroll(dx, dy),
        };
        apply(ops, mapper.extents());
        if let Err(e) = conn
            .flush()
            .map_err(lost)
            .and_then(|()| queue.dispatch_pending(&mut state).map(drop).map_err(lost))
        {
            result = Err(e);
            break;
        }
    }

    // R1.17: never leave the button held, whatever ended the session.
    apply(mapper.release(), mapper.extents());
    pointer.destroy();
    manager.destroy();
    let _ = queue.roundtrip(&mut state);
    debug!("input thread done");
    result
}
