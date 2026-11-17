//! The output lifecycle against a real Hyprland: the nested test bench.
//!
//! These tests create and remove outputs, so they never run by default and
//! refuse to run against the login session (SPEC.md rule 2). Run them with:
//!
//!     testbench/nested.sh                 # in one terminal
//!     testbench/run-nested-tests.sh       # in another

use std::path::PathBuf;

use padlink_hypr::{
    Dialect, Error, Hyprland, OutputSpec, Placement, instance, logical_size, output_name, place,
};

/// The nested instance, or a panic that says how to start one.
fn nested() -> Hyprland {
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
    Hyprland::new(target.request_socket())
}

fn spec(hypr: &Hyprland, name: &str, placement: Placement) -> OutputSpec {
    let size = logical_size(2360, 1640, 2.0).unwrap();
    let (x, y) = place(placement, &hypr.other_monitor_rects().unwrap(), size);
    OutputSpec {
        name: output_name(name),
        width: 2360,
        height: 1640,
        hz: 60,
        scale: 2.0,
        x,
        y,
    }
}

#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn the_bench_is_detected_as_a_lua_hyprland() {
    let hypr = nested();
    assert_eq!(hypr.dialect().unwrap(), Dialect::Lua);
    let version = hypr.version().unwrap();
    assert!((version.major, version.minor) >= (0, 55), "{version}");
    assert!(!hypr.enforces_permissions().unwrap());
}

/// R1.7, R1.8, R1.11, R1.18 (the removal half), R1.20.
#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn an_output_is_created_read_back_moved_and_removed_and_focus_stays_put() {
    let hypr = nested();
    let dialect = hypr.dialect().unwrap();
    let spec = spec(&hypr, "life01", Placement::Right);
    let _ = hypr.remove_output(&spec.name);

    let before = hypr.focus().unwrap();
    assert!(before.monitor.is_some());

    let monitor = hypr.create_output(dialect, &spec).unwrap();
    assert_eq!(
        (monitor.width, monitor.height, monitor.scale),
        (2360, 1640, 2.0)
    );
    assert_eq!((monitor.x, monitor.y), (spec.x, spec.y));
    assert_eq!(monitor.rect().w, 1180);
    assert!(hypr.leftover_outputs().unwrap().contains(&spec.name));

    // Creating the output takes focus; it must be handed back.
    hypr.restore_focus(dialect, &before).unwrap();
    assert_eq!(hypr.focus().unwrap().monitor, before.monitor);

    // Re-applying with a new position, as after a config reload.
    let moved = OutputSpec {
        x: spec.x + 100,
        y: 40,
        ..spec.clone()
    };
    let monitor = hypr.apply(dialect, &moved).unwrap();
    assert_eq!((monitor.x, monitor.y), (moved.x, 40));

    hypr.remove_output(&spec.name).unwrap();
    assert!(hypr.monitor(&spec.name).unwrap().is_none());
    assert!(!hypr.leftover_outputs().unwrap().contains(&spec.name));
    // Removing it again is not an error.
    hypr.remove_output(&spec.name).unwrap();
}

#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn a_name_that_is_taken_is_refused_and_the_existing_output_survives() {
    let hypr = nested();
    let dialect = hypr.dialect().unwrap();
    let spec = spec(&hypr, "dup01", Placement::Below);
    let _ = hypr.remove_output(&spec.name);

    hypr.create_output(dialect, &spec).unwrap();
    let err = hypr.create_output(dialect, &spec).unwrap_err();
    assert!(matches!(err, Error::Refused { .. }), "{err}");
    assert!(err.to_string().contains("Name already taken"), "{err}");
    assert!(hypr.monitor(&spec.name).unwrap().is_some());

    hypr.remove_output(&spec.name).unwrap();
}

/// A mode Hyprland will not take is reported, and nothing is left behind.
#[test]
#[ignore = "needs the nested Hyprland test bench"]
fn settings_that_do_not_stick_are_reported_and_cleaned_up() {
    let hypr = nested();
    let dialect = hypr.dialect().unwrap();
    // Scale 3 leaves fractional pixels at this size; padlink rejects it
    // before asking, but Hyprland's own answer is to pick another scale.
    let bad = OutputSpec {
        scale: 3.0,
        ..spec(&hypr, "bad01", Placement::Left)
    };
    let _ = hypr.remove_output(&bad.name);

    let err = hypr.create_output(dialect, &bad).unwrap_err();
    assert!(matches!(err, Error::NotApplied { .. }), "{err}");
    assert!(
        hypr.monitor(&bad.name).unwrap().is_none(),
        "output left behind"
    );
}
