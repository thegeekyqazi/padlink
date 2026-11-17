# Automated regression and streaming stress tests

Run from the repository root. No Python packages beyond the standard library
are needed. Streaming also needs `foot`, `hyprctl`, the build dependencies,
GPU render-node access, and an unlocked USB iPad with OpenDisplay foregrounded.

```sh
# Terminal 1: sacrificial desktop in a window
 testbench/nested.sh

# Terminal 2: full suite, about 5 minutes plus build time
 testbench/stress.sh

# Longer soak, 3 minutes per load and 20 reconnects per capture profile
 testbench/stress.sh --duration 180 --cycles 20

# Also compare the optional x264 software encoder
 testbench/stress.sh --profiles zero-copy,shared-memory,x264

# Build and unit checks only, without a compositor or iPad
 testbench/stress.sh --checks-only
```

Stop any existing padlinkd yourself before a streaming run. `padlink disconnect`
pauses streaming but leaves that daemon running, so it is not sufficient.
The runner refuses an existing daemon, never stops it for you, and never
modifies your configuration. Do not launch another sender during the test.
Leave the nested terminal/window open until the test is done. Ctrl+C in the
runner requests cleanup of its own workload and daemon. If cleanup times out,
it reports failure and leaves the compositor alone: stop capture before any
manual output removal.

The runner uses `testbench/padlinkd.sh`, validates the compositor process's
exact nested config path and Wayland socket, checks the control socket's
kernel-reported PID against its own daemon, and always passes an explicit
Hyprland signature. It never creates or removes outputs on the live desktop.
The runner does not automatically launch or stop a compositor.

## Coverage

- Formatting, Clippy, workspace tests and runner regression tests.
- Existing nested capture, input, layout and output lifecycle tests.
- Release build with isolated per-run configs (GPU encoder is mandatory for
  the zero-copy and shared-memory comparisons).
- Capture-path verification, including unexpected zero-copy fallback.
- Idle, moving-pointer and deterministic full-screen animated-text loads.
- Sender capture-to-encode and encode-to-send latency; fresh receiver
  end-to-end latency; FPS, bitrate, replaced frames, daemon CPU and RSS.
- Second-daemon lock rejection, nested config reload with video continuity,
  repeated disconnect/connect with output removal checks, SIGKILL followed
  by startup cleanup and reconnection, and final graceful cleanup.

The crash test kills only the runner's daemon. It waits for that process to
exit before the restarted daemon removes its leftover nested output.

## Reports and interpretation

Results go into `test-results/<timestamp>/` (git-ignored). Set a fresh path
with `--output PATH`. Existing report directories are never overwritten.
`report.md` is the readable report, `report.json` the complete machine-readable
result, `*.jsonl` the one-second samples, and `*.log` the build/test/daemon logs.
Failure and interruption paths also produce reports.

Exit codes: **0** all executed automated checks passed, **1** failure or
performance threshold missed, **2** incomplete coverage (including checks-only
or missing receiver measurements). Manual checks remain required regardless.
The runner continues through performance misses to collect the other loads.
A setup or functional exception stops further scenarios and attempts cleanup.

Each load has a fresh connection and 10 seconds of warmup (configurable,
minimum 6); measured windows default to 30 seconds (minimum 10). This reduces
cross-load contamination of rolling latency windows. Reports summarize the
median/minimum/maximum of those reported windows; they do **not** calculate
true whole-run frame percentiles. Receiver reports are deduplicated by sequence
and rejected if older than six seconds at status publication. At least two
fresh receiver reports are required for a latency verdict.

Spec targets are capture-to-encode p50 <=8 ms, receiver p50 <=20 ms and p95
<=35 ms, and moving-load daemon CPU below 15% of one core. The bench additionally
uses >=55 FPS on heavy animation and <2% idle CPU as explicit heuristics.
Light-load FPS must be nonzero; its damage rate is limited by cursor IPC, so
it is not graded against 60 FPS.
The heavy full-screen test is deliberately harsher than the spec's moving
window CPU workload. A FAIL reports a threshold miss; it does not necessarily
mean a functional defect. The existing hardware already misses the 8 ms target.
CPU excludes the workload and both compositors; values can exceed 100%.
Idle FPS is not graded because damage-driven capture should stop on static
content. RSS is reported, without inventing a memory-leak pass threshold.

Connect durations end at a status publication showing video, not a decoded
first frame. Reload checks confirm video continuity over four seconds; they
do not certify the spec's one-second recovery target. Physical unplug/lock,
rotation, suspend, image quality, actual touch/Pencil/scroll behavior and
notifications remain explicitly manual. A successful automated report is
not complete live-session acceptance.
