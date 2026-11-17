# iPad second display for Hyprland: build spec

Nov 16, 2026

## 1. Goal and scope

Build `padlink` (working name): a Linux daemon that turns an iPad into a true extended monitor for a Hyprland session, over a USB cable, with the lowest latency the hardware allows. Plugging in the iPad with the receiver app open must be enough to get a working display.

The iPad side is **not** built in this project. The daemon speaks the OpenDisplay wire protocol (`PROTOCOL.md` in [peetzweg/opendisplay](https://github.com/peetzweg/opendisplay), protocol version 3) to the existing OpenDisplay receiver app from the App Store or TestFlight.

**In scope**

- A Rust daemon and CLI for Fedora + Hyprland, licensed GPL-3.0 (it reuses GPL-3.0 code from OpenDisplay).
- USB transport through usbmuxd as the primary path; Wi-Fi as a later fallback.
- GPU capture and hardware H.264 encode, with a software fallback.
- Touch, scroll and later Pencil input from the iPad.
- Automatic start, reconnect and cleanup.

**Out of scope for now**

- A custom iPad app. Keep the protocol layer behind a trait so a different receiver can be added later.
- Keyboard passthrough, audio and encryption. The OpenDisplay protocol has none of them.
- Other compositors (Sway, KDE, GNOME), X11, mirror mode, multiple iPads at once.

**Priorities, in order:** latency, then reliability of plug-and-go, then Pencil support.

## 2. Target environment

These facts come from command output on the target machine on 16 Nov 2026. Treat them as the reference setup; re-check them with the `doctor` command instead of hard-coding them.

| Item | Value | Consequence |
| --- | --- | --- |
| Laptop | Lenovo, AMD Ryzen 5 PRO 5650U, Radeon "Cezanne" integrated GPU (radeonsi) | Hardware H.264 and HEVC encode exist in the chip. No AV1. No NVIDIA path needed. |
| OS | Fedora 44, kernel 7.2, Mesa 26.2.6 | RPM Fusion free and nonfree repos are already enabled. |
| VA-API today | `vainfo` lists only MPEG-2, JPEG and VP9 decode. No H.264 or HEVC, no encode entrypoints | Hardware encode is unavailable until `mesa-va-drivers-freeworld` is installed. See section 3. |
| FFmpeg | `ffmpeg-free` 8.1.3 | No libx264. Software fallback must be OpenH264, or x264 after swapping to RPM Fusion `ffmpeg`. |
| Compositor | Hyprland 0.56.2, Aquamarine 0.15 | Has a crash when an output is removed during capture. See section 8. |
| Hyprland config | Lua (`~/.config/hypr/hyprland.lua`, with `monitors.lua` and `workspaces.lua`) | Runtime changes go through `hyprctl eval`, not `hyprctl keyword`. |
| Permissions | `ecosystem:enforce_permissions` is false | No capture permission rule needed here; still detect it. |
| Main monitor | `eDP-1`, 1920x1080 at 60 Hz, scale 1, positioned at 957x0 | The iPad output defaults to the right: x = 2877, y = 0. Compute this from the live layout. |
| usbmuxd | 1.1.1 (Fedora package, git 2025-12-05) | `idevice_id -l` already lists the iPad, so the stock package works for a TCP tunnel. |
| iPad | iPad 10th generation, iPadOS 26.3.1, USB-C port at USB 2.0 speed | Panel 2360x1640 at 60 Hz. No 120 Hz work. USB 2.0 is ample for a 20 to 40 Mbit/s stream. |
| Stylus | Third-party pencil | Probably reports no pressure or tilt. Treat as touch until proven otherwise. |
| Receiver app | OpenDisplay installed from the App Store on iPadOS 26.3.1; it opens and shows "Ready to connect" | The app runs. Streaming to it is still unproven until Spike 1. |

## 3. Phase 0: prerequisites and spikes

Run these before writing the daemon. Each one removes a risk that would otherwise surface late. Record the results in `docs/spikes.md` in the repo. Stop and report if a spike fails; do not design around a failure silently.

**Host setup (the user runs these; they need sudo)**

```sh
sudo dnf install mesa-va-drivers-freeworld
vainfo | grep -E 'H264|HEVC'
```

Expected: `VAProfileH264High` and `VAProfileHEVCMain` each with `VAEntrypointEncSlice`. The earlier `vainfo` run showed libva already looks in `/usr/lib64/dri-freeworld/`, which is where this package installs.

**Spike 1: the receiver works at all.** OpenDisplay is already installed on the iPad. Download the official `od-sender` x86_64 tarball from the OpenDisplay releases page and run `od-sender doctor`, then `od-sender run` over Wi-Fi. This proves the app streams on iPadOS 26 and gives a baseline: note the latency and frame rate the app's overlay reports with CPU encoding.

**Spike 2: USB tunnel with no new code.** With the iPad plugged in and the app open, run `iproxy 9000 9000` (from `libimobiledevice-utils`), then `od-sender run --to 127.0.0.1:9000`. This proves the usbmuxd tunnel reaches the receiver's TCP port 9000. Compare latency with Spike 1.

**Spike 3: hardware encode at the iPad's size.** Encode a 2360x1640 60 fps test pattern with `h264_vaapi` through the FFmpeg CLI, no B-frames. Check with `ffprobe` that the stream reports 2360x1640. 1640 is not a multiple of 16, so the encoder pads to 1648 and must signal cropping. If the reported size is wrong, the daemon has to rewrite the cropping fields in the SPS, or use a 2360x1632 mode.

**Spike 4: zero-copy capture.** In a nested Hyprland (never the user's live session), create a headless output and capture it into dmabuf buffers, then import those into VA-API (DRM PRIME), convert to NV12 with the VA-API video processor, and encode. Hyprland 0.56.2 offers both `wlr-screencopy` and `ext-image-copy-capture`; use `wlr-screencopy` on this version (section 8). Measure capture-to-encoded-packet time. Target: under 8 ms at 60 fps.

**Fallback if Spike 4 fails:** capture into shared memory and upload to VA-API (`hwupload`). This still uses the hardware encoder and is acceptable for Phase 1; keep zero-copy as an open task.

**Spike 5: stylus type.** In the OpenDisplay app with the official sender at debug log level, draw with the third-party pencil and note whether `pencil` messages arrive or only `touch`. This decides whether Phase 3 Pencil work is worth doing.

## 4. Architecture

One long-running user daemon (`padlinkd`) owns everything; a thin CLI (`padlink`) talks to it over a Unix socket. The daemon is a state machine driven by three event sources: usbmuxd device events, the receiver connection, and Hyprland's event socket.

**Frame path, in order**

1. Hyprland renders the headless output.
2. The capture module receives each frame as a dmabuf (GPU buffer), with damage information so unchanged frames are skipped.
3. The VA-API video processor converts it to NV12 on the GPU.
4. `h264_vaapi` encodes it: no B-frames, low-latency rate control, one slice group per picture.
5. The packetizer emits Annex B with 4-byte start codes, prefixes SPS and PPS on every IDR, and adds the JSON telemetry prefix (`cap`, `snd` timestamps).
6. The session writes it to one TCP stream, tunnelled by usbmuxd to port 9000 on the iPad.

**Input path:** control messages from the receiver (`touch`, `scroll`, later `pencil`) are mapped from normalized coordinates onto the headless output and injected through `zwlr_virtual_pointer_v1`.

**Workspace layout (Cargo workspace)**

| Crate | Responsibility |
| --- | --- |
| `padlink-proto` | Wire protocol. Depend on OpenDisplay's `opendisplay-proto` and `opendisplay-session` crates by pinned git tag instead of rewriting them. Wrap them behind a `Receiver` trait so another receiver can be added later. |
| `padlink-usbmux` | Speaks the usbmuxd protocol on `/var/run/usbmuxd`: `Listen` for attach and detach, `Connect` to a device port. Also the hotplug source; no udev rule needed. |
| `padlink-hypr` | Hyprland IPC: create, configure and remove the headless output, read monitors, subscribe to events, detect version and config dialect. |
| `padlink-capture` | Wayland client for `wlr-screencopy` and `ext-image-copy-capture`, dmabuf first, shared memory fallback. |
| `padlink-encode` | FFmpeg bindings: `h264_vaapi` first, then HEVC later, then software OpenH264 or x264. One trait, runtime selection. |
| `padlink-input` | Virtual pointer now; `uinput` tablet in Phase 3. |
| `padlinkd` | State machine, config, control socket, logging, systemd integration. |
| `padlink` | CLI: `status`, `doctor`, `connect`, `disconnect`, `clean`, `logs`. |

**Session states:** `Idle` (no device) → `Attached` (device on USB, dialing port 9000 with backoff until the app is open) → `Handshake` (waiting for the receiver's `hello`) → `Streaming` (output exists, frames flowing) → `Teardown` (capture stopped, then output removed) → `Idle` or `Attached`.

**Latency rules that shape the code**

- Never queue frames. Between capture, encode and network there is one latest-wins slot each; a newer frame replaces an unsent one and a counter records the drop.
- `TCP_NODELAY` on every socket. Input messages are tiny and must not be coalesced.
- Capture is event-driven from compositor damage, not a timer. A static screen sends nothing, except one replayed IDR when the receiver asks with `kf` or reconnects.
- IDR frames only on connect, on `kf`, and on resolution change. No periodic keyframes.
- Each stage runs on its own thread. The Wayland event queue is not `Send`; keep capture and input each on a dedicated thread with channels.

**Reference code to read before writing each module** (all GPL-3.0 or compatible; credit what is reused)

- `opendisplay/linux/crates/od-sender/src/backend/hyprland/` for IPC, output drift and reload handling.
- `opendisplay/linux/crates/od-sender/src/wayland/` for capture buffer leasing and virtual pointer mapping.
- `opendisplay/linux/crates/od-sender/src/session.rs` for handshake, liveness and rotation.
- `aaditya19saini/opendisplay-linux`, `Linux/src/socket.cpp` and `discovery.cpp`, for the usbmuxd connection sequence.
- Sunshine `src/platform/linux/wlgrab.cpp` and `vaapi.cpp` for dmabuf capture into VA-API.

## 5. Phase 1: core

Phase 1 is done when `padlink connect` extends the desktop onto the plugged-in iPad with hardware encoding and working touch, and `padlink disconnect` leaves the session exactly as it was.

**Transport**

- R1.1 Discover iOS devices through usbmuxd and connect to TCP port 9000 on the selected device. Do not require `iproxy`, a network interface, NetworkManager or firewall changes.
- R1.2 If the device is not paired or is locked, say so in plain words and name the fix (unlock, accept Trust).
- R1.3 If port 9000 refuses the connection, report that the receiver app is not open, and keep retrying with backoff (0.5 s rising to 3 s).

**Protocol**

- R1.4 Implement the sender side of protocol version 3: wait for `hello`, reply `welcome`, answer `ping` with `pong`, send the sender `ping` liveness beat, honour `kf`, `sleeping` and `closing`. Ignore unknown message types and fields.
- R1.5 Size the display from `hello.panel` when present, otherwise from `pixelsWide` and `pixelsHigh`. Take nothing from the deprecated `scale` field.
- R1.6 Send `streamConfig` before video when the receiver advertises `videoCaps`.

**Virtual display**

- R1.7 Create a named headless output `padlink-<short device id>`, set mode, scale and position, and read the result back from `hyprctl monitors -j` instead of trusting the request.
- R1.8 Default mode 2360x1640 at 60 Hz, scale 2 (logical 1180x820), placed to the right of the rightmost monitor, top-aligned. Position, side and scale are configurable. Reject a scale that does not divide both dimensions into whole logical pixels, and say which values work.
- R1.9 Detect the config dialect at runtime. Under a Lua config use `hyprctl eval 'hl.monitor({...})'` with all of `mode`, `position` and `scale` in one call (it replaces the rule, it does not merge) and `XxY` position syntax. Under a legacy config use `hyprctl keyword monitor`.
- R1.10 Do not edit the user's Hyprland config files. Ever.
- R1.11 Before creating the output, record the focused monitor and window; restore focus afterwards so the keyboard does not jump to the new output.

**Capture and encode**

- R1.12 Use the capture path proven in Spike 4, with the shared-memory path as an automatic fallback. Log which path is active.
- R1.13 Encoder preference: `h264_vaapi`, then software. Software means x264 when FFmpeg has it, otherwise OpenH264. Log the choice and why.
- R1.14 Default 60 fps cap, 25 Mbit/s, constrained to what `videoCaps` and `displayMaxFrameRate` allow. All configurable.
- R1.15 The stream's signalled size must equal the output size exactly (the cropping issue from Spike 3). Add a unit test that parses the SPS of the first IDR and asserts this.

**Input**

- R1.16 Map `touch` to pointer motion plus left button on the headless output; `scroll` to axis events with the protocol's natural-scrolling sign. Stylus input that arrives as `touch` works the same way.
- R1.17 Release any held button when the session ends or the connection drops, so a drag never sticks.

**Teardown**

- R1.18 Order is fixed: stop capture and wait for the capture objects to be destroyed, then remove the output. Never the reverse (section 8).
- R1.19 Run teardown on `disconnect`, `SIGTERM`, `SIGINT`, `SIGHUP`, receiver `closing`, and cable removal.
- R1.20 `padlink clean` removes any leftover `padlink-*` output from a crash. The daemon runs the same check at startup.
- R1.21 A lock file under `$XDG_RUNTIME_DIR/padlink/` (kernel file lock) ensures one daemon per session.

**CLI and config**

- R1.22 `padlink status` prints state, device, transport, encoder, capture path, resolution, fps, bitrate and the latency figures from section 9. `--json` for scripts and bar widgets.
- R1.23 Config lives at `~/.config/padlink/config.toml`. Unknown keys and invalid values are errors, not silently ignored.
- R1.24 Logs go to the journal; `padlink logs` follows them. Debug level includes per-stage timings.

## 6. Phase 2: seamless

Phase 2 is done when the user never types a command: plug in the iPad, open the app, and the display appears within 2 seconds; unplug it and the desktop returns to normal.

One limit cannot be removed from the Linux side: iPadOS only lets the receiver listen while its app is in the foreground. The daemon cannot launch the app. State this in the README.

- R2.1 **Autostart.** Ship a systemd user unit bound to `graphical-session.target`. If the user's Hyprland does not start that target, `doctor` detects it and prints the one `exec-once` line to add; the tool does not add it.
- R2.2 **Auto-connect.** With `autoconnect = true` (default), a device attach event starts dialing without any command. Remember known devices by the receiver's `id`; a new device asks once through a desktop notification action, or connects automatically if `trust-new-devices = true`.
- R2.3 **Reconnect.** After `sleeping` (iPad locked), tear the output down and keep dialing; rebuild on wake. After `closing` (app quit), tear down and wait for the next attach or the app reopening. After a dropped cable, tear down immediately.
- R2.4 **Workspace memory.** Record which workspaces were on the iPad output. Hyprland moves them to another monitor when the output goes away; move them back when it returns. Optional `workspace = "<name or number>"` pins a fixed workspace to the iPad.
- R2.5 **Config reloads.** Subscribe to Hyprland's event socket. After `configreloaded`, wait 200 ms and re-apply the output's mode, scale and position. Also compare the live mode and scale against the applied ones every 2 s. Give up with a clear message after three losses within a minute.
- R2.6 **Suspend and resume.** Listen for logind `PrepareForSleep`. Before sleep, tear down. After resume, start from `Idle` and rediscover. Never trust a headless output that survived a suspend; existing tools get a frozen black frame from it.
- R2.7 **Rotation.** When the receiver re-sends `hello` with new dimensions, rebuild the output in the new orientation. Debounce: 25 ms after a single change, 150 ms after the last of a burst.
- R2.8 **Compositor restart.** If the Hyprland socket disappears, drop all state and wait for a new instance. Support `HYPRLAND_INSTANCE_SIGNATURE` discovery when started from systemd, where the variable may be missing.
- R2.9 **Notifications.** One desktop notification each for connected, disconnected and error, with the reason. Configurable off.
- R2.10 **`padlink doctor`.** Read-only. Checks, in order, and stops reporting at the first failure with a suggested fix: usbmuxd running; device visible and paired; receiver port reachable; Hyprland version and dialect; `enforce_permissions` (if on, print the exact permission rule to add, since Hyprland returns black frames instead of an error); required Wayland protocols; VA-API H.264 encode entrypoint (if missing, name `mesa-va-drivers-freeworld`); FFmpeg encoders available; render node access; leftover outputs.
- R2.11 **Fedora packaging.** A spec file for an RPM (and a COPR-ready layout) that installs both binaries, the user unit and shell completions. `cargo install` remains supported.

## 7. Phase 3 and later

Phase 3 items are independent; do them in this order and stop after any of them.

- R3.1 **HEVC.** Offer `hevc_vaapi` when the receiver's `videoCaps` accepts it. Keep H.264 as default until measurements on this laptop show HEVC is no slower end to end.
- R3.2 **Cursor latency.** On Hyprland the cursor is drawn into the video, so it moves at video latency and can stall on a static screen. Hyprland 0.56 and 0.57 return an empty image from the cursor capture session, so the protocol's separate cursor channel cannot be fed from it. Workaround to build: track the pointer position from Hyprland IPC, send `cursor` position messages at up to 120 per second, send a `cursorImg` rendered from the user's cursor theme, and capture without the cursor. Put it behind `cursor = "video" | "channel"`, default `video` until it is proven.
- R3.3 **Wi-Fi fallback.** Discover `_opensidecar._tcp` over mDNS and connect when no cable is present. Prefer USB whenever both exist, and switch to it when a cable is plugged in mid-session (`hello.addrs`, protocol section 6.4).
- R3.4 **Pencil.** Only if Spike 5 showed `pencil` messages. Create a `uinput` tablet device with pressure and tilt axes, and bind it to the headless output with Hyprland's per-device `output` setting. Needs a udev rule for `/dev/uinput` access; ship it and have `doctor` check it. Hyprland has no virtual tablet protocol, so this is the only route.
- R3.5 **Multi-touch gestures.** A `uinput` touchscreen bound to the output, so pinch and multi-finger swipes work natively instead of as a single pointer.
- R3.6 **Bar integration.** A Waybar custom module example that reads `padlink status --json`.

**Later, separate project:** a custom iPad receiver adding keyboard, audio and encryption. It needs a Mac or a signing service and a re-sign every 7 days on a free Apple ID. The `Receiver` trait from section 4 is the seam for it. An alternative is proposing those messages upstream to OpenDisplay.

## 8. Known pitfalls to design around

Each of these broke an existing project. They are requirements, not background.

| Pitfall | Where it was seen | Required handling |
| --- | --- | --- |
| Hyprland 0.56.2 aborts when a captured output is removed (`CScreenshareFrame::transform` dereferences a dead monitor). Fixed on Hyprland main, not in 0.56.2 | OpenDisplay research notes | Fixed teardown order (R1.18). One sender per output. Never test output removal on the live session. |
| `ext-image-copy-capture` is unsafe on Hyprland below 0.57 when a display disappears | OpenDisplay `od-sender` | Select `wlr-screencopy` below 0.57, `ext-image-copy-capture` from 0.57. Make it overridable. |
| `hyprctl keyword` fails under a Lua config: "keyword can't work with non-legacy parsers" | ipad-virtual-monitor | R1.9. |
| A scale that does not divide the mode evenly is silently rounded by Hyprland | ipad-virtual-monitor | Validate before applying; read back after (R1.7, R1.8). |
| Headless outputs get counter-based names (`HEADLESS-2`, `HEADLESS-3`) that never repeat | ipad-virtual-monitor | Always pass an explicit name. |
| Headless outputs vanish on compositor restart and are frozen after suspend | sunshine-hyprland-virtual-display | Create on demand per session; R2.6 and R2.8. |
| With permissions enforced, Hyprland returns black frames with a caption instead of failing | OpenDisplay `od-sender` | `doctor` check; also detect an all-black first frame and warn. |
| Creating an output can steal focus, so the keyboard appears dead | ipad-virtual-monitor | R1.11. |
| KMS capture cannot see headless outputs and silently mirrors the laptop panel | Sunshine setups | Use Wayland capture protocols only. |
| The portal screencast path shows a picker dialog and cannot preselect the output | opendisplay-linux fork | Do not use the desktop portal. |
| AMD H.264 pads height to a multiple of 16 | General VA-API behaviour | Spike 3 and R1.15. |
| A static screen produces no frames, so a reconnecting receiver sees nothing | OpenDisplay protocol section 5.3 | Keep the last captured frame and re-encode it as an IDR on connect and on `kf`. |
| Frames queue up when the link stalls, and latency grows without bound | Several projects | Latest-wins slots (section 4). |
| Fedora's `ffmpeg-free` lacks libx264; stock Mesa lacks H.264 and HEVC VA-API | This machine's `vainfo` and `rpm -q` output | Runtime detection, clear message, OpenH264 fallback. |

## 9. Testing and acceptance

**Automated, no iPad needed**

- Unit tests: protocol framing and message round-trips, layout maths (position on each side, scale validation), SPS size parsing (R1.15), config parsing errors, state machine transitions driven by fake events.
- A fake receiver that listens on a local port, sends `hello`, decodes the stream and checks size and keyframe behaviour. OpenDisplay's `od-receiver` (Linux) or its mock receiver scripts can serve; otherwise write a small one.
- A nested-Hyprland test bench (modelled on OpenDisplay's `linux/testbench/nested.sh`) that runs create, stream, reload, remove and crash-cleanup scenarios. All output lifecycle tests run there.
- A fake usbmuxd socket for attach, detach and connect-refused cases.
- CI: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.

**Latency measurement**

Build measurement in from the first streaming commit. The daemon records per-frame capture-to-encoded and encoded-to-sent times and reports p50 and p95. The receiver sends `stats` with end-to-end figures (`e2e50`, `e2e95`) based on the telemetry prefix and its clock sync; log them and show them in `padlink status`.

**Targets on the reference hardware, 2360x1640 at 60 fps over USB**

| Measure | Target |
| --- | --- |
| Capture to encoded packet, p50 | 8 ms or less |
| End to end as reported by the receiver, p50 | 20 ms or less |
| End to end, p95 | 35 ms or less |
| Plug-in (app already open) to first frame | 2 s or less |
| Daemon CPU while streaming a moving window | under 15% of one core |
| Daemon CPU with a static screen | near zero |

These are targets, not measured results. For comparison, the community fork reports about 14 ms typical over USB, and the official sender about 20 ms to a phone with CPU encoding. If a target is missed, report the per-stage numbers so the slow stage is known.

**Manual acceptance on the real devices (the user runs these)**

1. Plug in, open the app: display appears to the right of the laptop screen; a window can be dragged onto it.
2. Text is sharp at scale 2; no garbage strip at the bottom edge.
3. Tap, drag and two-finger scroll work and land where the finger is.
4. Lock the iPad, unlock it: the display returns, with the same workspace.
5. Unplug: windows move to the laptop screen; `hyprctl monitors` shows no `padlink-*` output.
6. Run `hyprctl reload` during a session: the stream continues or recovers within 1 s.
7. Suspend and resume the laptop with the iPad attached: the display returns without a black screen.
8. `kill -9` the daemon, restart it: the leftover output is cleaned and the session resumes.
9. Rotate the iPad: the output becomes 1640x2360 and returns on rotating back.

## 10. Working rules for Claude Code, and open risks

**Rules**

1. Work phase by phase. Finish Phase 0 and report the spike results before writing daemon code. Stop at the end of each phase for the user to run the manual acceptance list; Claude Code cannot see the iPad.
2. Never create or remove outputs on the user's live Hyprland session during development. Use the nested test bench. The live session runs 0.56.2, which can crash on a bad teardown and take the user's open work with it.
3. Never edit files under `~/.config/hypr/`, and never run `sudo`. Print the command and the reason and let the user run it.
4. Read the named reference code before writing each module (section 4). Reuse it with attribution where it fits; the project is GPL-3.0 for that reason. Pin the OpenDisplay crates to a release tag.
5. Keep the wire behaviour exactly as `PROTOCOL.md` specifies. If the spec and the receiver disagree, record the difference in `docs/protocol-notes.md` and follow the receiver.
6. Every requirement ID (R1.1 and so on) maps to at least one test or one manual acceptance step. Reference the ID in the commit message.
7. Prefer runtime detection to assumptions: Hyprland version, config dialect, encoders, capture protocols and permissions all vary.
8. Error messages name what is wrong and the next step, in plain words. No raw error codes on their own.
9. If a requirement turns out to be impossible or a better approach appears, stop and ask instead of silently changing scope.

**Open risks**

| Risk | How it is resolved |
| --- | --- |
| The OpenDisplay receiver opens on iPadOS 26.3.1, but streaming to it is untested, and its App Store build may lag the protocol | Spike 1. If it fails, fall back to Sunshine plus Moonlight glue or bring the custom app forward. |
| The Radeon encoder may add latency or lack a low-latency rate-control mode through VA-API | Spike 3 timings. Fall back to constant-QP with a bitrate ceiling, or software encode at reduced resolution. |
| dmabuf frames from Hyprland may not import into the VA-API video processor on radeonsi | Spike 4. Shared-memory upload is the fallback. |
| OpenDisplay's Rust crates are beta and their API may change | Pin a tag; vendor them if upstream breaks. |
| The user may upgrade Hyprland to 0.57 or later mid-project | Capture path is selected at runtime (section 8). |
| The third-party pencil may send only touch events | Spike 5. R3.4 is dropped if so. |
| usbmuxd throughput on a USB 2.0 port under load | Measure in Spike 2; lower the bitrate default if frames are dropped at the network stage. |
