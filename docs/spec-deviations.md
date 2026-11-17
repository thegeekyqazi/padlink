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

## Requirements changed

- **R1.16 widened (D9, agreed 16 Nov 2026):** `pencil` down, move and up drive
  the pointer in Phase 1, like `touch`.
- **R3.4 raised in priority:** the user wants to draw in desktop apps with the
  pencil as on a pen-enabled laptop. That needs the uinput tablet device, so
  R3.4 is done first among the Phase 3 items instead of fourth. The spec's
  order (HEVC, cursor, Wi-Fi, pencil) otherwise stands.

No requirement has been dropped.
