//! Capture against a real Hyprland: the nested test bench.
//!
//! Creates and removes outputs, so it never runs by default and refuses the
//! login session (SPEC.md rule 2). Run with `testbench/run-nested-tests.sh`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use padlink_capture::{End, Error, Options, Path, Pixels, capture, looks_black};
use padlink_hypr::{Hyprland, OutputSpec, Placement, instance, logical_size, output_name, place};

struct Bench {
    hypr: Hyprland,
    wayland_socket: PathBuf,
}

fn bench() -> Bench {
    assert_eq!(
        std::env::var("PADLINK_TESTBENCH").as_deref(),
        Ok("1"),
        "not inside the test bench: run testbench/run-nested-tests.sh"
    );
    let runtime = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap());
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap();
    let target = instance::find(&runtime, Some(&signature)).unwrap();
    let login = instance::find(&runtime, None).unwrap();
    assert_ne!(
        target.signature, login.signature,
        "refusing to create outputs on the login session"
    );
    Bench {
        hypr: Hyprland::new(target.request_socket()),
        wayland_socket: runtime.join(&target.wl_socket),
    }
}

/// A 2360x1640 scale 2 output below the others, removed first if left over.
fn make_output(b: &Bench, name: &str) -> OutputSpec {
    let size = logical_size(2360, 1640, 2.0).unwrap();
    let (x, y) = place(
        Placement::Below,
        &b.hypr.other_monitor_rects().unwrap(),
        size,
    );
    let spec = OutputSpec {
        name: output_name(name),
        width: 2360,
        height: 1640,
        hz: 60,
        scale: 2.0,
        x,
        y,
    };
    let _ = b.hypr.remove_output(&spec.name);
    let dialect = b.hypr.dialect().unwrap();
    let focus = b.hypr.focus().unwrap();
    b.hypr.create_output(dialect, &spec).unwrap();
    b.hypr.restore_focus(dialect, &focus).unwrap();
    spec
}

/// Move the pointer around inside the output: each move is damage.
fn wiggle_pointer(b: &Bench, spec: &OutputSpec, moves: u32) {
    for i in 0..moves {
        let (x, y) = (
            spec.x + 100 + (i as i32 * 37) % 800,
            spec.y + 100 + (i as i32 * 23) % 500,
        );
        let reply = b
            .hypr
            .request(&format!(
                "dispatch hl.dsp.cursor.move({{ x = {x}, y = {y} }})"
            ))
            .unwrap();
        assert_eq!(reply.trim(), "ok");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Facts about one frame, sent out of the capture thread.
struct Seen {
    size: (u32, u32, u32),
    bytes: usize,
    black: bool,
    at: Instant,
}

/// R1.12 (shared-memory path), R1.18 and the damage rule of section 4.
#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn frames_follow_damage_and_teardown_comes_before_output_removal() {
    frames_follow_damage("cap01", None, Path::WlrShm);
}

/// R1.12, the zero-copy path: the same behaviour with frames left on the GPU.
#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn zero_copy_frames_follow_damage_too() {
    frames_follow_damage("cap02", Some("/dev/dri/renderD128".into()), Path::WlrDmabuf);
}

fn frames_follow_damage(name: &str, gpu_buffers: Option<PathBuf>, expected: Path) {
    let b = bench();
    let spec = make_output(&b, name);
    let options = Options {
        wayland_socket: b.wayland_socket.clone(),
        output: spec.name.clone(),
        cursor: true,
        gpu_buffers,
    };

    let stop = AtomicBool::new(false);
    let (frames_tx, frames) = mpsc::channel::<Seen>();
    let (path_tx, path) = mpsc::channel::<Path>();

    let end = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            capture(
                &options,
                &stop,
                |p| path_tx.send(p).unwrap(),
                |frame| {
                    let (bytes, black) = match &frame.pixels {
                        Pixels::Cpu(pixels) => (
                            pixels.len(),
                            looks_black(pixels, frame.width, frame.height, frame.stride),
                        ),
                        // Nothing to read on the CPU: that is the point.
                        Pixels::Gpu(buffer) => {
                            assert_eq!(buffer.stride, frame.stride);
                            (frame.stride as usize * frame.height as usize, false)
                        }
                    };
                    let _ = frames_tx.send(Seen {
                        size: (frame.width, frame.height, frame.stride),
                        bytes,
                        black,
                        at: frame.captured_at,
                    });
                },
            )
        });

        assert_eq!(path.recv_timeout(Duration::from_secs(5)).unwrap(), expected);

        // The first frame arrives without any damage.
        let first = frames
            .recv_timeout(Duration::from_secs(5))
            .expect("a first frame");
        assert_eq!((first.size.0, first.size.1), (2360, 1640));
        assert!(first.size.2 >= 2360 * 4);
        assert_eq!(first.bytes, first.size.2 as usize * 1640);
        eprintln!("first frame looks black: {}", first.black);

        // Let anything from the output's creation settle, then: a static
        // screen sends nothing.
        while frames.recv_timeout(Duration::from_millis(300)).is_ok() {}
        assert!(
            frames.recv_timeout(Duration::from_millis(700)).is_err(),
            "a frame arrived from a static screen"
        );

        // Damage brings frames, and promptly.
        let started = Instant::now();
        wiggle_pointer(&b, &spec, 30);
        let mut seen = Vec::new();
        while let Ok(f) = frames.recv_timeout(Duration::from_millis(300)) {
            seen.push(f);
        }
        assert!(
            seen.len() >= 5,
            "only {} frames for 30 pointer moves",
            seen.len()
        );
        assert!(
            seen.len() <= 60,
            "{} frames for 30 moves: not damage-driven",
            seen.len()
        );
        assert!(seen[0].at.duration_since(started) < Duration::from_millis(500));
        // The pointer was drawn into the picture, so it is not all black.
        assert!(seen.iter().any(|f| !f.black) || !first.black);

        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap()
    });
    assert_eq!(end.unwrap(), End::Stopped);

    // Only now, with the capture objects gone, is the output removed; the
    // compositor must still be alive afterwards (SPEC.md section 8).
    b.hypr.remove_output(&spec.name).unwrap();
    assert!(
        b.hypr.version().is_ok(),
        "the compositor died on output removal"
    );
    assert!(b.hypr.monitor(&spec.name).unwrap().is_none());
}

#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn an_unknown_output_is_an_error_that_lists_the_real_ones() {
    let b = bench();
    let options = Options {
        wayland_socket: b.wayland_socket.clone(),
        output: "padlink-nosuch".into(),
        cursor: false,
        gpu_buffers: None,
    };
    let err = capture(&options, &AtomicBool::new(false), |_| {}, |_| {}).unwrap_err();
    assert!(matches!(err, Error::NoOutput { .. }), "{err}");
    assert!(err.to_string().contains("WAYLAND-1"), "{err}");
}

#[test]
fn a_missing_compositor_socket_is_reported_with_its_path() {
    let options = Options {
        wayland_socket: "/nonexistent/wayland-99".into(),
        output: "x".into(),
        cursor: false,
        gpu_buffers: None,
    };
    let err = capture(&options, &AtomicBool::new(false), |_| {}, |_| {}).unwrap_err();
    assert!(err.to_string().contains("/nonexistent/wayland-99"), "{err}");
}
