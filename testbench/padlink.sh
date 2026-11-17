#!/usr/bin/env bash
# Run the development `padlink` CLI with the nested test bench's environment,
# so `clean` can only ever touch the nested instance (SPEC.md rule 2).
#
#   testbench/padlink.sh status
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
env_file="${XDG_RUNTIME_DIR:?}/padlink-testbench/env"
[[ -f "$env_file" ]] || { echo "no nested instance running: start testbench/nested.sh first" >&2; exit 1; }
# shellcheck disable=SC1090
source "$env_file"
cd "$here/.."
cargo build --release -q -p padlink
exec target/release/padlink "$@"
