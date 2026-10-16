#!/usr/bin/env bash
# Compare retired instruction effects and independent architectural state reads.
# Python stdlib handles strict parsing, bounded execution and regression checks.
# Usage: scripts/difftest.sh --help
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
exec python3 "$root/scripts/difftest.py" "$@"
