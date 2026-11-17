//! Input injection against a real Hyprland: the nested test bench.
//! Run with `testbench/run-nested-tests.sh`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use padlink_hypr::{Hyprland, OutputSpec, Placement, instance, logical_size, output_name, place};
use padlink_input::{Error, Injector, Target};
use padlink_proto::{Pointer, PointerPhase, PointerSource};

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

fn cursor(hypr: &Hyprland) -> (i64, i64) {
    let reply = hypr.request("j/cursorpos").unwrap();
    let num = |key: &str| {
        let rest = &reply[reply.find(key).unwrap() + key.len()..];
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit() && *c != '-')
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        digits.parse::<i64>().unwrap()
    };
    (num("\"x\""), num("\"y\""))
}

/// Poll until the pointer is at `want` (within a pixel), or fail.
fn wait_for_cursor(hypr: &Hyprland, want: (i64, i64)) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let got = cursor(hypr);
        if (got.0 - want.0).abs() <= 1 && (got.1 - want.1).abs() <= 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "pointer is at {got:?}, wanted {want:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn touch(phase: PointerPhase, x: f64, y: f64) -> Pointer {
    Pointer {
        phase,
        source: PointerSource::Finger,
        x,
        y,
    }
}

/// R1.16: a touch lands where the finger is, on the virtual output.
#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn touches_land_on_the_matching_spot_of_the_output() {
    let b = bench();
    let (lw, lh) = logical_size(2360, 1640, 2.0).unwrap();
    let (x, y) = place(
        Placement::Below,
        &b.hypr.other_monitor_rects().unwrap(),
        (lw, lh),
    );
    let spec = OutputSpec {
        name: output_name("inp01"),
        width: 2360,
        height: 1640,
        hz: 60,
        scale: 2.0,
        x,
        y,
    };
    let _ = b.hypr.remove_output(&spec.name);
    let dialect = b.hypr.dialect().unwrap();
    b.hypr.create_output(dialect, &spec).unwrap();
    // The output may have shifted others; read where it really is.
    let at = b.hypr.monitor(&spec.name).unwrap().unwrap();
    let (ox, oy) = (i64::from(at.x), i64::from(at.y));

    let injector = Injector::start(Target {
        wayland_socket: b.wayland_socket.clone(),
        output: spec.name.clone(),
        width: 2360,
        height: 1640,
        scale: 2.0,
    })
    .unwrap();

    // The centre of the picture is the centre of the output (logical units).
    injector.pointer(touch(PointerPhase::Down, 0.5, 0.5));
    wait_for_cursor(&b.hypr, (ox + 590, oy + 410));
    // A drag to three quarters across, one quarter down.
    injector.pointer(touch(PointerPhase::Move, 0.75, 0.25));
    wait_for_cursor(&b.hypr, (ox + 885, oy + 205));
    injector.pointer(touch(PointerPhase::Up, 0.75, 0.25));
    // A stylus hovering near the top left corner.
    injector.pointer(Pointer {
        phase: PointerPhase::Hover,
        source: PointerSource::Stylus,
        x: 0.1,
        y: 0.1,
    });
    wait_for_cursor(&b.hypr, (ox + 118, oy + 82));
    injector.scroll(0.0, 40.0);

    // Dropping it mid-press must not hang (R1.17).
    injector.pointer(touch(PointerPhase::Down, 0.2, 0.2));
    let started = Instant::now();
    drop(injector);
    assert!(started.elapsed() < Duration::from_secs(2));

    b.hypr.remove_output(&spec.name).unwrap();
    assert!(b.hypr.version().is_ok());
}

#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn an_unknown_output_is_refused() {
    let b = bench();
    let err = Injector::start(Target {
        wayland_socket: b.wayland_socket.clone(),
        output: "padlink-nosuch".into(),
        width: 100,
        height: 100,
        scale: 1.0,
    })
    .err()
    .unwrap();
    assert!(matches!(err, Error::NoOutput(_)), "{err}");
}

#[test]
fn a_missing_compositor_socket_is_reported_with_its_path() {
    let err = Injector::start(Target {
        wayland_socket: "/nonexistent/wayland-99".into(),
        output: "x".into(),
        width: 100,
        height: 100,
        scale: 1.0,
    })
    .err()
    .unwrap();
    assert!(err.to_string().contains("/nonexistent/wayland-99"), "{err}");
}
