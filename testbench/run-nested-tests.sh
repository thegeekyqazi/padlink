#!/usr/bin/env bash
# Run the tests that create and remove outputs, against the nested Hyprland
# started by testbench/nested.sh. Extra arguments go to `cargo test`.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
env_file="${XDG_RUNTIME_DIR:?}/padlink-testbench/env"
[[ -f "$env_file" ]] || { echo "no nested instance running: start testbench/nested.sh first" >&2; exit 1; }
# shellcheck disable=SC1090
source "$env_file"
# A freshly started instance is listed before it answers; wait for it.
for _ in $(seq 50); do
    hyprctl -i "$HYPRLAND_INSTANCE_SIGNATURE" version >/dev/null 2>&1 && break
    sleep 0.1
done
cd "$here/.."
# One at a time: the tests share one compositor and read its focus.
exec cargo test --workspace --test nested "$@" -- --ignored --test-threads=1
