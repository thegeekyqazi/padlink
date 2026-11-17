# padlink handoff

For whoever continues this project, human or agent. It assumes you have not
seen the earlier conversation. Keep it current: update the "State" and "What
is left" sections whenever you finish something.

Last updated: 16 Nov 2026, after the long-running daemon and the `padlink`
CLI were built and checked against the real iPad in the nested bench.

## 1. What this is

`padlink` makes an iPad a true extended monitor for Hyprland over a USB
cable. The Linux side is built here; the iPad side is the existing
**OpenDisplay** receiver app from the App Store. The daemon speaks the
OpenDisplay wire protocol, version 3.

- `SPEC.md`: the requirements, with IDs (R1.1 to R3.6). The source of truth.
- `docs/spec-deviations.md`: every place the work differs from the spec,
  targets that cannot be met, and a table of what Phase 1 still lacks.
- `docs/spikes.md`: every measurement taken, with the commands.
- `AGENTS.md`: the rules. Read them first; rule 1 protects the user's
  desktop.

The user's priorities, in order: latency, then plug-and-go reliability, then
pencil support. The user wants to draw in desktop apps with the pencil.

## 2. The machine

Lenovo laptop, Ryzen 5 PRO 5650U, Radeon integrated GPU (radeonsi), Fedora
44, Hyprland 0.56.2 with a **Lua** config, iPad 10th generation (2360x1640,
60 Hz) on USB 2.0. Details and corrections to the spec's table are in
`docs/spec-deviations.md` (F1 to F6).

Already installed: `mesa-va-drivers-freeworld`, `libavcodec-freeworld`
(x264), FFmpeg 8.1 with `libavcodec`, `libavfilter`, `libavutil`,
`libswscale` headers, `libva-devel`, `libdrm-devel`, `wayland-devel`,
`libusbmuxd-utils`, `wf-recorder`, `foot`. Rust 1.95.

Not installed and possibly needed for zero-copy capture: `mesa-libgbm-devel`
(check with `rpm -q`; ask the user to install it, never use sudo yourself).

## 3. Layout

```
crates/
  padlink-usbmux    usbmuxd client: list devices, hotplug events, tunnel to a port
  padlink-proto     Receiver trait; OpenDisplay protocol 3 sender; stream planning
  padlink-hypr      Hyprland IPC: instance discovery, layout maths, output lifecycle
  padlink-capture   wlr-screencopy into leased shared-memory buffers
  padlink-encode    h264_vaapi, x264, OpenH264 through raw FFmpeg bindings
  padlink-input     touch, stylus and scroll as a virtual pointer on the output
  padlink-config    ~/.config/padlink/config.toml, strict keys
  padlinkd          the daemon: lock, control socket, state machine, sessions
  padlink           the CLI: status, connect, disconnect, clean, logs
testbench/          nested Hyprland and the scripts that are safe to run
spikes/             Phase 0 probes (Python): usbmux_probe.py, tap_proxy.py
reference/          git-ignored: OpenDisplay source and its od-sender binary
config.example.toml every config key, with defaults
```

`reference/` is not in git. Re-create it with:

```sh
mkdir -p reference && cd reference
git clone --depth 1 --branch v1.30.3 https://github.com/peetzweg/opendisplay
```

The OpenDisplay protocol crates are a pinned git dependency (tag `v1.30.3`)
in the workspace `Cargo.toml`; `reference/` is only for reading. The wire
protocol is `reference/opendisplay/PROTOCOL.md`.

## 4. How it works today

`padlinkd` (`crates/padlinkd/src/main.rs`) runs until it is signalled:

1. Loads the config. A broken config stops it before it touches anything.
2. Takes the session lock (`$XDG_RUNTIME_DIR/padlink/lock`, `flock`), finds
   the Hyprland instance, detects version and config dialect, removes
   leftover `padlink-*` outputs, and listens on the control socket
   (`$XDG_RUNTIME_DIR/padlink/control`, one JSON line each way; types in
   `padlink-config/src/runtime.rs`).
3. Loops ten times a second: reads what the user wants (`Shared::wanted`,
   set by `connect` and `disconnect`), polls usbmuxd for a USB device, and
   feeds events to the pure state machine in `machine.rs` (`Paused`, `Idle`,
   `Attached`, `Connected`). The machine answers with when to dial next
   (backoff 0.5 s to 3 s).
4. When a dial succeeds it runs a `Session` (`session.rs`) on the same
   thread:
   - a reader thread feeds socket bytes to the session thread;
   - the session thread drives `padlink_proto::OpenDisplay` and performs its
     `Action`s. It is the only writer to the socket;
   - on `Action::Display` it builds a `Pipeline`: opens the encoder, creates
     the headless output, restores focus, starts the input injector, a
     capture thread and an encode thread;
   - capture puts frames into a latest-wins `Slot`; the encode thread takes
     them and sends packets to the session thread through a channel of one;
   - once a second it publishes figures into the shared `Status`.
5. A session ends on `disconnect`, a signal, `closing`, `sleeping` or a
   dropped link. It tears the pipeline down in the fixed order and the loop
   carries on; only a signal exits.

The `padlink` CLI (`crates/padlink`) talks to the control socket. `clean`
does not: it talks to Hyprland directly and refuses while the lock is held.

The stream is always exactly the output's size (no scaling). The output name
is `padlink-<last 6 characters of the UDID>`.

## 5. How to build, test and run

```sh
cargo test --workspace                 # unit and fake-socket tests, real encodes
testbench/nested.sh                    # terminal 1: nested Hyprland in a window
testbench/run-nested-tests.sh          # terminal 2: the 8 tests that create outputs
testbench/padlinkd.sh                  # terminal 2: the daemon, against the nested instance
testbench/padlink.sh status            # terminal 3: the CLI, with the nested environment
testbench/wiggle.sh 12                 # light load: moves the pointer on the padlink output
cargo run --release -p padlink-encode --example bench            # encoder timing
cargo run -p padlink-usbmux --example hello                      # reads the iPad's hello
```

The iPad must be plugged in, unlocked, with the OpenDisplay app in the
foreground, for anything that streams. `PADLINK_LOG=debug` raises the log
level. To use another config: `testbench/padlinkd.sh --config <path>`.

For heavy load, put a terminal on the padlink output inside the nested
instance (source `$XDG_RUNTIME_DIR/padlink-testbench/env` first):

```sh
hyprctl -i "$HYPRLAND_INSTANCE_SIGNATURE" dispatch 'hl.dsp.focus({ monitor = "padlink-ba601e" })'
foot -e sh -c 'while :; do head -c 4000 /dev/urandom | base64; done'
```

Things that bit earlier and will bite again:

- The user's shell is zsh. An unquoted `$var` is not word-split, and
  `read -t N` on a file does not time out. Write helper scripts in bash.
- Under a Lua config, `hyprctl keyword` is refused. Use
  `hyprctl eval 'hl.monitor({...})'` and `hyprctl dispatch 'hl.dsp....'`.
  `padlink-hypr` already handles both dialects.
- Always pass `-i <signature>` to `hyprctl` when you mean the nested
  instance; without it you talk to the live session.
- In the nested bench the main monitor is auto-positioned, so adding an
  output shifts the layout. Read positions back; do not assume them.
- `rustfmt` reformats files after you write them, so re-read before editing
  by exact string match.

## 6. State

Working and tested:

| Area | Requirements covered | Evidence |
| --- | --- | --- |
| usbmuxd transport | R1.1, R1.3; R1.2 partly | 13 tests against a fake usbmuxd; real iPad `hello` in about 6 ms |
| Protocol | R1.4, R1.5, R1.6, R1.16 (with pencil) | 36 tests against a scripted receiver |
| Hyprland output | R1.7 to R1.11, R1.20 | 27 unit tests, 4 nested tests |
| Capture | R1.12 shared-memory half, R1.18 | 3 nested and unit tests |
| Encode | R1.13, R1.15 | 9 real-encode tests incl. SPS size for all three encoders |
| Input | R1.16, R1.17 | 10 unit tests, 2 nested tests |
| Config | R1.14, R1.23 | 12 tests |
| Daemon | R1.3, R1.19, R1.20, R1.21, R1.22, R1.24 timings in the log | 21 tests (state machine, lock, control socket); with the real iPad: streamed, `status`, `disconnect` in 0.2 s, `connect`, second daemon refused, `kill -9` then `clean` |
| CLI | R1.20, R1.22 | 3 formatting tests; every command run against the nested daemon |

Measured (details in `docs/spikes.md`): light load 14 to 17 ms end to end at
60 fps with either encoder; heavy full-screen load 30 to 39 ms at 39 to 50
fps. The spec's 8 ms capture-to-packet target is not reachable on this GPU
(about 11 ms for the Radeon encoder alone at this size).

User decisions already made (do not re-ask):

- The GPU encoder stays the default; x264 and OpenH264 are selectable with
  `encoder` in the config.
- Pencil input acts as a pointer in Phase 1 (deviation D9), and the real
  tablet device (R3.4) is first among the Phase 3 items.

Unverified, needs the user's eyes on the iPad:

- That touch and pencil move the pointer when streaming from `padlinkd`
  (they did with the official sender for touch; the pencil did nothing
  there because that sender drops it).
- Two-finger scroll: no `scroll` message was seen in the one capture taken.
- Whether the pencil's pressure ever differs from 1.0.

## 7. What is left

In the order I would do it. Each item names the files and the approach.

### Phase 1

1. Done: the long-running daemon, lock file, control socket and CLI.
   Loose ends there: the daemon polls `list_devices` instead of using
   `padlink_usbmux::Listener`; under systemd the log lines still carry their
   own timestamps; `padlink doctor` (R2.10) does not exist.
2. Done: `packaging/padlinkd.service` and `README.md`, if present in the
   tree; otherwise write them (a user unit bound to
   `graphical-session.target`, R2.1).
3. **Zero-copy capture** (R1.12): the biggest remaining latency item for
   the GPU path. It removes the compositor's readback and the 3.6 to 5.8 ms
   upload.
   - Capture: `wlr-screencopy` v3 sends a `linux_dmabuf` event with the
     format and size. Allocate a dmabuf (GBM via the `gbm` crate on the
     render node), wrap it with `zwp_linux_dmabuf_v1` `create_immed`, and
     pass that `wl_buffer` to `copy_with_damage`. Add a `Path::WlrDmabuf`
     and a second `Frame` kind carrying the fd, fourcc, modifier, offset and
     stride instead of a `Lease`.
   - Encode: add `Encoder::encode_dmabuf`. Build a second filter graph in
     `padlink-encode/src/lib.rs`: a `buffer` source whose frames are
     `AV_PIX_FMT_DRM_PRIME` with an `AVDRMFrameDescriptor`, then
     `hwmap=mode=direct:derive_device=vaapi`, then `scale_vaapi=format=nv12`.
     This is what `wf-recorder` does, and Spike 4 proved it works on this
     GPU. Read `wf-recorder`'s `frame-writer.cpp` and Sunshine's
     `src/platform/linux/vaapi.cpp`.
   - Keep shared memory as the automatic fallback and log which path is
     active. Software encoders cannot take dmabufs: use shared memory then.
   - Measure again with `testbench/padlinkd.sh` and add the numbers to
     `docs/spikes.md`.
4. **`ext-image-copy-capture`** for Hyprland 0.57 and later (SPEC.md section
   8). Select by `padlink_hypr::EXT_CAPTURE_SAFE_FROM`; `capture = "ext"` in
   the config must override. Today it warns and uses wlr.
5. **Fake receiver test** (section 9): a test that listens on a local
   socket, sends `hello`, and checks the stream it gets back: first frame is
   a keyframe with SPS and PPS at the right size, `kf` produces another
   keyframe. It can drive `Session` with a `UnixStream::pair()` only inside
   the nested bench, since a session creates an output.
6. **R1.2**: tell "locked" from "not trusted". usbmuxd's `ReadPairRecord`
   fails for an untrusted device; a paired but locked device refuses
   lockdown sessions. Today one message covers unplugged, locked and
   untrusted (`padlink_usbmux::Error::DeviceGone`).
7. **Bitrate follows content** (deviation T3): the Radeon encoder emits
   about 25 Mbit/s even for a moving pointer, in CBR and VBR alike. Try
   `rc_mode=CQP` with `max_frame_size`, using
   `cargo run --release -p padlink-encode --example bench rc_mode=CQP`
   and `BAND=8` for a thin changing strip.
8. **CI**: a GitHub Actions workflow with `cargo fmt --check`, clippy and
   tests. The encode tests skip backends that cannot open; the FFmpeg
   headers must be installed in the runner.
9. A `README.md`: install, usage, the foreground-app limit (SPEC.md section
   6), and credit to OpenDisplay for the adapted code.

Phase 1's commands work in the nested bench. Phase 1 is done when `padlink connect` extends the desktop with hardware
encoding and working touch, and `padlink disconnect` leaves the session as
it was. Then stop and let the user run the manual acceptance list in
SPEC.md section 9 on the **live** session. That is the first time anything
runs there, and it is the user who runs it.

### Phase 2 and 3

All of SPEC.md section 6 (autostart unit, auto-connect, reconnect,
workspace memory, config reloads, suspend and resume, rotation debounce,
compositor restart, notifications, `doctor`, RPM packaging) and section 7.
For Phase 3 the order is R3.4 (uinput tablet for the pencil) first, then
the spec's order. Rotation already rebuilds the display on a second
`hello`; only the debounce (R2.7) is missing.

## 8. Known problems and loose ends

- `Session` handles a second `hello` by tearing down and rebuilding with no
  debounce.
- The `cursor` option of R3.2 does not exist; the pointer is always drawn
  into the video.
- `padlinkd` picks the first USB device; several iPads are out of scope.
- `.claude/settings.json` (a Claude Code permission rule) is untracked on
  purpose.
- `spikes/__pycache__` is ignored; delete it freely.
