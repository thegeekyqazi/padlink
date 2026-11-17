# First run on the live session, and the Phase 1 acceptance list

Everything so far ran in a nested test compositor. This is the procedure
for the first run on the real desktop, which only the user does (SPEC.md
rule 2 and section 9).

## Before you start

- **Save your work.** The live session runs Hyprland 0.56.2, which can
  abort if a captured output is removed in the wrong order. padlink always
  stops capture first, and that order survived every nested test, including
  `kill -9`, but it has never run on the live session.
- Plug the iPad in, unlock it, open the OpenDisplay app.
- Run the checks. In the project folder:

  ```sh
  cargo build --release -p padlink -p padlinkd
  target/release/padlink doctor
  ```

  Every line should say `ok` or `note`. Fix anything marked `FAIL` first.

## Start it by hand (recommended for the first time)

In a terminal you can watch:

```sh
target/release/padlinkd
```

It logs what it does. Ctrl+C removes the display and exits. In another
terminal, `target/release/padlink status` shows the figures.

Only once that works, install it so it starts with the session:

```sh
cargo install --path crates/padlinkd && cargo install --path crates/padlink
cp packaging/padlinkd.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now padlinkd
```

## The list (SPEC.md section 9)

Tick each one and note what you saw; "expected today" says what the code
should do at this point.

| # | Step | Expected today |
| --- | --- | --- |
| 1 | Plug in, open the app | Display appears right of the laptop screen within about 2 s; a window can be dragged onto it |
| 2 | Look at text at scale 2 | Sharp; no garbage strip along the bottom edge |
| 3 | Tap, drag, two-finger scroll, pencil | Tap, drag and pencil move and click the pointer where you touch. **Scroll is unverified** |
| 4 | Lock the iPad, unlock it | Display goes away, comes back after unlock. **The same workspace returning is not built** (R2.4) |
| 5 | Unplug | Windows move to the laptop screen; `hyprctl monitors` shows no `padlink-*` |
| 6 | `hyprctl reload` during a session | Stream continues within about 1 s (built and tested nested) |
| 7 | Suspend and resume with the iPad attached | **Not built** (R2.6). The link should drop and reconnect, but a frozen picture is possible. Run `padlink disconnect` before suspending for now |
| 8 | `kill -9` the daemon, restart it | The leftover display is removed at startup and the session resumes |
| 9 | Rotate the iPad | The display is rebuilt as 1640x2360 and back. No debounce yet, so a quick double rotation rebuilds twice |

Also worth a look: `padlink status` while streaming (latency on the iPad
should be under 20 ms in ordinary use), and CPU use of `padlinkd` in `top`.

## If something goes wrong

- The picture is black: `padlink doctor` and look at "capture permission".
- The display stays behind after a crash: `padlink clean`.
- Hyprland itself crashed: please keep `~/.cache/hyprland/hyprlandCrashReport*.txt`
  and the daemon's log (`padlink logs`, or the terminal it ran in).
- Wrong place or size: `position` and `scale` in
  `~/.config/padlink/config.toml` (see `config.example.toml`).
