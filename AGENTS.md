# Instructions for coding agents

This repository is `padlink`: a Rust daemon that turns an iPad into an
extended monitor for a Hyprland session over USB. The full requirements are
in `SPEC.md`. **Read `docs/HANDOFF.md` before doing anything**: it says what
is built, what is left, and how to continue.

## Rules that must not be broken

1. **Never create or remove outputs on the user's live Hyprland session.**
   It runs Hyprland 0.56.2, which can crash on a bad teardown and take the
   user's open work with it. Everything that creates an output runs in the
   nested test bench: `testbench/nested.sh` in one terminal, then
   `testbench/run-nested-tests.sh`, `testbench/padlinkd.sh` or
   `testbench/od-sender.sh` in another. Those scripts refuse to run without a
   nested instance. Never run `target/*/padlinkd` directly.
2. **Never edit files under `~/.config/hypr/`. Never run `sudo`.** Print the
   command and the reason and let the user run it.
3. **Teardown order is fixed:** stop capture and wait for the capture thread
   to finish, then remove the output. Never the reverse.
4. **Small commits**, each building and passing `cargo clippy --workspace
   --all-targets -- -D warnings` and `cargo test --workspace`. Reference the
   requirement IDs from `SPEC.md` (R1.7 and so on) in the commit message.
5. **Do not change scope silently.** Where the work departs from `SPEC.md`,
   add a row to `docs/spec-deviations.md`. If a requirement looks impossible,
   stop and ask the user.
6. **Report honestly.** If a test fails or a number misses a target, say so
   with the figures. `docs/spikes.md` holds every measurement so far.
7. The user's standing instruction for this machine: before finishing any
   session, update the shared note
   `/home/geeki/agent-handoffs/2026-11-16-padlink-ipad-second-monitor.md`
   (see `/home/geeki/AGENTS.md`). Also keep `docs/HANDOFF.md` current.
8. The agent cannot see the iPad. Anything that needs eyes on it (does a
   picture appear, does the pencil draw) must be asked of the user.

## Quick check that the tree is healthy

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # about 130 tests, under a minute
```
