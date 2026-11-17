#!/usr/bin/env bash
# Run the tests that create and remove outputs, against the nested Hyprland
# started by testbench/nested.sh. Extra arguments go to `cargo test`.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
env_file="${XDG_RUNTIME_DIR:?}/padlink-testbench/env"
[[ -f "$env_file" ]] || { echo "no nested instance running: start testbench/nested.sh first" >&2; exit 1; }
# shellcheck disable=SC1090
source "$env_file"
cd "$here/.."
# One at a time: the tests share one compositor and read its focus.
exec cargo test --workspace --test nested "$@" -- --ignored --test-threads=1
