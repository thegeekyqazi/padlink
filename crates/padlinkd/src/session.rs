//! One connection to a receiver, from `hello` to teardown.
//!
//! Threads, and the single slot or channel between each pair (SPEC.md
//! section 4):
//!
//! ```text
//! capture --[latest-wins slot]--> encode --[channel of 1]--> session --> socket
//! reader  --------------------------------------------------^
//! ```
//!
//! The session thread is the only writer to the socket and the only caller
//! of the receiver protocol. If the link stalls it blocks in `write`, the
//! encoder blocks on the full channel, and capture keeps replacing the frame
//! in the slot: frames are dropped, nothing queues.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use padlink_capture::{Frame, Options as CaptureOptions};
use padlink_encode::{Backend, Encoder, Settings as EncoderSettings};
use padlink_hypr::{Dialect, Hyprland, OutputSpec, Placement, logical_size, place};
use padlink_input::{Injector, Target};
use padlink_proto::{Action, Codec, DisplayRequest, LinkStats, Now, Receiver, plan_stream};
use tracing::{debug, error, info, warn};

use padlink_config::runtime::{Latency, State};

use crate::control::Shared;
use crate::debounce::Debounce;
use crate::slot::{Slot, Work};
use crate::stats::{Samples, describe};

/// The settings one session needs. Filled from the config file.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The virtual output's name, such as `padlink-ba601e`.
    pub output_name: String,
    pub scale: f64,
    pub placement: Placement,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub encoder: Backend,
    pub render_node: PathBuf,
    /// Draw the pointer into the video.
    pub cursor: bool,
    /// The compositor's Wayland socket.
    pub wayland_socket: PathBuf,
    /// Hyprland's event socket (`.socket2.sock`).
    pub hypr_events: PathBuf,
    /// The workspaces that were on the display when it last went away, to
    /// be moved back when it returns (R2.4). Outlives the session.
    pub workspaces: Arc<Mutex<Vec<String>>>,
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// The daemon is shutting down or the user disconnected.
    Stopped,
    /// The receiver locked or left the foreground: keep dialing.
    ReceiverSleeping,
    /// The receiver app quit: wait for it to come back.
    ReceiverClosed,
    /// The cable or the connection went away.
    Disconnected(String),
    /// Something padlink could not do. The text says what and what to try.
    Failed(String),
    /// Trying again would only repeat the problem. The daemon stays
    /// disconnected until the user runs `padlink connect`.
    GaveUp(String),
}

enum Event {
    Bytes(Vec<u8>),
    /// The socket closed or failed.
    Closed(String),
    Packet(Encoded),
    /// The capture or encode thread stopped on its own.
    PipelineFailed(String),
    /// Hyprland reloaded its config, which resets the output's settings.
    ConfigReloaded,
}

struct Encoded {
    data: Vec<u8>,
    keyframe: bool,
    captured_wall_ms: Option<f64>,
    /// Capture to encoded packet.
    capture_to_encoded_ms: Option<f64>,
    prepare_ms: f64,
    encode_ms: f64,
    encoded_at: Instant,
}

/// The output and the threads working on it. Built on `hello`, rebuilt on
/// rotation, torn down in a fixed order (R1.18).
struct Pipeline {
    /// What was asked of Hyprland, to put back after a config reload (R2.5).
    spec: OutputSpec,
    output: String,
    stop_capture: Arc<AtomicBool>,
    slot: Arc<Slot<Frame>>,
    capture: Option<JoinHandle<()>>,
    encode: Option<JoinHandle<()>>,
    injector: Option<Injector>,
    description: String,
}

impl Pipeline {
    /// Stop capture and wait for its objects to be destroyed, then remove
    /// the output. Never the reverse: Hyprland 0.56.2 aborts when an output
    /// is removed under a capture (SPEC.md section 8).
    fn teardown(mut self, hypr: &Hyprland, workspaces: &Mutex<Vec<String>>) {
        // R2.4: note which workspaces are here before they are moved away.
        if let Ok(here) = hypr.workspaces_on(&self.output) {
            *workspaces.lock().unwrap_or_else(|e| e.into_inner()) = here;
        }
        // Release any held button while the output still exists (R1.17).
        self.injector.take();
        self.stop_capture.store(true, Ordering::Relaxed);
        let capture_ended = match self.capture.take() {
            Some(handle) => handle.join().is_ok(),
            None => true,
        };
        self.slot.close();
        if let Some(handle) = self.encode.take() {
            let _ = handle.join();
        }
        if !capture_ended {
            // The capture thread panicked, so its objects may still exist.
            // Leaving the output is safer than crashing the compositor;
            // `padlink clean` or the next start removes it.
            error!(
                "capture did not shut down cleanly; leaving {} in place",
                self.output
            );
            return;
        }
        match hypr.remove_output(&self.output) {
            Ok(()) => info!("removed virtual display {}", self.output),
            Err(e) => warn!("{e}"),
        }
    }
}

fn wall_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// Live figures of a session, for the log now and `padlink status` later.
#[derive(Debug, Default)]
struct Figures {
    capture_to_encoded: Samples,
    encoded_to_sent: Samples,
    prepare: Samples,
    encode: Samples,
    frames: u64,
    bytes: u64,
    keyframes: u64,
    receiver: Option<LinkStats>,
}

pub struct Session<'a, R: Receiver> {
    receiver: R,
    config: &'a SessionConfig,
    hypr: &'a Hyprland,
    dialect: Dialect,
    writer: UnixStream,
    events: mpsc::Receiver<Event>,
    events_tx: mpsc::SyncSender<Event>,
    pipeline: Option<Pipeline>,
    started: Instant,
    figures: Figures,
    last_report: Instant,
    shared: Arc<Shared>,
    /// Frames and bytes since the status was last published.
    published: (Instant, u64, u64),
    /// When to put the output's settings back after a config reload.
    reapply_at: Option<Instant>,
    last_drift_check: Instant,
    /// When the output's settings were lost, to give up if it keeps happening.
    losses: Vec<Instant>,
    /// Cleared when the session ends, so helper threads stop.
    alive: Arc<AtomicBool>,
    /// A changed display request waiting out its debounce.
    rotation: Debounce<DisplayRequest>,
}

impl<'a, R: Receiver> Session<'a, R> {
    /// Take over a connected transport. Nothing is sent until the receiver
    /// speaks first.
    pub fn new(
        receiver: R,
        transport: UnixStream,
        config: &'a SessionConfig,
        hypr: &'a Hyprland,
        dialect: Dialect,
        shared: Arc<Shared>,
    ) -> std::io::Result<Self> {
        let writer = transport.try_clone()?;
        // A write that cannot make progress for this long means the link is
        // gone, not slow.
        writer.set_write_timeout(Some(Duration::from_secs(5)))?;
        // Room for one event: a stalled link backs up into the capture slot,
        // where frames are replaced, instead of into a queue of packets.
        let (events_tx, events) = mpsc::sync_channel(1);
        spawn_reader(transport, events_tx.clone());
        let alive = Arc::new(AtomicBool::new(true));
        spawn_reload_watch(&config.hypr_events, events_tx.clone(), alive.clone());
        Ok(Session {
            receiver,
            config,
            hypr,
            dialect,
            writer,
            events,
            events_tx,
            pipeline: None,
            started: Instant::now(),
            figures: Figures::default(),
            last_report: Instant::now(),
            shared,
            published: (Instant::now(), 0, 0),
            reapply_at: None,
            last_drift_check: Instant::now(),
            losses: Vec::new(),
            alive,
            rotation: Debounce::default(),
        })
    }

    fn now(&self) -> Now {
        Now {
            mono: self.started.elapsed(),
            wall_ms: wall_ms(),
        }
    }

    /// Run until the session ends, then tear everything down.
    ///
    /// `stop` is asked several times a second; when it says yes the session
    /// ends with [`SessionEnd::Stopped`].
    pub fn run(mut self, stop: impl Fn() -> bool) -> SessionEnd {
        self.receiver.connected(self.now());
        self.shared.status().state = State::Handshake;
        let mut last_tick = Instant::now();
        let end = loop {
            if stop() {
                break SessionEnd::Stopped;
            }
            let outcome = match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(Event::Bytes(bytes)) => {
                    let actions = self.receiver.received(self.now(), &bytes);
                    self.perform(actions)
                }
                Ok(Event::Packet(packet)) => self.send_packet(packet),
                Ok(Event::Closed(why)) => Some(SessionEnd::Disconnected(why)),
                Ok(Event::PipelineFailed(why)) => Some(SessionEnd::Failed(why)),
                Ok(Event::ConfigReloaded) => {
                    // R2.5: give Hyprland 200 ms to finish applying its own
                    // rules, then put ours back.
                    self.reapply_at = Some(Instant::now() + Duration::from_millis(200));
                    None
                }
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    Some(SessionEnd::Disconnected("the reader stopped".into()))
                }
            };
            if let Some(end) = outcome {
                break end;
            }
            if let Some(display) = self.rotation.take_due(Instant::now()) {
                if let Some(end) = self.rebuild(&display) {
                    break end;
                }
            }
            // Also compare the live settings every two seconds, in case
            // something changed them without a reload event.
            let drifted = self.last_drift_check.elapsed() >= Duration::from_secs(2) && {
                self.last_drift_check = Instant::now();
                self.output_drifted()
            };
            if drifted || self.reapply_at.is_some_and(|at| Instant::now() >= at) {
                self.reapply_at = None;
                if let Some(end) = self.reapply() {
                    break end;
                }
            }
            if last_tick.elapsed() >= Duration::from_millis(250) {
                last_tick = Instant::now();
                let actions = self.receiver.tick(self.now());
                if let Some(end) = self.perform(actions) {
                    break end;
                }
                self.report();
                self.publish();
            }
        };
        info!("session ended: {end:?}");
        self.alive.store(false, Ordering::Relaxed);
        self.receiver.disconnected();
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.teardown(self.hypr, &self.config.workspaces);
        }
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        end
    }

    fn send(&mut self, bytes: &[u8]) -> Option<SessionEnd> {
        self.writer.write_all(bytes).err().map(|e| {
            SessionEnd::Disconnected(format!("could not send to the iPad ({e}). Check the cable"))
        })
    }

    fn perform(&mut self, actions: Vec<Action>) -> Option<SessionEnd> {
        for action in actions {
            let end = match action {
                Action::Send(bytes) => self.send(&bytes),
                // The first display is built at once; a change to an
                // existing one (rotation) is debounced (R2.7).
                Action::Display(display) if self.pipeline.is_none() => self.rebuild(&display),
                Action::Display(display) => {
                    let same_size = self.pipeline.as_ref().is_some_and(|p| {
                        (p.spec.width, p.spec.height) == (display.width, display.height)
                    });
                    if same_size {
                        // A repeated `hello` for another reason, such as a
                        // changed address list: nothing to rebuild.
                        debug!("the receiver repeated its hello with the same size");
                    } else {
                        self.rotation.push(display, Instant::now());
                    }
                    None
                }
                Action::Keyframe => {
                    if let Some(p) = &self.pipeline {
                        p.slot.request_keyframe();
                    }
                    None
                }
                Action::Pointer(pointer) => {
                    if let Some(injector) = self.pipeline.as_ref().and_then(|p| p.injector.as_ref())
                    {
                        injector.pointer(pointer);
                    }
                    None
                }
                Action::Scroll { dx, dy } => {
                    if let Some(injector) = self.pipeline.as_ref().and_then(|p| p.injector.as_ref())
                    {
                        injector.scroll(dx, dy);
                    }
                    None
                }
                Action::Stats(stats) => {
                    self.figures.receiver = Some(stats);
                    None
                }
                Action::Sleeping => Some(SessionEnd::ReceiverSleeping),
                Action::Closing => Some(SessionEnd::ReceiverClosed),
                Action::Close(why) => Some(SessionEnd::Disconnected(why)),
                Action::Warn(text) => {
                    warn!("{text}");
                    None
                }
            };
            if end.is_some() {
                return end;
            }
        }
        None
    }

    fn send_packet(&mut self, packet: Encoded) -> Option<SessionEnd> {
        let actions = self.receiver.video_frame(
            self.now(),
            &packet.data,
            packet.keyframe,
            packet.captured_wall_ms,
        );
        let sent_something = !actions.is_empty();
        let end = self.perform(actions);
        if sent_something {
            let f = &mut self.figures;
            f.frames += 1;
            f.bytes += packet.data.len() as u64;
            f.keyframes += u64::from(packet.keyframe);
            self.published.1 += 1;
            self.published.2 += packet.data.len() as u64;
            f.prepare.add(packet.prepare_ms);
            f.encode.add(packet.encode_ms);
            f.encoded_to_sent
                .add(packet.encoded_at.elapsed().as_secs_f64() * 1000.0);
            if let Some(ms) = packet.capture_to_encoded_ms {
                f.capture_to_encoded.add(ms);
            }
        }
        end
    }

    /// Whether the output no longer has the mode, scale and position asked for.
    fn output_drifted(&self) -> bool {
        let Some(pipeline) = &self.pipeline else {
            return false;
        };
        match self.hypr.monitor(&pipeline.output) {
            Ok(Some(m)) => {
                let s = &pipeline.spec;
                (m.width, m.height, m.x, m.y) != (s.width, s.height, s.x, s.y)
                    || (m.scale - s.scale).abs() > 1e-3
            }
            // Gone or unreadable: the capture thread reports a removed
            // output, and a dead compositor ends the session elsewhere.
            _ => false,
        }
    }

    /// R2.5: put the output's mode, scale and position back. Three losses
    /// within a minute mean something keeps overriding them: give up and say
    /// what to look for.
    fn reapply(&mut self) -> Option<SessionEnd> {
        let pipeline = self.pipeline.as_ref()?;
        if !self.output_drifted() {
            return None;
        }
        let now = Instant::now();
        self.losses
            .retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        self.losses.push(now);
        if self.losses.len() >= 3 {
            return Some(SessionEnd::GaveUp(format!(
                "Hyprland changed the settings of {} three times within a minute. \
                 A monitor rule in your Hyprland config probably applies to it (one \
                 with an empty or wildcard output name); make that rule skip `padlink-*`, then run `padlink connect`",
                pipeline.output
            )));
        }
        warn!(
            "Hyprland reset the settings of {}; putting them back",
            pipeline.output
        );
        match self.hypr.apply(self.dialect, &pipeline.spec) {
            Ok(_) => {
                // The receiver saw frames stop; start again from a keyframe.
                pipeline.slot.request_keyframe();
                None
            }
            Err(e) => Some(SessionEnd::Failed(e.to_string())),
        }
    }

    /// Refresh the figures `padlink status` shows, once a second (R1.22).
    fn publish(&mut self) {
        let (since, frames, bytes) = self.published;
        let secs = since.elapsed().as_secs_f64();
        if secs < 1.0 || self.pipeline.is_none() {
            return;
        }
        self.published = (Instant::now(), 0, 0);
        let pair = |s: &Samples| {
            Some(Latency {
                p50_ms: s.percentile(0.5)?,
                p95_ms: s.percentile(0.95)?,
            })
        };
        let f = &self.figures;
        let mut status = self.shared.status();
        status.fps = Some((frames as f64 / secs * 10.0).round() / 10.0);
        status.bitrate_mbps = Some((bytes as f64 * 8.0 / secs / 1e5).round() / 10.0);
        status.capture_to_encoded = pair(&f.capture_to_encoded);
        status.encoded_to_sent = pair(&f.encoded_to_sent);
        status.end_to_end = f.receiver.as_ref().and_then(|r| {
            Some(Latency {
                p50_ms: r.e2e_p50_ms?,
                p95_ms: r.e2e_p95_ms?,
            })
        });
        status.frames_replaced = self.pipeline.as_ref().map_or(0, |p| p.slot.dropped());
    }

    /// One line every five seconds with the per-stage numbers (section 9).
    fn report(&mut self) {
        let elapsed = self.last_report.elapsed();
        if elapsed < Duration::from_secs(5) {
            return;
        }
        self.last_report = Instant::now();
        let f = &mut self.figures;
        let secs = elapsed.as_secs_f64();
        let dropped = self.pipeline.as_ref().map_or(0, |p| p.slot.dropped());
        info!(
            "sent {:.1} fps, {:.1} Mbit/s, {} keyframes | capture to encoded {} \
             (prepare {}, codec {}) | encoded to sent {} | {dropped} frames replaced before encoding",
            f.frames as f64 / secs,
            f.bytes as f64 * 8.0 / secs / 1e6,
            f.keyframes,
            describe(&f.capture_to_encoded),
            describe(&f.prepare),
            describe(&f.encode),
            describe(&f.encoded_to_sent),
        );
        if let Some(r) = &f.receiver {
            let n = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.0}"));
            info!(
                "iPad reports: end to end p50 {} / p95 {} ms, {} fps, {} Mbit/s, {} stalls, over {}",
                n(r.e2e_p50_ms),
                n(r.e2e_p95_ms),
                n(r.fps),
                r.mbps.map_or("-".to_string(), |v| format!("{v:.1}")),
                n(r.stalls),
                r.transport.as_deref().unwrap_or("?"),
            );
        }
        (f.frames, f.bytes, f.keyframes) = (0, 0, 0);
    }

    /// Create the display for `request`, or rebuild it after a rotation.
    fn rebuild(&mut self, request: &DisplayRequest) -> Option<SessionEnd> {
        if let Some(old) = self.pipeline.take() {
            info!("the receiver changed its display; rebuilding");
            old.teardown(self.hypr, &self.config.workspaces);
        }
        match self.build(request) {
            Ok(pipeline) => {
                info!("{}", pipeline.description);
                self.pipeline = Some(pipeline);
                None
            }
            Err(why) => Some(SessionEnd::Failed(why)),
        }
    }

    fn build(&mut self, request: &DisplayRequest) -> Result<Pipeline, String> {
        let config = self.config;
        let plan = plan_stream(request, Codec::H264, config.fps).map_err(|e| e.to_string())?;
        let logical =
            logical_size(plan.width, plan.height, config.scale).map_err(|e| e.to_string())?;
        let others = self.hypr.other_monitor_rects().map_err(|e| e.to_string())?;
        let (x, y) = place(config.placement, &others, logical);
        let spec = OutputSpec {
            name: config.output_name.clone(),
            width: plan.width,
            height: plan.height,
            hz: plan.fps,
            scale: config.scale,
            x,
            y,
        };

        // Open the encoder before touching the desktop: if no encoder
        // works, no output should appear and disappear.
        let opened = Encoder::open(&EncoderSettings {
            width: plan.width,
            height: plan.height,
            fps: plan.fps,
            bitrate_bps: config.bitrate_bps,
            backend: config.encoder,
            render_node: config.render_node.clone(),
            codec_options: Vec::new(),
        })
        .map_err(|e| e.to_string())?;
        for (kind, why) in &opened.skipped {
            warn!("not using {}: {why}", kind.label());
        }
        let encoder = opened.encoder;
        let encoder_label = encoder.kind().label().to_string();
        info!("encoder: {}", encoder.describe());

        // R1.11: creating an output takes focus; hand it back.
        let focus = self.hypr.focus().map_err(|e| e.to_string())?;
        let _ = self.hypr.remove_output(&spec.name);
        let monitor = self
            .hypr
            .create_output(self.dialect, &spec)
            .map_err(|e| e.to_string())?;
        // R2.4: bring back the workspaces that were here last time. Empty
        // ones were deleted by Hyprland in the meantime and are skipped.
        let remembered = self
            .config
            .workspaces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for workspace in &remembered {
            match self
                .hypr
                .move_workspace(self.dialect, workspace, &spec.name)
            {
                Ok(true) => info!("moved workspace {workspace} back to {}", spec.name),
                Ok(false) => {}
                Err(e) => warn!("{e}"),
            }
        }
        if let Err(e) = self.hypr.restore_focus(self.dialect, &focus) {
            warn!("could not put focus back where it was: {e}");
        }

        let description = format!(
            "extending onto {}: {}x{} at {} Hz, scale {} ({}x{} logical) at {}x{} as {}",
            request.device_name.as_deref().unwrap_or("the receiver"),
            monitor.width,
            monitor.height,
            plan.fps,
            monitor.scale,
            logical.0,
            logical.1,
            monitor.x,
            monitor.y,
            spec.name
        );

        let injector = match Injector::start(Target {
            wayland_socket: config.wayland_socket.clone(),
            output: spec.name.clone(),
            width: plan.width,
            height: plan.height,
            scale: config.scale,
        }) {
            Ok(injector) => Some(injector),
            Err(e) => {
                warn!("touch input is off: {e}");
                None
            }
        };

        let slot = Arc::new(Slot::default());
        let stop_capture = Arc::new(AtomicBool::new(false));
        let encode = spawn_encoder(
            encoder,
            (plan.width, plan.height),
            slot.clone(),
            self.events_tx.clone(),
        );
        let capture = spawn_capture(
            CaptureOptions {
                wayland_socket: config.wayland_socket.clone(),
                output: spec.name.clone(),
                cursor: config.cursor,
            },
            stop_capture.clone(),
            slot.clone(),
            self.events_tx.clone(),
            self.shared.clone(),
        );
        {
            let mut status = self.shared.status();
            status.state = State::Streaming;
            status.message = None;
            status.device = request.device_name.clone();
            status.transport = Some("USB".into());
            status.output = Some(spec.name.clone());
            status.encoder = Some(encoder_label);
            status.resolution = Some(format!("{}x{}", plan.width, plan.height));
        }

        let pipeline = Pipeline {
            output: spec.name.clone(),
            spec,
            stop_capture,
            slot,
            capture: Some(capture),
            encode: Some(encode),
            injector,
            description,
        };

        // R1.6: announce the stream before the first frame.
        let actions = self.receiver.announce_stream(&plan);
        if let Some(end) = self.perform(actions) {
            pipeline.teardown(self.hypr, &self.config.workspaces);
            return Err(format!("{end:?}"));
        }
        self.figures = Figures::default();
        Ok(pipeline)
    }
}

fn spawn_reader(mut transport: UnixStream, events: mpsc::SyncSender<Event>) {
    std::thread::Builder::new()
        .name("padlink-read".into())
        .spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let event = match transport.read(&mut buf) {
                    Ok(0) => Event::Closed("the iPad closed the connection".into()),
                    Ok(n) => Event::Bytes(buf[..n].to_vec()),
                    Err(e) => Event::Closed(format!("the connection to the iPad broke ({e})")),
                };
                let closed = matches!(event, Event::Closed(_));
                if events.send(event).is_err() || closed {
                    break;
                }
            }
        })
        .expect("spawn reader thread");
}

fn spawn_capture(
    options: CaptureOptions,
    stop: Arc<AtomicBool>,
    slot: Arc<Slot<Frame>>,
    events: mpsc::SyncSender<Event>,
    shared: Arc<Shared>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("padlink-capture".into())
        .spawn(move || {
            let mut first = true;
            let result = padlink_capture::capture(
                &options,
                &stop,
                |path| {
                    info!("capture: {}", path.label());
                    shared.status().capture = Some(path.label().to_string());
                },
                |frame| {
                    if std::mem::take(&mut first)
                        && padlink_capture::looks_black(
                            &frame.pixels,
                            frame.width,
                            frame.height,
                            frame.stride,
                        )
                    {
                        // Normal for an empty output, but also what
                        // Hyprland sends when capture is not permitted.
                        debug!("the first captured frame is entirely black");
                    }
                    slot.put(frame);
                },
            );
            match result {
                Ok(padlink_capture::End::Stopped) => {}
                Ok(padlink_capture::End::OutputRemoved) => {
                    let _ = events.try_send(Event::PipelineFailed(
                        "the virtual display was removed from outside padlink".into(),
                    ));
                }
                Err(e) => {
                    let _ = events.try_send(Event::PipelineFailed(e.to_string()));
                }
            }
        })
        .expect("spawn capture thread")
}

/// Watch Hyprland's event socket for config reloads (R2.5).
fn spawn_reload_watch(socket: &Path, events: mpsc::SyncSender<Event>, alive: Arc<AtomicBool>) {
    let Ok(stream) = UnixStream::connect(socket) else {
        warn!(
            "cannot watch Hyprland's events at {}; a config reload will be noticed within two seconds instead",
            socket.display()
        );
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = std::thread::Builder::new()
        .name("padlink-hypr-events".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            while alive.load(Ordering::Relaxed) {
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.starts_with("configreloaded>>") {
                            let _ = events.try_send(Event::ConfigReloaded);
                        }
                        line.clear();
                    }
                    // The read timed out, possibly mid-line: keep what was
                    // read and go round to check `alive`.
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => break,
                }
            }
        });
}

fn spawn_encoder(
    mut encoder: Encoder,
    size: (u32, u32),
    slot: Arc<Slot<Frame>>,
    events: mpsc::SyncSender<Event>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("padlink-encode".into())
        .spawn(move || {
            loop {
                let (result, captured) = match slot.take() {
                    Work::Closed => break,
                    Work::Frame { item, keyframe } => {
                        if (item.width, item.height) != size {
                            // The output briefly has another mode, as after
                            // a config reload. The session puts it back.
                            debug!(
                                "skipping a {}x{} frame while the output is not {}x{}",
                                item.width, item.height, size.0, size.1
                            );
                            if keyframe {
                                slot.defer_keyframe();
                            }
                            continue;
                        }
                        let result =
                            encoder.encode_bgrx(&item.pixels, item.stride as usize, keyframe);
                        // The lease on the capture buffer ends here.
                        (result, Some((item.captured_at, item.captured_wall_ms)))
                    }
                    Work::RepeatAsKeyframe => match encoder.repeat_as_keyframe() {
                        Err(padlink_encode::Error::NothingToRepeat) => {
                            slot.defer_keyframe();
                            // Wait for the first frame without spinning.
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        result => (result, None),
                    },
                };
                let packet = match result {
                    Ok(Some(packet)) => packet,
                    Ok(None) => continue,
                    Err(e) => {
                        let _ = events.try_send(Event::PipelineFailed(e.to_string()));
                        break;
                    }
                };
                let timings = encoder.timings();
                let encoded = Encoded {
                    data: packet.data,
                    keyframe: packet.keyframe,
                    captured_wall_ms: captured.map(|c| c.1),
                    capture_to_encoded_ms: captured.map(|c| c.0.elapsed().as_secs_f64() * 1000.0),
                    prepare_ms: timings.prepare_ms,
                    encode_ms: timings.encode_ms,
                    encoded_at: Instant::now(),
                };
                // Hand the packet over, but never block for ever: during
                // teardown the session thread is waiting for this thread and
                // no longer reads the channel.
                let mut event = Event::Packet(encoded);
                loop {
                    match events.try_send(event) {
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                        Err(mpsc::TrySendError::Full(back)) => {
                            if slot.is_closed() {
                                return;
                            }
                            event = back;
                            std::thread::sleep(Duration::from_micros(500));
                        }
                    }
                }
            }
        })
        .expect("spawn encoder thread")
}
