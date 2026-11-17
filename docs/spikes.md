# Phase 0 spike results

Run on 16 Nov 2026 on the reference laptop (SPEC.md section 2). Status
summary:

| Spike | Status | Short result |
| --- | --- | --- |
| Host setup | Done | Driver was already installed; H.264 and HEVC encode entrypoints present |
| 1. Receiver works at all | Pass | The App Store receiver decodes and displays the stream on iPadOS 26.3.1: 10 ms p50 end to end on a nearly static screen |
| 2. USB tunnel | Pass | Streaming through usbmuxd works under load: 18 ms p50, 21 ms p95, 52 fps at 17.5 Mbit/s, no stalls |
| 3. Hardware encode at iPad size | Pass | Stream signals exactly 2360x1640; no SPS rewriting needed |
| 4. Zero-copy capture | Pass on feasibility; **latency target missed** | dmabuf to VA-API to H.264 works on radeonsi. Measured later: GPU convert plus encode takes about 11 ms at 2360x1640, above the 8 ms target |
| 5. Stylus type | Done | The pencil sends `pencil` and `proximity` messages, with tilt. Pressure was 1.0 in every sample seen |

## Differences from SPEC.md section 2

Found by re-checking the machine instead of trusting the table:

- `mesa-va-drivers-freeworld` 26.2.6 is already installed. `vainfo` lists
  `VAProfileH264ConstrainedBaseline`, `Main` and `High`, and
  `VAProfileHEVCMain` and `Main10`, each with `VAEntrypointEncSlice`.
- FFmpeg is still `ffmpeg-free` 8.1.3, but `libavcodec-freeworld` 8.1.3 is
  installed beside it, so `libx264` **is** available and works (155 fps at
  2360x1640, `ultrafast` + `zerolatency`). `libopenh264` also works (53 fps,
  Constrained Baseline). Software fallback order is therefore x264, then
  OpenH264, detected at run time.
- `iproxy` is not installed. It ships in `libusbmuxd-utils`, not in
  `libimobiledevice-utils` as the spec says.
- Development headers present: `libavcodec`, `libavutil`, `libavformat`,
  `libswscale`, `libdrm`, `wayland`, `pipewire`. Missing: `libavfilter`
  (for `scale_vaapi` through the FFmpeg API) and `libva` (for calling the
  video processor directly). One of them is needed for the zero-copy path.
  **Update:** the user has since installed `libavfilter-free-devel` 8.1.3,
  `libva-devel` 2.23.0 and `libusbmuxd-utils` 2.1.1 (which provides `iproxy`).

## Spike 1: the receiver works at all

Pass. Run with the official `od-sender` v1.30.3 through
`testbench/od-sender.sh`, which targets the nested Hyprland only.
`od-sender doctor` reported Hyprland 0.56.2 as tested, `enforce_permissions`
off, capture by `wlr-screencopy`, and encoders x264 and OpenH264 (it has no
VA-API support).

`od-sender list` found the iPad at `172.20.10.1:9000`. That is **not Wi-Fi**:
the laptop's only network at the time was the iPad's own USB tethering
interface (`ipheth`, 172.20.10.3/28), so this run also travelled over the
cable, as IP packets instead of through usbmuxd. A true Wi-Fi baseline was
not taken.

`od-sender --debug run --to 172.20.10.1:9000`, with the user touching the
iPad and little on screen (0.2 Mbit/s):

| Figure | Value |
| --- | --- |
| Receiver end to end (`e2e50` / `e2e95`) | 10 ms / 12 ms |
| Receiver frame rate while content moved | 50 to 56 fps |
| Sender x264 encode p50 / p95 | 7.3 ms / 8.8 ms |
| Sender capture to send p50 / p95 | 8.2 ms / 10.3 ms |
| Round trip time | 1 to 2 ms |

The receiver sent `hello`, `stats` and `touch` messages; one `kf` request was
served. The virtual display came up as 2360x1640 at scale 2 and was removed
cleanly on `od-sender disconnect`.

## Spike 2: USB tunnel

`spikes/usbmux_probe.py` talks to `/var/run/usbmuxd` directly: `ListDevices`,
then `Connect` to port 9000, then reads the first length-prefixed message. It
sends nothing to the receiver.

- usbmuxd lists one USB device at 480 Mbit/s (USB 2.0).
- `Connect` to port 9000 returned OK. No `iproxy`, network interface or
  firewall change involved, which is what R1.1 requires.
- The receiver's `hello` arrived 61 ms after dialing:

```json
{
  "type": "hello", "pv": 3, "device": "iPad",
  "pixelsWide": 2360, "pixelsHigh": 1640, "scale": 2,
  "panel": { "pixelsWide": 2360, "pixelsHigh": 1640, "scale": 2 },
  "displayMaxFrameRate": 60, "cursorPort": 9001,
  "videoCaps": [
    { "codec": "h264", "maxFrameRate": 60 },
    { "codec": "hevc", "maxWidth": 2360, "maxHeight": 2360, "maxFrameRate": 60 }
  ]
}
```

Consequences: `hello.panel` is present (R1.5), `videoCaps` is advertised so
`streamConfig` must be sent (R1.6), HEVC is accepted (R3.1), and the cap is
60 fps (R1.14).

Re-check after the iPad was moved to a laptop USB-A port (same day): still
one USB device at 480 Mbit/s on bus 3, `Connect` OK, same `hello`, this time
6 ms after dialing.

**Streaming through the tunnel.** `iproxy 9000 9000`, then
`od-sender --debug run --to 127.0.0.1:9000`, with a terminal printing
continuously on the streamed display so the link carried real bitrate. The
receiver classified the transport as `USB`.

| Figure | Value |
| --- | --- |
| Receiver end to end (`e2e50` / `e2e95`) | 18 ms / 21 ms |
| Frame rate | 52 fps (sender and receiver agree) |
| Bitrate | 17.5 Mbit/s of a 20 Mbit/s target |
| Stalls reported by the receiver | 0 once content was flowing |
| Sender x264 encode p50 / p95 | 15.0 ms / 17.4 ms |
| Sender capture to send p50 / p95 | 15.2 ms / 17.6 ms |
| Round trip time | 1 to 4 ms |

Reading: of the 18 ms, about 15 ms is the CPU encoder; the tunnel, decode and
display add about 3 ms. The encoder is also what holds the rate at 52 fps
(15 ms is close to the 16.7 ms frame budget). The usbmuxd tunnel on USB 2.0 is
not the bottleneck at this bitrate. Hardware encode at roughly 7 ms per frame
(Spike 3) should bring the end-to-end figure down by a similar amount.

The two runs are not a like-for-like transport comparison: the first had an
almost static screen, the second a full-screen moving one.

## Spike 3: hardware encode at the iPad's size

```sh
ffmpeg -vaapi_device /dev/dri/renderD128 -f lavfi -i testsrc2=size=2360x1640:rate=60 -t 5 \
  -vf 'format=nv12,hwupload' -c:v h264_vaapi -bf 0 -g 600 -b:v 25M -maxrate 25M \
  -rc_mode CBR -profile:v high -f h264 spike3.h264
```

- `ffprobe`: H.264 High, level 5.1, 2360x1640, `has_b_frames=0`.
- SPS: 148 x 103 macroblocks (2368x1648 coded), `frame_cropping_flag=1`,
  right offset 4 and bottom offset 4 (8 pixels each), so the signalled size
  is exactly 2360x1640. No SPS rewriting and no 1632-line mode needed. R1.15
  still gets its unit test.
- 1 I-frame followed by 299 P-frames, one slice per picture,
  `max_num_ref_frames=1`, `max_num_reorder_frames=0`,
  `max_dec_frame_buffering=1`: nothing that makes a decoder hold frames back.
- CBR rate control is accepted and hit 25.0 Mbit/s.
- Throughput 138 fps, including generating the pattern on the CPU and
  uploading it.

## Spike 4: zero-copy capture

Run in the nested Hyprland 0.56.2 test bench, never the live session. A
headless output `padlink-spike` was created and set with one
`hyprctl eval 'hl.monitor({...})'` call; `hyprctl monitors -j` read back
2360x1640 at 60 Hz, scale 2. A terminal printing continuously gave it damage.

```sh
wf-recorder -o padlink-spike -c h264_vaapi -d /dev/dri/renderD128 -p bf=0 -p b=25M -f spike4.mkv
```

`wf-recorder` 0.6.0 uses the same pipeline the spec describes:
`wlr-screencopy` dmabuf frames, imported into VA-API, converted to NV12 by
`scale_vaapi` (the VA-API video processor), encoded by `h264_vaapi`.

- It worked: 324 frames in 6 s, damage-driven, H.264 High 2360x1640, no
  B-frames. The dmabuf import on radeonsi is not a problem, so the
  shared-memory fallback is not needed as the primary path.
- Teardown in the R1.18 order (recorder stopped first, then
  `hyprctl output remove`) left the nested compositor running.

**Not measured: capture-to-encoded-packet time** (target 8 ms p50).
`wf-recorder` does not report it. Throughput figures from the FFmpeg CLI give
a rough bound only, because throughput is not latency:

| Pipeline | Throughput | Per frame |
| --- | --- | --- |
| VA-API decode, `scale_vaapi` to NV12, `h264_vaapi` (all on GPU) | 99 fps | 10.1 ms, of which about 3 ms is the decode |
| BGRx upload from memory, `scale_vaapi`, `h264_vaapi` (the fallback path) | 104 fps | 9.6 ms, including CPU pattern generation |
| NV12 upload, `h264_vaapi` | 138 fps | 7.2 ms, including CPU pattern generation |

So convert plus encode costs roughly 7 ms per frame at this size. The 8 ms
target looks reachable but tight; treat it as an open risk until the daemon's
built-in per-stage timing (section 9) gives real numbers. `-async_depth 1`
made no difference to throughput.

### Follow-up: measured encode latency (Phase 1, `padlink-encode`)

The missing figure was measured once the encoder crate existed, with
`cargo run --release -p padlink-encode --example bench`: one frame in, one
packet out, timed synchronously, a 200-row band changing each frame. This is
the shared-memory path (pixels uploaded from the CPU), not zero-copy.

| Size | Backend | Total p50 / p95 | Prepare | Codec |
| --- | --- | --- | --- | --- |
| 2360x1640 | `h264_vaapi` | 14.6 / 15.6 ms | 3.6 ms upload | 11.1 ms convert + encode |
| 2360x1640 | x264 `ultrafast` | 12.2 / 13.0 ms | 8.8 ms colour conversion | 3.4 ms |
| 2360x1640 | OpenH264 | 21.5 / 22.4 ms | 8.7 ms | 12.8 ms |
| 1640x1148 | `h264_vaapi` | 6.9 / 7.7 ms | 1.0 ms | 5.9 ms |
| 1180x820 | `h264_vaapi` | 3.7 / 4.0 ms | 0.4 ms | 3.3 ms |

Findings:

- **The 8 ms capture-to-packet target is not reachable at 2360x1640 on this
  GPU.** GPU convert plus encode costs about 2.9 ms per megapixel, in a
  straight line across three sizes: that is the Radeon encoder's throughput,
  not a setting. The "about 7 ms" estimate above came from pipelined
  throughput with two frames in flight and understated latency.
- Rate-control mode (CBR, VBR, CQP), profile, `compression_level` 0 to 2 and
  `quality` 0 to 3 all land within a millisecond of each other; `quality`
  4 to 6 doubles the time. `low_power` is not offered by the driver.
- Zero-copy capture would remove the 3.6 ms upload, leaving about 11 ms.
- x264's codec time is only 3.4 ms on mostly-static content; its cost is the
  single-threaded BGRx to YUV conversion (8.8 ms). With a multi-threaded
  conversion it could be the lower-latency path, at the price of CPU load
  (the target of under 15% of one core would not hold).
- The end-to-end target (20 ms p50) still looks reachable: the official
  sender reached 18 ms with a 15 ms CPU encode, so about 11 ms of GPU time
  should land near 14 to 15 ms. Unmeasured until the daemon streams.

### Follow-up: first end-to-end numbers from `padlinkd`

16 Nov 2026, `testbench/padlinkd.sh` in the nested Hyprland, real iPad over
USB through usbmuxd, 2360x1640, shared-memory capture. "End to end" is the
iPad's own figure. The nested bench shares the integrated GPU between two
compositors, so the live session may do better.

| Load | Encoder | Frame rate | Capture to encoded p50 / p95 | End to end p50 / p95 |
| --- | --- | --- | --- | --- |
| Light: pointer moving | `h264_vaapi` | 60 fps | 13.5 / 14.0 ms | 16 to 17 / 18 ms |
| Light: pointer moving | x264 | 60 fps | 13.6 / 14.7 ms | 14 / 16 ms |
| Heavy: full-screen scrolling text | `h264_vaapi` | 39 fps | 35.8 / 38.5 ms | 39 / 41 ms |
| Heavy: full-screen scrolling text | x264 | 50 fps | 26.6 / 37.0 ms | 30 / 41 ms |
| Heavy, for reference: official `od-sender`, x264 (Spike 2) | | 52 fps | 15.2 / 17.6 ms | 18 / 21 ms |

Per-stage medians: `h264_vaapi` light 3.6 ms upload + 9.8 ms codec, heavy
5.8 + 20; x264 light 9.7 ms colour conversion + 3.9 ms codec, heavy
12.9 + 6.7. Encoded-to-sent is 0.1 ms in every run: the USB tunnel adds
nothing measurable on the sending side.

Reading:

- **Ordinary use meets the end-to-end targets** (20 ms p50, 35 ms p95) with
  either encoder.
- **Under full-screen change padlink is behind the official sender.** A
  frame takes 20 to 26 ms to process, longer than the 16.7 ms between
  frames, so frames wait in the slot and some are replaced. The GPU codec
  slows from 10 to 20 ms when the same GPU is busy compositing the changing
  screen.
- The official sender is faster under load because it converts colours on
  several threads and overlaps conversion with encoding.
- `h264_vaapi` in CBR mode pads to the full 25 Mbit/s even when only the
  pointer moves. It should be VBR.

What should close the gap, in order of expected effect: zero-copy capture
(removes the compositor's readback and the 3.6 to 5.8 ms upload on the GPU
path); a multi-threaded colour conversion (cuts 10 to 13 ms to about 3 on
the CPU path); VBR rate control.

## Spike 5: stylus type

Done: the third-party pencil arrives as `pencil` messages, not `touch`.

The official sender's debug log could not answer this. It logs `touch` and
`scroll` but handles `pencil` without a log line, and on Hyprland its input
backend drops `pencil` entirely (the default no-op in `backend/mod.rs`). That
is why the user saw finger input move the pointer and the pencil do nothing.

So `spikes/tap_proxy.py` was put between `od-sender` and the usbmuxd tunnel.
It forwards bytes unchanged and counts the message types the iPad sends. In
about 80 seconds, with the user drawing with the pencil and then a finger:

| Message | Count |
| --- | --- |
| `pencil` | 3055 |
| `proximity` | 28 |
| `touch` | 994 |
| `ping` | 28 |
| `stats` | 3 |
| `hello` | 1 |

A sample:

```json
{"type":"pencil","phase":"down","x":0.5729,"y":0.5232,"pressure":1,
 "azimuth":1.9619,"altitude":1.2731,"rotation":0,"t":1794881707492.2495}
```

- `phase` values seen: `down`, `move`. `proximity` reports the pen entering
  and leaving hover range.
- `azimuth` and `altitude` carry real values, so tilt is reported.
- `pressure` was exactly 1 in all five samples printed. Only five were
  printed, so whether pressure ever varies is **not established**; the
  expectation for a third-party pencil is that it does not.
- No `scroll` message was seen in this run; two-finger scroll was not
  confirmed either way.

Consequences:

- R3.4 (uinput tablet) stays in scope, since `pencil` messages do arrive. Its
  value is tilt and hover; pressure needs a longer capture before promising
  anything.
- R1.16 assumes stylus input arrives as `touch`. It does not: this receiver
  sends `pencil` to a protocol 3 sender. Unless Phase 1 also maps `pencil`
  down, move and up to the pointer, the pencil does nothing until Phase 3.
  See `docs/spec-deviations.md`, D9.

## Pinned upstream

OpenDisplay `v1.30.3` (commit `90f7446`), the latest release on 16 Nov 2026.

## Automated stress comparison, 16 Nov 2026 (Codex)

The user reports minimal live testing looks good; that does not certify the
manual acceptance checklist. This comparison ran entirely in nested Hyprland
with the real USB iPad, GPU H.264 at 2360x1640, scale 2, 60 Hz cap:

```sh
testbench/nested.sh
testbench/stress.sh --duration 15 --cycles 3 --output test-results/first-stress
```

Each load used a new session and 10 seconds of warmup, followed by 15 seconds
of measurement. Light load moves the pointer through IPC (about 39 updates/s
on this machine). Heavy load uses `foot --fullscreen` with deterministic
60 Hz animated text/colour output from `testbench/stress-load.py`. This is
repeatable between capture profiles, but differs from the earlier unbounded
scrolling-text workload. CPU is daemon-only percentage of one logical core.

**These are medians of rolling-window reports**, not percentiles computed
from all raw frames in the run. Receiver reports are deduplicated by sequence
and age-checked; 3–4 receiver reports contributed per active load.

| Load / capture | FPS | Capture-to-encode p50 / p95 ms | Receiver p50 / p95 ms | CPU % | Replaced frames during sampling |
| --- | ---: | ---: | ---: | ---: | ---: |
| zero-copy-idle | 0.0 | 9.2 / 18.3 | - / - | 0.0 | 0 |
| zero-copy-light | 39.0 | 9.1 / 16.1 | 13.0 / 16.5 | 4.9 | 0 |
| zero-copy-heavy | 59.5 | 14.4 / 16.8 | 16.0 / 20.0 | 8.0 | 0 |
| shared-memory-idle | 0.0 | 15.5 / 18.1 | - / - | 0.0 | 0 |
| shared-memory-light | 38.5 | 15.5 / 22.6 | 21.0 / 23.5 | 21.8 | 0 |
| shared-memory-heavy | 37.2 | 36.4 / 43.8 | 41.0 / 47.0 | 29.8 | 287 |

Findings: zero-copy stayed active and substantially improved both latency
and CPU cost, particularly under heavy animation. It met the receiver
20/35 ms targets in this run, but still missed the 8 ms capture-to-encode
target. Shared memory missed receiver latency and CPU targets under heavy
load. Idle CPU was near zero for both paths.

Functional checks passed for both profiles: second-daemon refusal, nested
config reload with continued video, three disconnect/reconnect cycles,
SIGKILL followed by startup cleanup and reconnection, and final output
removal. Connect-to-video-status was about 1.21 s; disconnect was 0.11–0.21 s.
These are not true first-displayed-frame or subsecond reload measurements.
The workspace checks (168 tests), nested tests (10) and initial runner tests
passed. No live display operations were performed.

The first report also flags light FPS below 55, which was a **test-design
error**: pointer damage is IPC-limited. The final runner grades that load
for nonzero video and applies the >=55 FPS heuristic only to heavy animation.
The original report is preserved without changing its figures. Raw JSONL,
logs and report are local under `test-results/first-stress/` (git-ignored).

A short final zero-copy validation used
`testbench/stress.sh --duration 10 --cycles 1 --profiles zero-copy --output test-results/verified-zero-copy`.
Light receiver p50/p95 was 13/13 ms, heavy 15/20 ms at 59.4 FPS and about
9% daemon CPU. Capture-to-encode p50 was 9.1 ms light and 16.1 ms heavy,
so both checks correctly failed the 8 ms target. Every functional check,
receiver latency check, CPU check and corrected FPS check passed. Final
Python regression coverage is 10 tests; formatting, Clippy, 168 workspace
tests and 10 nested tests passed. The agent stopped its own nested compositor
after daemon exit and successful output cleanup; no live outputs were changed.
