//! Hyprland's request socket: one command per connection, a text reply.
//!
//! Every change is read back from `monitors` instead of trusting the reply
//! (R1.7): under a Lua config `eval` answers `ok` to anything that parses.
//!
//! Adapted from OpenDisplay's `od-sender/src/backend/hyprland/ipc.rs` and
//! `workspace.rs` (GPL-3.0).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::layout::Rect;

/// Every output padlink creates starts with this, which is how leftovers
/// from a crash are recognised (R1.20).
pub const OUTPUT_PREFIX: &str = "padlink-";

/// The output name for a device, from its short id (R1.7). An explicit name
/// is always passed: Hyprland's own `HEADLESS-n` names never repeat.
pub fn output_name(short_id: &str) -> String {
    let id: String = short_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    format!(
        "{OUTPUT_PREFIX}{}",
        if id.is_empty() { "ipad" } else { &id }
    )
}

/// How long Hyprland gets to apply a monitor rule before it counts as lost.
const SETTLE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(25);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "Hyprland's control socket {path} is not reachable ({source}). \
         Is Hyprland still running in this session?"
    )]
    Unreachable {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Hyprland refused to {action}: {reply}")]
    Refused { action: String, reply: String },

    #[error(
        "Hyprland did not apply the settings for {name}: asked for {wanted}, it has {got}. \
         Another monitor rule in the Hyprland config may be overriding it"
    )]
    NotApplied {
        name: String,
        wanted: String,
        got: String,
    },

    #[error("Hyprland answered `{command}` with something unexpected: {detail}")]
    BadReply { command: String, detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which config language the running Hyprland uses. It decides how monitor
/// rules and dispatchers are written (R1.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `hyprland.lua`: rules go through `eval`, `keyword` is refused.
    Lua,
    /// `hyprland.conf`: rules go through `keyword`.
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `0.56.2`, `v0.57.0-b`, `0.55`.
pub fn parse_version(s: &str) -> Option<Version> {
    let mut parts = s.trim().trim_start_matches('v').split(['.', '-', '+']);
    let mut next = || parts.next()?.parse().ok();
    Some(Version {
        major: next()?,
        minor: next()?,
        patch: next().unwrap_or(0),
    })
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(rename = "refreshRate")]
    pub refresh_rate: f64,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    #[serde(default)]
    pub transform: u32,
    #[serde(default)]
    pub focused: bool,
    #[serde(rename = "activeWorkspace", default)]
    pub active_workspace: Option<WorkspaceRef>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkspaceRef {
    pub id: i64,
    pub name: String,
}

impl Monitor {
    pub fn is_padlink(&self) -> bool {
        self.name.starts_with(OUTPUT_PREFIX)
    }

    /// The area this monitor covers in the layout. Transforms 1, 3, 5 and 7
    /// are the quarter turns, which swap the sides.
    pub fn rect(&self) -> Rect {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (mut w, mut h) = (
            (f64::from(self.width) / scale).round() as i32,
            (f64::from(self.height) / scale).round() as i32,
        );
        if self.transform % 2 == 1 {
            std::mem::swap(&mut w, &mut h);
        }
        Rect {
            x: self.x,
            y: self.y,
            w,
            h,
        }
    }
}

pub fn parse_monitors(json: &str) -> Result<Vec<Monitor>> {
    serde_json::from_str(json).map_err(|e| Error::BadReply {
        command: "monitors".into(),
        detail: e.to_string(),
    })
}

/// Everything about the virtual display that padlink asks Hyprland for.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSpec {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub scale: f64,
    pub x: i32,
    pub y: i32,
}

impl OutputSpec {
    fn describe(&self) -> String {
        format!(
            "{}x{}@{} scale {} at {}x{}",
            self.width, self.height, self.hz, self.scale, self.x, self.y
        )
    }

    fn matches(&self, m: &Monitor) -> bool {
        m.width == self.width
            && m.height == self.height
            && (m.refresh_rate - f64::from(self.hz)).abs() < 0.5
            && (m.scale - self.scale).abs() < 1e-3
            && m.x == self.x
            && m.y == self.y
    }
}

fn describe_monitor(m: &Monitor) -> String {
    format!(
        "{}x{}@{:.0} scale {} at {}x{}",
        m.width, m.height, m.refresh_rate, m.scale, m.x, m.y
    )
}

/// A Lua string literal.
pub fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The request that sets the output's monitor rule (R1.9).
///
/// Under Lua, `hl.monitor` replaces the rule for that output instead of
/// merging into it, so mode, position and scale always go in one call, and
/// the position uses `XxY`. Under a legacy config the same rule is a
/// `keyword monitor` line.
pub fn monitor_rule(dialect: Dialect, spec: &OutputSpec) -> String {
    let OutputSpec {
        name,
        width,
        height,
        hz,
        scale,
        x,
        y,
    } = spec;
    match dialect {
        Dialect::Lua => format!(
            "eval hl.monitor({{output={}, mode=\"{width}x{height}@{hz}\", \
             position=\"{x}x{y}\", scale={scale}}})",
            lua_str(name)
        ),
        Dialect::Legacy => {
            format!("keyword monitor {name},{width}x{height}@{hz},{x}x{y},{scale}")
        }
    }
}

/// What had focus before the output was created (R1.11).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Focus {
    pub monitor: Option<String>,
    /// The active window's address, such as `0x55f3ea42e280`.
    pub window: Option<String>,
    /// The pointer position relative to the focused monitor's top left
    /// corner. Relative, because adding an output can shift monitors that
    /// are positioned automatically, and an absolute position would then
    /// land on a different monitor and take focus with it.
    pub cursor_offset: Option<(f64, f64)>,
}

/// The dispatch requests that give focus back, in order (R1.11).
/// `monitor_origin` is where the focused monitor is in the layout now.
pub fn restore_focus_requests(
    dialect: Dialect,
    focus: &Focus,
    monitor_origin: Option<(i32, i32)>,
) -> Vec<String> {
    let mut out = Vec::new();
    let cursor = focus
        .cursor_offset
        .zip(monitor_origin)
        .map(|((dx, dy), (mx, my))| (f64::from(mx) + dx, f64::from(my) + dy));
    match dialect {
        Dialect::Lua => {
            if let Some(m) = &focus.monitor {
                out.push(format!(
                    "dispatch hl.dsp.focus({{ monitor = {} }})",
                    lua_str(m)
                ));
            }
            if let Some(w) = &focus.window {
                out.push(format!(
                    "dispatch hl.dsp.focus({{ window = {} }})",
                    lua_str(&format!("address:{w}"))
                ));
            }
            if let Some((x, y)) = cursor {
                out.push(format!(
                    "dispatch hl.dsp.cursor.move({{ x = {x}, y = {y} }})"
                ));
            }
        }
        Dialect::Legacy => {
            if let Some(m) = &focus.monitor {
                out.push(format!("dispatch focusmonitor {m}"));
            }
            if let Some(w) = &focus.window {
                out.push(format!("dispatch focuswindow address:{w}"));
            }
            if let Some((x, y)) = cursor {
                out.push(format!("dispatch movecursor {} {}", x.round(), y.round()));
            }
        }
    }
    out
}

/// A connection to one Hyprland instance.
#[derive(Debug, Clone)]
pub struct Hyprland {
    socket: PathBuf,
}

impl Hyprland {
    pub fn new(request_socket: impl Into<PathBuf>) -> Self {
        Hyprland {
            socket: request_socket.into(),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Send one request and return the reply.
    pub fn request(&self, command: &str) -> Result<String> {
        let unreachable = |source| Error::Unreachable {
            path: self.socket.clone(),
            source,
        };
        let mut s = UnixStream::connect(&self.socket).map_err(unreachable)?;
        s.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(unreachable)?;
        s.write_all(command.as_bytes()).map_err(unreachable)?;
        let mut reply = String::new();
        s.read_to_string(&mut reply).map_err(unreachable)?;
        Ok(reply)
    }

    /// A request whose only good answer is `ok`.
    fn command(&self, command: &str, action: &str) -> Result<()> {
        let reply = self.request(command)?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(Error::Refused {
                action: action.to_string(),
                reply: reply.trim().to_string(),
            })
        }
    }

    fn json(&self, command: &str) -> Result<serde_json::Value> {
        let reply = self.request(&format!("j/{command}"))?;
        serde_json::from_str(&reply).map_err(|e| Error::BadReply {
            command: command.to_string(),
            detail: e.to_string(),
        })
    }

    pub fn version(&self) -> Result<Version> {
        let v = self.json("version")?;
        ["version", "tag"]
            .iter()
            .filter_map(|k| v.get(k)?.as_str())
            .find_map(parse_version)
            .ok_or_else(|| Error::BadReply {
                command: "version".into(),
                detail: "no version number in the reply".into(),
            })
    }

    /// Detect the config language with a Lua statement that does nothing:
    /// a Lua-configured Hyprland answers `ok`, a legacy one says `eval` is
    /// not supported.
    pub fn dialect(&self) -> Result<Dialect> {
        Ok(if self.request("eval local _ = 1")?.trim() == "ok" {
            Dialect::Lua
        } else {
            Dialect::Legacy
        })
    }

    /// Whether `ecosystem:enforce_permissions` is on. When it is, Hyprland
    /// hands screen capture black frames unless a permission rule allows it.
    pub fn enforces_permissions(&self) -> Result<bool> {
        let v = self.json("getoption ecosystem:enforce_permissions")?;
        Ok(
            v.get("bool").and_then(serde_json::Value::as_bool) == Some(true)
                || v.get("int").and_then(serde_json::Value::as_i64) == Some(1),
        )
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>> {
        parse_monitors(&self.request("j/monitors")?)
    }

    pub fn monitor(&self, name: &str) -> Result<Option<Monitor>> {
        Ok(self.monitors()?.into_iter().find(|m| m.name == name))
    }

    /// The layout areas of every monitor that is not one of padlink's own.
    pub fn other_monitor_rects(&self) -> Result<Vec<Rect>> {
        Ok(self
            .monitors()?
            .iter()
            .filter(|m| !m.is_padlink())
            .map(Monitor::rect)
            .collect())
    }

    /// Outputs left behind by a daemon that did not shut down cleanly (R1.20).
    pub fn leftover_outputs(&self) -> Result<Vec<String>> {
        Ok(self
            .monitors()?
            .into_iter()
            .filter(Monitor::is_padlink)
            .map(|m| m.name)
            .collect())
    }

    /// What has focus now, to be handed back with [`Hyprland::restore_focus`].
    pub fn focus(&self) -> Result<Focus> {
        let focused = self.monitors()?.into_iter().find(|m| m.focused);
        let window = self
            .json("activewindow")
            .ok()
            .and_then(|v| Some(v.get("address")?.as_str()?.to_string()));
        let cursor = self
            .json("cursorpos")
            .ok()
            .and_then(|v| Some((v.get("x")?.as_f64()?, v.get("y")?.as_f64()?)));
        // Only a pointer that is on the focused monitor is worth restoring.
        let cursor_offset = cursor.zip(focused.as_ref()).and_then(|((x, y), m)| {
            let r = m.rect();
            let (dx, dy) = (x - f64::from(r.x), y - f64::from(r.y));
            (dx >= 0.0 && dy >= 0.0 && dx < f64::from(r.w) && dy < f64::from(r.h))
                .then_some((dx, dy))
        });
        Ok(Focus {
            monitor: focused.map(|m| m.name),
            window,
            cursor_offset,
        })
    }

    /// Give focus back (R1.11). A window that closed in the meantime is not
    /// an error: every step is attempted and the first failure is returned.
    pub fn restore_focus(&self, dialect: Dialect, focus: &Focus) -> Result<()> {
        let origin = match &focus.monitor {
            Some(name) => self.monitor(name)?.map(|m| (m.x, m.y)),
            None => None,
        };
        let mut first_error = None;
        for request in restore_focus_requests(dialect, focus, origin) {
            if let Err(e) = self.command(&request, "restore focus") {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Create the headless output and apply `spec`, then confirm it from
    /// `monitors` (R1.7). If anything fails the output is removed again, so
    /// no half-configured display is left behind.
    pub fn create_output(&self, dialect: Dialect, spec: &OutputSpec) -> Result<Monitor> {
        self.command(
            &format!("output create headless {}", spec.name),
            &format!("create the virtual display {}", spec.name),
        )?;
        match self.apply(dialect, spec) {
            Ok(m) => Ok(m),
            Err(e) => {
                let _ = self.remove_output(&spec.name);
                Err(e)
            }
        }
    }

    /// Apply `spec` to an existing output and wait until `monitors` shows it.
    /// Also used to put the settings back after a config reload (R2.5).
    pub fn apply(&self, dialect: Dialect, spec: &OutputSpec) -> Result<Monitor> {
        self.command(
            &monitor_rule(dialect, spec),
            &format!("configure the virtual display {}", spec.name),
        )?;
        let deadline = Instant::now() + SETTLE;
        loop {
            let current = self.monitor(&spec.name)?;
            if let Some(m) = &current {
                if spec.matches(m) {
                    return Ok(m.clone());
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::NotApplied {
                    name: spec.name.clone(),
                    wanted: spec.describe(),
                    got: current
                        .as_ref()
                        .map_or("no such output".to_string(), describe_monitor),
                });
            }
            std::thread::sleep(POLL);
        }
    }

    /// Remove an output and wait until it is gone. One that is already gone
    /// counts as success.
    ///
    /// The caller must have stopped capturing it first and seen the capture
    /// objects destroyed (R1.18): Hyprland 0.56.2 aborts when an output is
    /// removed under an active capture.
    pub fn remove_output(&self, name: &str) -> Result<()> {
        let reply = self.request(&format!("output remove {name}"))?;
        let reply = reply.trim();
        if reply != "ok" && reply != "output not found" {
            return Err(Error::Refused {
                action: format!("remove the virtual display {name}"),
                reply: reply.to_string(),
            });
        }
        let deadline = Instant::now() + SETTLE;
        while self.monitor(name)?.is_some() {
            if Instant::now() >= deadline {
                return Err(Error::NotApplied {
                    name: name.to_string(),
                    wanted: "removed".into(),
                    got: "still listed".into(),
                });
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipad() -> OutputSpec {
        OutputSpec {
            name: "padlink-ba601e".into(),
            width: 2360,
            height: 1640,
            hz: 60,
            scale: 2.0,
            x: 2877,
            y: 0,
        }
    }

    #[test]
    fn output_names_are_explicit_and_prefixed() {
        assert_eq!(output_name("ba601e"), "padlink-ba601e");
        assert_eq!(output_name("BA-60 1E"), "padlink-ba601e");
        assert_eq!(output_name(""), "padlink-ipad");
    }

    #[test]
    fn the_lua_rule_sets_mode_position_and_scale_in_one_call() {
        assert_eq!(
            monitor_rule(Dialect::Lua, &ipad()),
            r#"eval hl.monitor({output="padlink-ba601e", mode="2360x1640@60", position="2877x0", scale=2})"#
        );
    }

    #[test]
    fn the_legacy_rule_is_a_keyword_monitor_line() {
        assert_eq!(
            monitor_rule(Dialect::Legacy, &ipad()),
            "keyword monitor padlink-ba601e,2360x1640@60,2877x0,2"
        );
    }

    #[test]
    fn negative_positions_and_fractional_scales_render_plainly() {
        let spec = OutputSpec {
            x: -1180,
            y: -820,
            scale: 1.25,
            ..ipad()
        };
        assert!(monitor_rule(Dialect::Lua, &spec).contains(r#"position="-1180x-820", scale=1.25"#));
        assert!(monitor_rule(Dialect::Legacy, &spec).ends_with(",-1180x-820,1.25"));
    }

    #[test]
    fn lua_strings_are_escaped() {
        assert_eq!(lua_str("eDP-1"), r#""eDP-1""#);
        assert_eq!(lua_str(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn focus_is_restored_monitor_first_then_window_then_pointer() {
        let focus = Focus {
            monitor: Some("eDP-1".into()),
            window: Some("0x55f3ea42e280".into()),
            cursor_offset: Some((243.0, 540.5)),
        };
        // The monitor sits at 957x0, so the pointer goes back to 1200x540.5.
        let origin = Some((957, 0));
        assert_eq!(
            restore_focus_requests(Dialect::Lua, &focus, origin),
            [
                r#"dispatch hl.dsp.focus({ monitor = "eDP-1" })"#,
                r#"dispatch hl.dsp.focus({ window = "address:0x55f3ea42e280" })"#,
                "dispatch hl.dsp.cursor.move({ x = 1200, y = 540.5 })",
            ]
        );
        assert_eq!(
            restore_focus_requests(Dialect::Legacy, &focus, origin),
            [
                "dispatch focusmonitor eDP-1",
                "dispatch focuswindow address:0x55f3ea42e280",
                "dispatch movecursor 1200 541",
            ]
        );
        assert!(restore_focus_requests(Dialect::Lua, &Focus::default(), origin).is_empty());
    }

    /// Adding an output can shift an automatically positioned monitor; the
    /// pointer must follow it instead of landing on whatever is at the old spot.
    #[test]
    fn the_pointer_follows_its_monitor_when_the_layout_shifts() {
        let focus = Focus {
            monitor: Some("WAYLAND-1".into()),
            window: None,
            cursor_offset: Some((237.0, 126.0)),
        };
        let moved = restore_focus_requests(Dialect::Lua, &focus, Some((4180, 0)));
        assert_eq!(
            moved[1],
            "dispatch hl.dsp.cursor.move({ x = 4417, y = 126 })"
        );
        // A monitor that has gone leaves the pointer alone.
        assert_eq!(restore_focus_requests(Dialect::Lua, &focus, None).len(), 1);
    }

    #[test]
    fn versions_parse() {
        let v = |s| parse_version(s).unwrap();
        assert_eq!(v("0.56.2").to_string(), "0.56.2");
        assert_eq!(v("v0.57.0-b").minor, 57);
        assert_eq!(v("0.55").patch, 0);
        assert!(v("0.56.2") < v("0.57.0"));
        assert!(parse_version("main").is_none());
    }

    /// `hyprctl -j monitors` from the reference laptop, trimmed, plus a
    /// padlink output and a rotated external monitor.
    const MONITORS: &str = r#"[{
        "id": 0, "name": "eDP-1", "description": "Lenovo", "width": 1920, "height": 1080,
        "refreshRate": 60.00000, "x": 957, "y": 0,
        "activeWorkspace": {"id": 1, "name": "1"}, "scale": 1.00, "transform": 0,
        "focused": true, "availableModes": ["1920x1080@60.00Hz"]
    },{
        "id": 1, "name": "padlink-ba601e", "width": 2360, "height": 1640,
        "refreshRate": 60.00000, "x": 2877, "y": 0, "scale": 2.00, "transform": 0,
        "focused": false
    },{
        "id": 2, "name": "DP-1", "width": 2560, "height": 1440,
        "refreshRate": 59.95100, "x": -1440, "y": 0, "scale": 1.00, "transform": 1
    }]"#;

    #[test]
    fn monitors_parse_and_report_their_layout_area() {
        let m = parse_monitors(MONITORS).unwrap();
        assert_eq!(m.len(), 3);
        assert!(m[0].focused && !m[0].is_padlink());
        assert_eq!(m[0].active_workspace.as_ref().unwrap().name, "1");
        assert_eq!(
            m[0].rect(),
            Rect {
                x: 957,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
        // Scale 2 halves the area.
        assert!(m[1].is_padlink());
        assert_eq!((m[1].rect().w, m[1].rect().h), (1180, 820));
        // A quarter turn swaps the sides.
        assert_eq!((m[2].rect().w, m[2].rect().h), (1440, 2560));
    }

    #[test]
    fn a_spec_matches_only_the_monitor_state_it_asked_for() {
        let m = parse_monitors(MONITORS).unwrap().remove(1);
        assert!(ipad().matches(&m));
        assert!(
            !OutputSpec {
                scale: 1.0,
                ..ipad()
            }
            .matches(&m)
        );
        assert!(!OutputSpec { x: 0, ..ipad() }.matches(&m));
        assert!(
            !OutputSpec {
                height: 1632,
                ..ipad()
            }
            .matches(&m)
        );
        assert!(!OutputSpec { hz: 30, ..ipad() }.matches(&m));
    }

    #[test]
    fn a_malformed_monitor_list_is_an_error_not_a_panic() {
        assert!(matches!(
            parse_monitors("unknown request"),
            Err(Error::BadReply { .. })
        ));
    }
}
