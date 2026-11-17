# Phase 0 spike results

Run on 16 Nov 2026 on the reference laptop (SPEC.md section 2). Status
summary:

| Spike | Status | Short result |
| --- | --- | --- |
| Host setup | Done | Driver was already installed; H.264 and HEVC encode entrypoints present |
| 1. Receiver works at all | **Not run** | Blocked: the session was not permitted to execute the downloaded `od-sender` binary |
| 2. USB tunnel | **Partly done** | Tunnel and `hello` proven with a probe script; streaming over it not run (needs `od-sender`) |
| 3. Hardware encode at iPad size | Pass | Stream signals exactly 2360x1640; no SPS rewriting needed |
| 4. Zero-copy capture | Pass on feasibility; **latency not measured** | dmabuf to VA-API to H.264 works on radeonsi; per-frame timing still to be measured |
| 5. Stylus type | **Not run** | Needs the user, the iPad and `od-sender` at debug level |

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

## Spike 1: the receiver works at all

Not run. `od-sender` v1.30.3 (x86_64 tarball, checksum verified) is unpacked
in `reference/dl/`, but executing it was denied by the session's permission
rules, so nothing was streamed and there is no baseline latency figure.

Also note a conflict in the spec: `od-sender run` creates and removes a
headless output, and rule 2 forbids that on the live session. It should be
run inside the nested test bench (`testbench/nested.sh`), which is safe.

What is known without it: the receiver app is open, listening, and speaks
protocol version 3 (see Spike 2). Whether it decodes and displays a stream on
iPadOS 26.3.1 is still unproven.

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

Not done: streaming through the tunnel and comparing latency with Wi-Fi, and
throughput under load on the USB 2.0 port. Both need a sender.

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

## Spike 5: stylus type

Not run. Needs the user to draw with the pencil while a sender logs incoming
control messages at debug level.

## Pinned upstream

OpenDisplay `v1.30.3` (commit `90f7446`), the latest release on 16 Nov 2026.
