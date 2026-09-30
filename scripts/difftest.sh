#!/usr/bin/env bash
#
# Differential test: run a program on the model and on Spike, and compare them
# instruction by instruction.
#
# This is the ROADMAP P3 comparison, run for the first time against something
# other than the model. The model is the golden reference, so the question is not
# "which is right" but "do they agree, and if not, where" — a divergence has to
# be attributable to one instruction, not found by reading two state dumps.
#
# # What is compared
#
# For every step: the PC, the instruction word, the destination register and the
# value written to it, and any memory write with its address and value. That is
# the intersection of what the two tools report, and it is enough to catch a
# wrong decode, a wrong operand, a wrong sign extension, or a wrong store.
#
# Only *retired* instructions are compared. Spike's commit log records nothing
# else, so a trapping step has no counterpart on that side; a trap is still
# visible, because the handler's first instruction appears as the next retired step
# at the handler address.
#
# Two things are deliberately *not* compared, because they would differ for
# reasons that say nothing about correctness:
#
#   * Step and cycle numbers. Spike's log opens at cycle 3 because of its own
#     HTIF setup, and the model's counter has no meaning off this machine.
#   * The number of steps. Neither simulator has a reason to stop at the same
#     point, so the comparison is over the shorter of the two and Spike's log is
#     truncated to match. Agreement means the two agree on every step the model
#     took, which is the only claim that means anything here.
#
# # Termination
#
# Spike's built-in HTIF is not the riscv-tests `tohost` protocol, and the proxy
# kernel that used to translate between them is no longer part of riscv-isa-sim.
# A program therefore cannot end through a channel both simulators understand.
#
# So the harness does not ask them to stop. A program that exports a `pass_spin`
# symbol is run with a fixed budget and its verdict is read from the address it
# settles at; anything else is treated as a riscv-tests image and run through the
# model's --htif mode, where the model's own verdict is authoritative and Spike is
# only asked to agree step for step.
#
# # What this does not compare
#
# CSR writes and the final architectural state. Spike's commit log does not record
# CSR writes, so they are not in the intersection. Comparing final state as well
# would catch more, and is the obvious next step; see docs/TODO.md.
#
# Usage: scripts/difftest.sh [options] <program.elf>...
#
#   --require        fail instead of skipping when Spike is missing (CI uses
#                    this, so a missing reference cannot pass silently)
#   --budget N       per-test instruction budget for the model (default 2000)
#   --memory SPEC    Spike memory region, base:size (default 0x80000000:0x1000000)
#   --isa STRING     Spike ISA string (default rv32im_zicsr)
#   --priv STRING    Spike privilege modes (default msu)
#   --pc ADDRESS     start address (default 0x80000000)
#   -v, --verbose    show the first divergence in full
#
# Environment:
#   SPIKE            path to the spike binary
#   RISCV_GCC        cross compiler, used to read the tohost symbol
#   RISCV_NM         symbol reader
#   CARGO            cargo binary
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

require=0
budget=2000
memory=0x80000000:0x1000000
isa=rv32im_zicsr
priv=msu
pc=0x80000000
verbose=0
programs=()

while [ $# -gt 0 ]; do
  case "$1" in
    --require) require=1 ;;
    --budget) budget=$2; shift ;;
    --memory) memory=$2; shift ;;
    --isa) isa=$2; shift ;;
    --priv) priv=$2; shift ;;
    --pc) pc=$2; shift ;;
    -v|--verbose) verbose=1 ;;
    -h|--help) sed -n '2,50p' "$0"; exit 0 ;;
    -*) echo "difftest: unknown option $1" >&2; exit 2 ;;
    *) programs+=("$1") ;;
  esac
  shift
done

spike=${SPIKE:-spike}
nm=${RISCV_NM:-riscv64-unknown-elf-nm}
objcopy=${RISCV_OBJCOPY:-riscv64-unknown-elf-objcopy}
cargo=${CARGO:-cargo}

if ! command -v "$spike" >/dev/null 2>&1; then
  message="difftest: '$spike' not found."
  message="$message Spike is the external reference this compares against; install"
  message="$message riscv-isa-sim (not packaged by Ubuntu) or set SPIKE=/path/to/spike."
  if [ "$require" -eq 1 ]; then
    echo "$message" >&2
    exit 1
  fi
  echo "SKIP: $message" >&2
  exit 0
fi
for tool in "$nm" "$objcopy"; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "difftest: '$tool' not found; install gcc-riscv64-unknown-elf" >&2
    exit 1
  fi
done

(cd "$root" && "$cargo" build --quiet --bin chips)
model="$root/target/debug/chips"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Normalize the model's trace to `pc=<pc> inst=<inst> [x<n>=<val>] [m<addr>=<val>]`.
#
# Two kinds of model record are dropped, both for the same reason: Spike's commit
# log records only instructions that *retired*, so a step that did not retire has
# no counterpart on that side.
#
#   * `t` and `x` records. A trap is still visible in the comparison, because the
#     handler's first instruction appears as the next retired step at the handler
#     address — which is exactly what makes a trap entry comparable, and is how a
#     differential run would notice a handler entered at the wrong address.
#   * CSR fields (`c<addr>=`). Spike's commit log does not record CSR writes at
#     all, so keeping them would guarantee a mismatch on the first `csrrw` in any
#     program that touches a CSR, and every program touches mtvec.
model_norm() {
  awk '
    /^v[0-9]+$/ { next }
    !/^i / { next }
    {
      pc = ""; inst = ""; regs = ""; mems = ""
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^pc=/) pc = $i
        else if ($i ~ /^inst=/) inst = $i
        else if ($i ~ /^x[0-9]+=/) regs = regs " " $i
        else if ($i ~ /^m[0-9a-f]+=/) mems = mems " " $i
      }
      if (pc != "") print pc, inst, regs, mems
    }
  ' "$1" | sed -E "s/ +/ /g; s/ +$//; s/^ //"
}

# Normalize Spike's commit log to the same shape.
#
# A Spike line is: core 0: <cycle> 0x<pc> (0x<inst>) [x<n> <val>] [x<n> <val> ...]
# [mem 0x<addr> 0x<val>]. The *first* register field is the destination; the rest
# are source operands, which the model does not report as writes. A store has no
# register field at all, only `mem`.
#
# The register token is `x<n>` with the number attached (`x1`, not `x 1`) and the
# spacing after it varies, so fields are matched by shape rather than by
# position.
spike_norm() {
  awk '
    /^core +[0-9]+:/ {
      pc = $4; inst = $5; gsub(/[()]/, "", inst)
      dest = ""; mem = ""
      if ($6 ~ /^x[0-9]+$/) dest = "x" substr($6, 2) "=" substr($7, 3)
      else if ($6 == "mem") mem = "m" substr($7, 3) "=" substr($8, 3)
      printf "%s %s %s %s\n", "pc=" substr(pc, 3), "inst=" substr(inst, 3), dest, mem
    }
  ' "$1" | sed -E "s/ +/ /g; s/ +$//; s/^ //"
}

# CSRs that are optional in the architecture and that the model and Spike do not
# both implement. `riscv-tests` feature-detects by *writing* them and relying on a
# trap to skip the rest, so when the two implementations disagree about which of
# these exist they walk through the same source by different routes and their
# instruction streams part company before any real arithmetic happens.
#
# A divergence at one of these is therefore not evidence of a semantic
# difference, and is reported as a gap. A divergence anywhere else is a real
# finding. The list is of CSR *numbers*; the address is in inst[31:20] for both a
# Zicsr instruction and a funct12-encoded SYSTEM instruction.
optional_csrs=(
  0x302 # medeleg: the model has no trap delegation
  0x303 # mideleg
  0x320 # mcountinhibit
  0x3a0 # pmpcfg0: the model has no PMP
  0x3b0 # pmpaddr0 .. 0x3bf
  0x3c0 # pmpaddr8 .. 0x3cf
  0x744 # mnstatus: no NMI source
  0x7a0 # tselect: no debug trigger module
  0x7a1 # tdata1
  0x7a2 # tdata2
  0x7a5 # tcontrol
)

# Is the instruction a CSR access naming one of the CSRs above?
#
# A Zicsr instruction is opcode 0x73 with funct3 != 0, and a funct12-encoded
# SYSTEM instruction is opcode 0x73 with funct3 == 0; both carry the CSR number in
# inst[31:20].
is_optional_csr_instruction() {
  local inst=$1 number
  # An empty or non-hex field means the step was not found; that is a real
  # difference, not a gap, so decline to classify it.
  case "$inst" in
    ''|*[!0-9a-f]*) return 1 ;;
  esac
  # `test -eq` does not accept hex literals, so every comparison is done as
  # arithmetic expansion instead.
  [ $((inst & 0x7f)) -eq $((0x73)) ] || return 1
  number=$((inst >> 20))
  local csr
  for csr in "${optional_csrs[@]}"; do
    [ "$number" -eq "$((csr))" ] && return 0
  done
  return 1
}

# The instruction on the given side of a normalized trace, for one step number.
instruction_at() {
  sed -n "$(( $2 ))p" "$1" | sed -E 's/.*inst=([0-9a-f]+).*/\1/'
}

# The PC on one line of a trace, in the same bare-hex form as `instruction_at`.
pc_at() {
  sed -n "$(( $2 ))p" "$1" | sed -E 's/^[^ ]* [0-9]+ pc=([0-9a-f]+).*/\1/'
}

# With no programs named, build and run the self-contained program instead. That
# is the default because it is the only input whose trace comparison is
# meaningful: see the comment at the top of scripts/difftest.S.
if [ ${#programs[@]} -eq 0 ]; then
  build_dir=$(mktemp -d)
  if ! riscv64-unknown-elf-gcc -march=rv32im_zicsr -mabi=ilp32 \
        -nostdlib -nostartfiles -T "$root/scripts/difftest.ld" \
        -o "$build_dir/difftest.elf" "$root/scripts/difftest.S" 2>"$build_dir/cc.log"; then
    echo "difftest: could not assemble scripts/difftest.S:" >&2
    sed 's/^/  /' "$build_dir/cc.log" >&2
    rm -rf "$build_dir"
    exit 1
  fi
  programs=("$build_dir/difftest.elf")
  # The build tree is temporary and owned here, so the cleanup trap may remove it.
  cleanup_build=$build_dir
  trap 'rm -rf "$work" ${cleanup_build:-}' EXIT
fi

pass=0
gap=0
fail=0
failed=()
gapped=()

for elf in "${programs[@]}"; do
  name=$(basename "$elf")

  symbol() { "$nm" "$elf" | awk -v s="$1" '$3 == s { print $1; exit }'; }

  pass_spin=$(symbol pass_spin)
  tohost=$(symbol tohost)

  "$objcopy" -O binary "$elf" "$work/$name.bin"
  # Spike writes its commit log to *stderr*, not stdout. Redirecting the wrong
  # stream yields an empty log and a diff that looks like total disagreement, so
  # the log is taken from stderr and stdout is discarded.
  # A bounded run: a program that writes a value of zero to tohost has not
  # signalled anything, and Spike will spin on it forever. The timeout is what
  # turns that into a reported disagreement instead of a hung job.
  if ! timeout "${spike_timeout:-60}" "$spike" --isa="$isa" --priv="$priv" \
    -m"$memory" --pc="$pc" --log-commits "$elf" \
    >/dev/null 2>"$work/$name.spike"; then
    status=$?
    if [ "$status" -eq 124 ]; then
      echo "  DIFFER $name — Spike did not finish within ${spike_timeout:-60}s."
      echo "      Usually the program signalled nothing: tohost = 0 is not a command."
      fail=$((fail + 1))
      failed+=("$name")
      continue
    fi
  fi
  if [ ! -s "$work/$name.spike" ]; then
    echo "  FAIL $name — Spike produced no commit log (stderr above, if any)"
    fail=$((fail + 1))
    failed+=("$name")
    continue
  fi

  if [ -n "$pass_spin" ]; then
    # A self-contained program: no verdict channel, so run to the budget and read
    # the result off the final PC. A budget that the program never reaches is not
    # an error — it is the spin loop at the end.
    "$model" "$work/$name.bin" "$pc" "$budget" --trace \
      >"$work/$name.model" 2>/dev/null || true
    verdict=$(pc_at "$work/$name.model" "$budget")
    # Both sides in the same bare-hex form, or the comparison is between a hex
    # string and a decimal number and never matches.
    expected=$(printf '%08x' "$((16#$pass_spin))")
    if [ "$verdict" != "$expected" ]; then
      fail=$((fail + 1))
      failed+=("$name")
      echo "  FAIL   $name — the program did not reach pass_spin"
      echo "        final pc 0x$verdict, expected 0x$expected"
      continue
    fi
    # How much of the comparison was the program rather than the spin loop it ends
    # in. Reporting the total would be honest and useless: nearly all of it is one
    # repeated jump, and a hundred spin steps would swamp a real count of thirty.
    # The `pc=` field is not the first one, so match the field with its trailing
    # space rather than the line's first word.
    reached=$(awk -v p=" pc=$expected " 'index($0, p) { print NR; exit }' \
      "$work/$name.model")
    real_steps=${reached:-$budget}
  elif [ -n "$tohost" ]; then
    tohost=$((16#$tohost))
    if ! "$model" "$work/$name.bin" "$pc" "$budget" "--htif=$tohost" --trace \
      >"$work/$name.model" 2>"$work/$name.model.err"; then
      echo "  FAIL $name — the model itself reported a failure:"
      sed 's/^/        /' "$work/$name.model.err" | head -5
      fail=$((fail + 1))
      failed+=("$name")
      continue
    fi
  else
    echo "  SKIP $name (neither pass_spin nor tohost: not a program this can judge)"
    continue
  fi

  model_norm "$work/$name.model" >"$work/$name.model.norm"
  spike_norm "$work/$name.spike" >"$work/$name.spike.norm"

  # The model's trace should be a prefix of Spike's: Spike keeps spinning after
  # the test writes tohost, and the model stops there.
  steps=$(wc -l <"$work/$name.model.norm")
  if [ "$steps" -eq 0 ]; then
    echo "  FAIL $name — the model produced no trace"
    fail=$((fail + 1))
    failed+=("$name")
    continue
  fi
  head -n "$steps" "$work/$name.spike.norm" >"$work/$name.spike.head"

  if diff -u "$work/$name.model.norm" "$work/$name.spike.head" >"$work/$name.diff"; then
    pass=$((pass + 1))
    # `real_steps` is unset for a riscv-tests image, which reports through tohost
    # rather than a spin address; fall back to the total there.
    printf '  AGREE %-28s %s program steps, %s compared\n' \
      "$name" "${real_steps:-$steps}" "$steps"
    continue
  fi

  # Locate the first differing step and ask whether it is one of the optional
  # CSRs. If so the two implementations simply have different feature sets and
  # the rest of the comparison is meaningless, because everything after it is
  # shifted by however many steps the skipped probes cost.
  # Find the first line where the two traces actually differ. Parsing the diff's
  # hunk header would give the start of the *context*, which is often a line the
  # two agree on.
  hunk=$(awk 'NR==FNR { a[FNR] = $0; next }
               { if (a[FNR] != $0) { print FNR; exit } }' \
    "$work/$name.model.norm" "$work/$name.spike.head")
  if [ -z "$hunk" ]; then
    # The only difference is length: Spike kept spinning after the model stopped.
    pass=$((pass + 1))
    printf '  AGREE %-28s %s steps (spike continued)\n' "$name" "$steps"
    continue
  fi
  model_inst=$(instruction_at "$work/$name.model.norm" "$hunk")
  spike_inst=$(instruction_at "$work/$name.spike.head" "$hunk")

  model_num=0
  spike_num=0
  case "$model_inst" in ''|*[!0-9a-f]*) ;; *) model_num=$((16#$model_inst)) ;; esac
  case "$spike_inst" in ''|*[!0-9a-f]*) ;; *) spike_num=$((16#$spike_inst)) ;; esac
  if is_optional_csr_instruction "$model_num" ||
    is_optional_csr_instruction "$spike_num"; then
    gap=$((gap + 1))
    gapped+=("$name")
    printf '  GAP    %-28s optional CSR at step %s: model %s / spike %s\n' \
      "$name" "$hunk" "0x$model_inst" "0x$spike_inst"
    continue
  fi

  fail=$((fail + 1))
  failed+=("$name")
  echo "  DIFFER $name"
  if [ "$verbose" -eq 1 ]; then
    # The first differing hunk, with the raw lines from both sides, is what
    # makes a divergence attributable to an instruction.
    sed -n '1,25p' "$work/$name.diff" | sed 's/^/        /'
  else
    echo "      first difference at step $hunk: model 0x$model_inst / spike 0x$spike_inst"
    echo "      re-run with -v for context"
  fi
done

echo "difftest: $pass agreed, $gap gaps, $fail differed, out of ${#programs[@]}"

if [ "${#gapped[@]}" -ne 0 ]; then
  echo "difftest: gaps (optional CSR feature detection): ${gapped[*]}"
fi

if [ "$fail" -ne 0 ]; then
  echo "difftest: differed: ${failed[*]}" >&2
  exit 1
fi
