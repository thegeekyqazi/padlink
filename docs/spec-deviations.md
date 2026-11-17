# Differences from SPEC.md

Where the work so far departs from `SPEC.md`, and where the spec's facts
turned out to be wrong. Updated as the project goes; last update 16 Nov 2026,
during Phase 0. No daemon code exists yet, so every entry is about Phase 0.

## Where the work differs from the spec

| # | Spec says | What was done | Why | Effect |
| --- | --- | --- | --- | --- |
| D1 | Spike 1: run `od-sender doctor` and `od-sender run` over Wi-Fi (section 3) | Binary downloaded and checksum-verified, but not run | The session's permission rules refused to execute the downloaded binary, and the result has to be read off the iPad's screen anyway | Spike 1 is still open. It is the go/no-go for the whole design |
| D2 | Spike 1 gives no instruction on where to run `od-sender run` | To be run inside the nested test bench, not the live session | `od-sender run` creates and removes a headless output. Rule 2 (section 10) forbids that on the live Hyprland 0.56.2, which can crash on output removal. The two parts of the spec conflict; rule 2 wins | Spike 1's Wi-Fi baseline is measured from a nested compositor. The numbers should be the same; the output just lives in a window |
| D3 | Spike 2: "USB tunnel with no new code", using `iproxy 9000 9000` and `od-sender run --to 127.0.0.1:9000` | A 100-line Python probe (`spikes/usbmux_probe.py`) speaks to usbmuxd directly, connects to port 9000 and reads the `hello` | `iproxy` was not installed at the time and `od-sender` could not be run (D1) | The tunnel and the handshake's first message are proven, which is the part R1.1 depends on. Streaming through the tunnel and the latency comparison with Wi-Fi are not done |
| D4 | Spike 4: create a headless output, capture into dmabuf, import into VA-API, convert, encode, and **measure capture-to-encoded-packet time** (target under 8 ms) | Feasibility proven with the installed `wf-recorder` 0.6.0, which runs that same pipeline. No timing measured | It answered the real risk (does dmabuf import work on radeonsi) with no new code. `wf-recorder` reports no per-frame timing, and writing a measuring tool means writing most of the capture and encode crates, which rule 1 puts after Phase 0 | The 8 ms figure is unverified. Throughput suggests about 7 ms per frame for convert plus encode. It will be measured by the daemon's built-in per-stage timing in the first streaming commit (section 9) |
| D5 | Rule 1: "Finish Phase 0 and report the spike results before writing daemon code" | Reported with Phase 0 unfinished: Spikes 1 and 5 not run, 2 and 4 partial | Spikes 1, 2 (streaming half) and 5 need a person at the iPad and the blocked binary | No daemon code has been written. Phase 1 waits for Spike 1 |
| D6 | Host setup: user runs `sudo dnf install mesa-va-drivers-freeworld` | Skipped | Already installed | None |
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
| F6 | Stylus and receiver capabilities are unknown | The receiver's `hello` advertises protocol 3, `panel`, H.264 and HEVC at 60 fps, and a cursor port | R1.5, R1.6 and R3.1 all apply. Stylus type is still unknown (Spike 5) |

## Requirements changed

None. No requirement ID (R1.x, R2.x, R3.x) has been dropped, altered or
re-scoped.
