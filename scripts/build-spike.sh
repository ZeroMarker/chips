#!/usr/bin/env bash
# Build the locked Spike; stdout contains only the executable path.
# Usage: bash scripts/build-spike.sh [--jobs N] [--cache-dir DIR] [--cache-key]
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
exec python3 "$root/scripts/build_spike.py" "$@"
