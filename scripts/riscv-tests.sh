#!/usr/bin/env bash
#
# Run the official riscv-tests conformance suites on the model.
#
# The suites do not halt. On success or failure a test writes a completion code
# to the `tohost` register and then spins forever, so this drives the model with
# `--htif` and reads the result out of that register. The convention is in
# env/p/riscv_test.h: `RVTEST_PASS` writes 1, and `RVTEST_FAIL` writes
# `(check << 1) | 1`, so any odd value above one names the check that failed.
#
# Two things make this more than a loop over binaries:
#
#   * `tohost` is not at a fixed address. env/p/link.ld places it at the first
#     4 KiB boundary after `.text.init`, so a test whose `.text.init` exceeds a
#     page gets it at 0x80002000 or higher. It is read from the symbol table
#     per test and passed to the model with `--htif=`.
#
#   * The image must include `.data` and `.bss`, not just `.text.init`. The
#     load/store tests read a data buffer, and a text-only image would leave it
#     zeroed and fail. `objcopy -O binary` without `--only-section` produces one
#     contiguous image with the gaps filled.
#
# Usage: scripts/riscv-tests.sh [options] [suite...]
#
#   --require       fail instead of skipping when a tool is missing (CI uses
#                   this, so a missing toolchain cannot pass silently)
#   --jobs N        parallelism for the build (default: nproc)
#   --budget N      per-test instruction budget (default: 2000000)
#   --keep          leave the clone and build tree in place for inspection
#   suite...        suites to run, e.g. rv32ui rv32mi (default: both)
#
# Environment:
#   RISCV_TESTS_DIR  reuse an existing clone/build tree
#   RISCV_GCC        override the compiler (default riscv64-unknown-elf-gcc)
#   RISCV_NM         override the symbol reader (default riscv64-unknown-elf-nm)
#   RISCV_OBJCOPY    override the binary extractor
#   CARGO            override the cargo binary
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

require=0
jobs=$(nproc 2>/dev/null || echo 4)
budget=2000000
keep=0
suites=()

while [ $# -gt 0 ]; do
  case "$1" in
    --require) require=1 ;;
    --jobs) jobs=$2; shift ;;
    --budget) budget=$2; shift ;;
    --keep) keep=1 ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    -*) echo "riscv-tests: unknown option $1" >&2; exit 2 ;;
    *) suites+=("$1") ;;
  esac
  shift
done
[ ${#suites[@]} -eq 0 ] && suites=(rv32ui rv32mi)

gcc=${RISCV_GCC:-riscv64-unknown-elf-gcc}
nm=${RISCV_NM:-riscv64-unknown-elf-nm}
objcopy=${RISCV_OBJCOPY:-riscv64-unknown-elf-objcopy}
cargo=${CARGO:-cargo}

missing=0
for tool in "$gcc" "$nm" "$objcopy"; do
  command -v "$tool" >/dev/null 2>&1 || missing=1
done
if [ "$missing" -eq 1 ]; then
  message="riscv-tests: '$gcc'/'$nm'/'$objcopy' not found."
  message="$message Install gcc-riscv64-unknown-elf (Ubuntu: apt-get install"
  message="$message gcc-riscv64-unknown-elf) or set RISCV_GCC/RISCV_NM/RISCV_OBJCOPY."
  if [ "$require" -eq 1 ]; then
    echo "$message" >&2
    exit 1
  fi
  echo "SKIP: $message" >&2
  exit 0
fi

if [ -n "${RISCV_TESTS_DIR:-}" ]; then
  work=$RISCV_TESTS_DIR
  # The caller owns this tree. Never remove it, whatever the outcome.
  cleanup() { :; }
else
  work=$(mktemp -d)
  cleanup() { [ "$keep" -eq 0 ] && rm -rf "$work"; }
fi

if [ ! -d "$work/.git" ]; then
  echo "riscv-tests: cloning into $work"
  git clone --depth 1 --recurse-submodules --shallow-submodules \
    https://github.com/riscv/riscv-tests.git "$work"
fi

cd "$work"
trap cleanup EXIT

# XLEN=32 builds the rv32* suites. The virtual-memory (-v-) targets need newlib
# headers the bare cross toolchain does not ship, and they are not what this
# model implements anyway, so the build is limited to the physical (-p-) ones.
if [ ! -f Makefile ] || ! grep -q "XLEN = 32" Makefile; then
  echo "riscv-tests: configuring for XLEN=32"
  ./configure --with-xlen=32 >/dev/null
fi

targets=()
for suite in "${suites[@]}"; do
  # The -p- binary for isa/<suite>/<name>.S is isa/<suite>-p-<name>, so the
  # source list is the authoritative set of targets. Reusing a build tree means
  # they already exist and `make` has nothing to do.
  names=()
  for src in isa/"$suite"/*.S; do
    [ -e "$src" ] || continue
    names+=("$suite-p-$(basename "$src" .S)")
  done
  if [ ${#names[@]} -eq 0 ]; then
    echo "riscv-tests: no sources under isa/$suite" >&2
    exit 1
  fi
  if [ ! -e "isa/${names[0]}" ]; then
    echo "riscv-tests: building ${#names[@]} $suite-p-* binaries"
    # Only the physical (-p-) targets: the -v- ones need newlib headers the bare
    # cross toolchain does not ship, and they need virtual memory, which this
    # model does not implement.
    #
    # XLEN=32 must be passed explicitly. isa/Makefile defaults it to 64 and only
    # includes the rv32* Makefrags when it is 32, so without this the sub-make
    # reports "No rule to make target" for every rv32 target.
    make -C isa -j"$jobs" XLEN=32 "${names[@]}" >/dev/null
  fi
  for name in "${names[@]}"; do
    [ -e "isa/$name" ] && targets+=("isa/$name")
  done
done

if [ ${#targets[@]} -eq 0 ]; then
  echo "FAIL: no test binaries were produced for: ${suites[*]}" >&2
  exit 1
fi

echo "riscv-tests: running ${#targets[@]} binaries on the model"

# Tests for facilities this model does not implement. They are reported
# separately rather than counted as passes, so the headline number means what it
# says and a regression in a supported test is never lost in the noise.
#
#   rv32mi-p-breakpoint  the debug trigger module (tcontrol/tselect/tdata1-3).
#                       The test's own handler jumps straight to `fail` when a
#                       `tselect` write traps, so an implementation without the
#                       module cannot pass it. Debug spec, not base ISA.
#   rv32mi-p-pmpaddr     physical memory protection (pmpaddr0/pmpcfg0). The
#                       model has no PMP at all.
#   rv32mi-p-illegal     the model has S and U privilege levels but no trap
#                       *delegation*. This test probes for S-mode by writing
#                       MPP = S and reading it back, and skips itself when the
#                       answer is no; since the model does support S-mode it
#                       proceeds into mideleg, vectored supervisor interrupts,
#                       and the TVM/TSR/SUM/MXR gating, none of which exist yet.
#                       See docs/TODO.md.
excluded_patterns=(
  "rv32mi-p-breakpoint"
  "rv32mi-p-pmpaddr"
  "rv32mi-p-illegal"
)
is_excluded() {
  local name=$1 pattern
  for pattern in "${excluded_patterns[@]}"; do
    [ "$name" = "$pattern" ] && return 0
  done
  return 1
}

image=$(mktemp)
(cd "$root" && "$cargo" build --quiet --bin chips)
bin="$root/target/debug/chips"

pass=0
fail=0
excluded=0
failed_names=()
excluded_names=()
skipped=0

for elf in "${targets[@]}"; do
  name=$(basename "$elf")
  [ -x "$elf" ] || { skipped=$((skipped + 1)); continue; }

  if is_excluded "$name"; then
    excluded=$((excluded + 1))
    excluded_names+=("$name")
    continue
  fi

  "$objcopy" -O binary "$elf" "$image" 2>/dev/null || {
    echo "  SKIP $name (objcopy failed)"
    skipped=$((skipped + 1))
    continue
  }

  tohost=$("$nm" "$elf" | awk '$3 == "tohost" { print $1; exit }')
  if [ -z "$tohost" ]; then
    echo "  SKIP $name (no tohost symbol)"
    skipped=$((skipped + 1))
    continue
  fi
  # `nm` prints bare hex; the model reads an unprefixed number as decimal.
  tohost=$((16#$tohost))

  if out=$("$bin" "$image" 0x80000000 "$budget" "--htif=$tohost" 2>&1); then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failed_names+=("$name")
    printf '  FAIL %-28s %s\n' "$name" "$(head -1 <<<"$out")"
  fi
done
rm -f "$image"

echo "riscv-tests: $pass passed, $fail failed, $excluded excluded, $skipped skipped"
if [ "$excluded" -ne 0 ]; then
  echo "riscv-tests: excluded (unimplemented): ${excluded_names[*]}"
fi

if [ "$fail" -ne 0 ]; then
  echo "riscv-tests: failed: ${failed_names[*]}" >&2
  exit 1
fi
