#!/usr/bin/env bash
# SPEC section 9: automated checks and nested-only streaming stress report.
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 testbench/stress.py "$@"
