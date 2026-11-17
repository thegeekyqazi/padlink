#!/usr/bin/env bash
# Run the development padlinkd against the nested test bench only.
# Refuses to start when no nested instance is running, so a development
# build never creates outputs on the live session (SPEC.md rule 2).
#
#   testbench/padlinkd.sh            # Ctrl+C or SIGTERM stops it
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
env_file="${XDG_RUNTIME_DIR:?}/padlink-testbench/env"
[[ -f "$env_file" ]] || { echo "no nested instance running: start testbench/nested.sh first" >&2; exit 1; }
# shellcheck disable=SC1090
source "$env_file"
hyprctl -i "$HYPRLAND_INSTANCE_SIGNATURE" version >/dev/null \
    || { echo "nested instance in $env_file is gone: restart testbench/nested.sh" >&2; exit 1; }
cd "$here/.."
cargo build --release -q -p padlinkd
exec target/release/padlinkd "$@"
