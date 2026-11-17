# Differences from SPEC.md

Where the work so far departs from `SPEC.md`, and where the spec's facts
turned out to be wrong. Updated as the project goes; last update 16 Nov 2026,
early in Phase 1. Entries D1 to D8 are about Phase 0; D9 is an agreed change to Phase 1.

## Where the work differs from the spec

| # | Spec says | What was done | Why | Effect |
| --- | --- | --- | --- | --- |
| D1 | Spike 1: run `od-sender run` **over Wi-Fi** (section 3) | Run over the iPad's USB tethering network interface (`172.20.10.1`), not Wi-Fi | That interface was the laptop's only network; `od-sender list` found the iPad there | The receiver is proven to work (the point of the spike), but there is no Wi-Fi baseline. One is needed before R3.3 (Wi-Fi fallback) |
| D2 | Spike 1 gives no instruction on where to run `od-sender run` | To be run inside the nested test bench, not the live session | `od-sender run` creates and removes a headless output. Rule 2 (section 10) forbids that on the live Hyprland 0.56.2, which can crash on output removal. The two parts of the spec conflict; rule 2 wins | Spike 1's Wi-Fi baseline is measured from a nested compositor. The numbers should be the same; the output just lives in a window |
| D3 | Spike 2: "USB tunnel with no new code", using `iproxy` and `od-sender`; compare latency with Spike 1 | Done as written, after first proving the tunnel with a 100-line Python probe (`spikes/usbmux_probe.py`). The latency comparison is not like for like | The probe was written while `iproxy` and `od-sender` were unavailable. The two streaming runs had different screen content | Tunnel streaming is proven under load. The probe stays as a quick check. No clean Wi-Fi versus USB comparison exists |
| D4 | Spike 4: create a headless output, capture into dmabuf, import into VA-API, convert, encode, and **measure capture-to-encoded-packet time** (target under 8 ms) | Feasibility proven with the installed `wf-recorder` 0.6.0, which runs that same pipeline. No timing measured | It answered the real risk (does dmabuf import work on radeonsi) with no new code. `wf-recorder` reports no per-frame timing, and writing a measuring tool means writing most of the capture and encode crates, which rule 1 puts after Phase 0 | The 8 ms figure is unverified. Throughput suggests about 7 ms per frame for convert plus encode. It will be measured by the daemon's built-in per-stage timing in the first streaming commit (section 9) |
| D5 | Rule 1: "Finish Phase 0 and report the spike results before writing daemon code" | The first Phase 1 crate (`padlink-usbmux`) was written after Spikes 1 to 4 but before Spike 5 was settled. Spike 4 still lacks its latency figure (D4) | Spike 5 only decides a Phase 3 item (R3.4) and the usbmuxd crate does not depend on it | None; all five spikes are now recorded |
| D6 | Host setup: user runs `sudo dnf install mesa-va-drivers-freeworld` | Skipped | Already installed | None |
| D8 | Not in the spec | `testbench/od-sender.sh` wraps the official sender so it can only run against the nested instance | Makes D2 enforceable, and gives the session one narrow thing to be permitted to run | None on the product |
| D9 | R1.16: "Stylus input that arrives as `touch` works the same way"; `pencil` handling is Phase 3 (R3.4) | **Agreed by the user on 16 Nov 2026:** in Phase 1, also map `pencil` down, move and up to pointer motion and left button, exactly like `touch` | Spike 5 showed this receiver sends `pencil`, never `touch`, for the stylus. Without this the pencil is dead until the uinput tablet exists | The pencil works as a plain pointer from Phase 1. R3.4 later replaces it with a real tablet device. Small change in `padlink-input` |
| D10 | R1.20: "`padlink clean` removes any leftover `padlink-*` output from a crash" | `clean` refuses while a daemon holds the session lock | Removing an output that is being captured can crash Hyprland 0.56.2 (section 8). `disconnect` is the right command then, and the message says so | None for the crash case the requirement is about |
| D11 | R2.5: "Give up with a clear message after three losses within a minute" | On giving up the daemon also pauses, as after `padlink disconnect`, and keeps the reason in `padlink status` until `padlink connect` | Otherwise it would redial three seconds later and repeat the cycle for ever against a rule that keeps winning | The user must run `padlink connect` after fixing the rule |
| D12 | R2.6: listen for logind `PrepareForSleep`; tear down before sleep, start from `Idle` after resume | No D-Bus. The daemon notices a suspend afterwards, from the gap between `CLOCK_BOOTTIME` and `CLOCK_MONOTONIC`, ends the session and builds a fresh display | Much less code and it cannot miss a suspend. The requirement's point, never trusting an output that survived a suspend, is met | The display is **not** removed before the laptop sleeps, only replaced after it wakes. Untested with a real suspend |
| D13 | R2.9 notifications | Sent by running `notify-send` | No D-Bus code | Nothing is shown if `notify-send` is not installed. Notification actions (the "ask once about a new device" prompt of R2.2) are not possible this way and are not built |
| D7 | Section 4 lists reference code to read; no location given | Cloned into `reference/`, which is git-ignored, along with the `od-sender` download | Keeps third-party source and binaries out of this repo's history | `reference/` must be re-created on a fresh clone; `docs/spikes.md` names the pinned tag |

## Where the spec's facts were wrong

These are corrections to section 2 and section 3, found by re-checking the
machine (rule 7). They are not choices.

| # | Spec says | Actual | Consequence |
| --- | --- | --- | --- |
| F1 | `vainfo` lists no H.264 or HEVC and no encode entrypoints until `mesa-va-drivers-freeworld` is installed | The package is installed; H.264 (Constrained Baseline, Main, High) and HEVC (Main, Main10) all have `VAEntrypointEncSlice` | Hardware encode works today. `doctor` still checks it |
| F2 | `ffmpeg-free` has no libx264; software fallback must be OpenH264 unless FFmpeg is swapped | `libavcodec-freeworld` is installed beside `ffmpeg-free`, so `libx264` is present and works | Software fallback is x264 first, then OpenH264, as R1.13 already orders them. No swap needed |
| F3 | `iproxy` comes from `libimobiledevice-utils` | It comes from `libusbmuxd-utils` (now installed) | Only matters for Spike 2. The daemon never uses `iproxy` (R1.1) |
| F4 | The iPad is on its "USB-C port at USB 2.0 speed" | Since 16 Nov it is on a laptop USB-A port (bus 3, 480 Mbit/s). Same USB 2.0 speed either way | None. Port 9000 still answers; the `hello` came back in 6 ms |
| F5 | 1640 not being a multiple of 16 may need SPS rewriting or a 2360x1632 mode | The encoder pads to 1648 and signals the crop correctly | Neither workaround is needed. R1.15's unit test stays |
| F6 | Stylus "probably reports no pressure or tilt. Treat as touch"; receiver capabilities unknown | The stylus sends `pencil` messages with tilt (pressure looked fixed at 1). The `hello` advertises protocol 3, `panel`, H.264 and HEVC at 60 fps, and a cursor port | R1.5, R1.6, R3.1 and R3.4 all apply; see D9 for the stylus |

## Targets at risk

| # | Spec target (section 9) | Measured | Why | Options |
| --- | --- | --- | --- | --- |
| T1 | Capture to encoded packet, p50 8 ms or less, at 2360x1640 | 14.6 ms with pixels uploaded from the CPU (3.6 ms upload + 11.1 ms GPU convert and encode). About 11 ms expected with zero-copy | The Radeon encoder's throughput: about 2.9 ms per megapixel, linear across sizes, unaffected by rate control or presets. Details in `docs/spikes.md` | (a) Accept about 11 ms; the 20 ms end-to-end target still looks reachable. (b) Use x264 with a multi-threaded colour conversion: lower latency, much more CPU. (c) Stream fewer pixels, for example 1640x1148 (6.9 ms), giving up sharpness. The build continues with (a), GPU first as the spec orders; the choice stays open until end-to-end numbers exist |

| T2 | End to end p50 20 ms, p95 35 ms | Met in ordinary use (14 to 17 ms p50, 16 to 18 ms p95). Missed under full-screen change: 30 to 39 ms p50 at 39 to 50 fps, where the official sender reaches 18 ms | Each frame takes longer to process than the gap between frames; details in `docs/spikes.md` | Zero-copy capture, multi-threaded colour conversion, VBR. **Decided by the user on 16 Nov 2026:** the GPU stays the default because this laptop's CPU is weak; x264 and OpenH264 remain selectable with `encoder` in the config file |

| T3 | Not a spec target: bitrate should follow content | `h264_vaapi` emits about 25 Mbit/s even when only a thin strip changes, in CBR (25.1) and in VBR (24.4) alike | The Radeon driver's rate control fills the target in both modes | Untried: constant-QP with a frame-size ceiling (`rc_mode=CQP`, `max_frame_size`). VBR was tried and reverted; the encoder stays on CBR |

## Not built yet (Phase 1)

Requirements still open, so nobody reads the code and assumes they exist.
Last updated 16 Nov 2026, after the daemon and CLI were built.

| Requirement | State |
| --- | --- |
| R1.2 telling locked from untrusted | One combined message for unplugged, locked or untrusted |
| R1.12 zero-copy (dmabuf) capture | Built, with automatic fallback to shared memory and a `zero-copy` config key. Tested in parts (encoder with GBM buffers; capture in the nested bench). Now streamed end to end in the nested bench by `testbench/stress.sh`; see the latest comparison in `docs/spikes.md` |
| Section 8: `ext-image-copy-capture` from Hyprland 0.57 | Not built. `wlr-screencopy` is used on every version; `capture = "ext"` warns |
| R1.24 logs to the journal | The daemon logs to stderr, which the journal captures under systemd. `padlink logs` follows the unit. Timestamps are not yet dropped under the journal |
| Section 4 hotplug through usbmuxd `Listen` | The daemon polls `ListDevices` ten times a second instead. `padlink_usbmux::Listener` exists and is tested but is not used |
| Section 9: fake receiver that decodes the stream; CI | Not built |

Built since this table was first written: config file (R1.23), lock file
(R1.21), `padlink status` (R1.22), `connect`, `disconnect`, `clean` (R1.20),
`logs`, `doctor` (R2.10), continuous running with reconnect, config-reload
recovery (R2.5).

## Requirements changed

- **R1.16 widened (D9, agreed 16 Nov 2026):** `pencil` down, move and up drive
  the pointer in Phase 1, like `touch`.
- **R3.4 raised in priority:** the user wants to draw in desktop apps with the
  pencil as on a pen-enabled laptop. That needs the uinput tablet device, so
  R3.4 is done first among the Phase 3 items instead of fourth. The spec's
  order (HEVC, cursor, Wi-Fi, pencil) otherwise stands.

No requirement has been dropped.
