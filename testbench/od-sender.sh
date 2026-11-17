#!/usr/bin/env bash
# Run the official od-sender against the nested test bench only.
# Refuses to start when no nested instance is running, so it can never
# create or remove an output on the live session (SPEC.md rule 2).
#
#   testbench/od-sender.sh doctor
#   testbench/od-sender.sh run --to 127.0.0.1:9000
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
env_file="${XDG_RUNTIME_DIR:?}/padlink-testbench/env"
bin="$here/../reference/dl/od-sender-x86_64-unknown-linux-gnu/od-sender"

[[ -f "$env_file" ]] || { echo "no nested instance running: start testbench/nested.sh first" >&2; exit 1; }
[[ -x "$bin" ]] || { echo "od-sender not found at $bin (see docs/spikes.md)" >&2; exit 1; }
live="${HYPRLAND_INSTANCE_SIGNATURE:-}"
# shellcheck disable=SC1090
source "$env_file"
[[ "$HYPRLAND_INSTANCE_SIGNATURE" != "$live" || "${PADLINK_TESTBENCH_SHELL:-}" == 1 ]] \
    || { echo "refusing: target is the live Hyprland session" >&2; exit 1; }
hyprctl -i "$HYPRLAND_INSTANCE_SIGNATURE" version >/dev/null \
    || { echo "nested instance in $env_file is gone: restart testbench/nested.sh" >&2; exit 1; }
exec "$bin" "$@"
