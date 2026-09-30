#!/usr/bin/env bash
#
# End-to-end smoke test: assemble a bare-metal RV32IM program with the real
# RISC-V cross toolchain, run the resulting binary image on the model, and check
# the final register state.
#
# This is the closest thing to ROADMAP phase P0's acceptance check that can run
# without Spike or QEMU: the model consumes compiler-produced encodings instead
# of hand-written ones. See docs/ROADMAP.md phase P0.
#
# Usage: scripts/riscv-smoke.sh [--require]
#
#   --require   fail instead of skipping when the cross toolchain is missing.
#               CI passes this so a missing toolchain cannot pass silently.
#
# Environment:
#   RISCV_GCC, RISCV_OBJCOPY   override the tool names (default below).
#   CARGO                      override the cargo binary.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

require=0
case "${1:-}" in
  --require) require=1 ;;
  "") ;;
  *) echo "usage: scripts/riscv-smoke.sh [--require]" >&2; exit 2 ;;
esac

# The Ubuntu package `gcc-riscv64-unknown-elf` provides both of these; the
# toolchain is named riscv64-* even when targeting rv32.
gcc=${RISCV_GCC:-riscv64-unknown-elf-gcc}
objcopy=${RISCV_OBJCOPY:-riscv64-unknown-elf-objcopy}
cargo=${CARGO:-cargo}

if ! command -v "$gcc" >/dev/null 2>&1 || ! command -v "$objcopy" >/dev/null 2>&1; then
  message="riscv-smoke: '$gcc'/'$objcopy' not found. Install gcc-riscv64-unknown-elf"
  message="$message (Ubuntu: apt-get install gcc-riscv64-unknown-elf) or set"
  message="$message RISCV_GCC/RISCV_OBJCOPY."
  if [ "$require" -eq 1 ]; then
    echo "$message" >&2
    exit 1
  fi
  echo "SKIP: $message" >&2
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

"$gcc" -march=rv32im -mabi=ilp32 -nostdlib -nostartfiles \
       -Wl,-Ttext=0x0 -o "$work/smoke.elf" "$root/scripts/smoke.S"
"$objcopy" -O binary --only-section=.text "$work/smoke.elf" "$work/smoke.bin"

if [ ! -s "$work/smoke.bin" ]; then
  echo "riscv-smoke: the toolchain produced an empty image" >&2
  exit 1
fi

out=$(cd "$root" && "$cargo" run --quiet --bin chips -- "$work/smoke.bin" 0x0)

status=0

report() {
  if [ "$status" -ne 0 ]; then
    echo "--- model output ---" >&2
    echo "$out" >&2
    echo "riscv-smoke: FAILED" >&2
    exit 1
  fi
}

if ! grep -q '^stopped: Ebreak$' <<<"$out"; then
  echo "FAIL: the program did not halt on ebreak" >&2
  status=1
fi

# expect <register index> <expected hex, as the driver prints it>
expect() {
  got=$(awk -v reg="x$(printf '%02d' "$1")" '$1 == reg { print $3 }' <<<"$out")
  if [ "$got" != "$2" ]; then
    echo "FAIL: x$1 = ${got:-<missing>}, expected $2 ($3)" >&2
    status=1
  fi
}

expect 10 00000006 "a0 = 6"
expect 11 00000007 "a1 = 7"
expect 12 0000002a "a2 = mul(6, 7)"
expect 13 ffffffec "a3 = -20"
expect 14 00000003 "a4 = 3"
expect 15 fffffffa "a5 = div(-20, 3)"
expect 16 fffffffe "a6 = rem(-20, 3)"
expect 17 ffffffff "a7 = divu(0, 0)"

report
echo "riscv-smoke: OK ($("$gcc" --version | head -n 1))"
