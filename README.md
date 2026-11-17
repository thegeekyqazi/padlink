# padlink

Use an iPad as a true extended monitor for a Hyprland session, over a USB
cable. Plug the iPad in, open the receiver app, and a new display appears
next to your laptop screen; finger and pencil input come back as pointer
input.

The iPad side is the existing [OpenDisplay](https://opendisplay.app)
receiver app from the App Store. padlink is the Linux side: it speaks
OpenDisplay's wire protocol (version 3) through usbmuxd, with no network
setup.

## Status

Work in progress. Read this before relying on it.

- Streaming, hardware encoding, touch input, `connect`, `disconnect`,
  `status` and crash cleanup work **in a nested test compositor** against a
  real iPad 10th generation.
- **It has not been run on a live desktop session yet.**
- Measured on the development laptop (Ryzen 5 PRO 5650U, integrated Radeon,
  2360x1640 at 60 Hz): 14 to 17 ms end to end in ordinary use, 30 to 39 ms
  when the whole screen changes every frame. Details in `docs/spikes.md`.
- Not built yet: zero-copy capture, the newer capture protocol for Hyprland
  0.57 and later, `padlink doctor`, auto-start polish, rotation debounce,
  workspace memory, suspend handling, Wi-Fi, a real pen tablet device. The
  full list is in `docs/spec-deviations.md`.

## Limits that will not go away

- **The receiver app must be open and in the foreground on the iPad.**
  iPadOS only lets it listen then, and nothing on the Linux side can launch
  it.
- The stream is not encrypted. Over a cable that means the cable.
- No keyboard passthrough and no audio: the protocol has neither.
- Hyprland only.

## Requirements

Fedora 44 is what this is developed on.

- Hyprland 0.55 or newer.
- `usbmuxd` running (it starts when an iPad is plugged in).
- FFmpeg with its development headers, and a VA-API driver that can encode
  H.264. On Fedora with RPM Fusion:

  ```sh
  sudo dnf install mesa-va-drivers-freeworld libavcodec-freeworld \
      libavcodec-free-devel libavfilter-free-devel libavutil-free-devel \
      libswscale-free-devel clang
  vainfo | grep -E 'H264.*EncSlice'    # must print something
  ```

  Without a working GPU encoder padlink falls back to x264, then OpenH264.
- Rust 1.85 or newer.

## Install

```sh
cargo install --path crates/padlinkd
cargo install --path crates/padlink
cp packaging/padlinkd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now padlinkd
```

## Use

1. Plug the iPad in, unlock it, tap Trust if it asks.
2. Open the OpenDisplay app.
3. The display appears to the right of your other monitors. Drag a window
   onto it.

```sh
padlink status          # state, encoder, frame rate, latency
padlink status --json   # the same for scripts and bar widgets
padlink disconnect      # remove the display and stay disconnected
padlink connect         # connect again
padlink clean           # remove a display left behind by a crash
padlink logs            # follow the daemon's log
```

## Configure

Copy `config.example.toml` to `~/.config/padlink/config.toml`. Every key is
explained there; the main ones:

```toml
encoder = "auto"       # "auto", "vaapi" (GPU), "x264" or "openh264" (CPU)
scale = 2              # must divide the iPad's resolution into whole pixels
position = "right"     # "right", "left", "above", "below" or "x,y"
fps = 60
bitrate-mbps = 25
autoconnect = true     # false: wait for `padlink connect`
```

An unknown key or an invalid value stops the daemon with a message that
names it. Restart the daemon after editing:
`systemctl --user restart padlinkd`.

padlink never edits your Hyprland config.

## Develop

`AGENTS.md` has the rules and `docs/HANDOFF.md` the state of the code and
what is left. The one rule that matters most: anything that creates an
output runs in the nested test compositor, never on your real session.

```sh
cargo test --workspace
testbench/nested.sh               # a nested Hyprland in a window
testbench/run-nested-tests.sh     # tests that create outputs, against it
testbench/padlinkd.sh             # the daemon, against it
testbench/padlink.sh status       # the CLI, against it
```

## Credits and licence

GPL-3.0-only; see `LICENSE`.

padlink depends on the `opendisplay-proto` and `opendisplay-session` crates
from [peetzweg/opendisplay](https://github.com/peetzweg/opendisplay)
(GPL-3.0), and several modules are adapted from its Linux sender
`od-sender`: Hyprland IPC and instance discovery, the `wlr-screencopy`
capture loop and its buffer leasing, the virtual pointer, and the nested
test bench. Each adapted file says so at the top. The usbmuxd connection
sequence follows `aaditya19saini/opendisplay-linux` and libusbmuxd, and the
VA-API pipeline follows `wf-recorder` and Sunshine.
