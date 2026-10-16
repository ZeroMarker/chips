#!/usr/bin/env python3
"""Bounded RV32 differential runner with independent per-step state reads."""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent

# Shared implemented fields, not every field supported by either simulator.
# Counter/time/pending bits and identity/capability CSRs are deliberately absent.
CSRS = {
    0x100: ("sstatus", 0x122), 0x104: ("sie", 0x222),
    0x105: ("stvec", 0xffffffff), 0x140: ("sscratch", 0xffffffff),
    0x141: ("sepc", 0xffffffff), 0x142: ("scause", 0xffffffff),
    0x143: ("stval", 0xffffffff), 0x180: ("satp", 0xffffffff),
    0x300: ("mstatus", 0x4219aa), 0x302: ("medeleg", 0x3ff),
    0x303: ("mideleg", 0x222), 0x304: ("mie", 0xaaa),
    0x305: ("mtvec", 0xffffffff), 0x340: ("mscratch", 0xffffffff),
    0x341: ("mepc", 0xffffffff), 0x342: ("mcause", 0xffffffff),
    0x343: ("mtval", 0xffffffff),
    **{0x3a0 + i: (f"pmpcfg{i}", 0x9f9f9f9f) for i in range(4)},
    **{0x3b0 + i: (f"pmpaddr{i}", 0x3fffffff) for i in range(16)},
    0x7a0: ("tselect", 0xffffffff),
    # Type/capability, hit, maskmax, context and advanced trigger fields differ.
    0x7a1: ("tdata1", 0x5f), 0x7a2: ("tdata2", 0xffffffff),
}
FIELDS = ["pc", "priv"] + [f"x{i}" for i in range(32)] + [f"c{a:03x}" for a in CSRS]
MASKS = {key: 0xffffffff for key in FIELDS}
MASKS.update({f"c{a:03x}": mask for a, (_, mask) in CSRS.items()})
LABELS = {f"c{a:03x}": name for a, (name, _) in CSRS.items()}


class Failure(Exception):
    """A failed check, unsupported protocol, or incomplete execution."""


@dataclass(eq=True)
class Event:
    pc: int
    instruction: int
    registers: dict[int, int]
    stores: list[tuple[int, int]]
    csrs: dict[int, int]


def number(value: str) -> int:
    return int(value, 16 if value.lower().startswith("0x") else 10)


def run(command: list[str], *, timeout: float, cwd: Path = ROOT) -> str:
    try:
        result = subprocess.run(command, cwd=cwd, capture_output=True, text=True,
                                timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Failure(f"{command[0]}: {error}") from error
    if result.returncode:
        detail = (result.stderr or result.stdout)[-1500:]
        raise Failure(f"{command[0]} exited {result.returncode}: {detail.strip()}")
    return result.stdout


def parse_model_states(text: str) -> list[dict[str, int]]:
    states = []
    for line in text.splitlines():
        tokens = line.split()
        if not tokens or tokens.pop(0) != "s1":
            raise Failure("invalid model state version/record")
        state = {}
        for token in tokens:
            match = re.fullmatch(r"(pc|priv|x\d+|c[0-9a-f]{3})=([0-9a-f]+)", token)
            if not match or match[1] in state:
                raise Failure(f"invalid/duplicate model state field: {token}")
            state[match[1]] = int(match[2], 10 if match[1] == "priv" else 16)
        if set(FIELDS) - state.keys():
            raise Failure("model snapshot is missing required fields")
        if state["priv"] not in (0, 1, 3) or any(v > 0xffffffff for v in state.values()):
            raise Failure("model state is not RV32 M/S/U")
        states.append(state)
    if not states:
        raise Failure("empty model state log")
    return states


def parse_model_trace(text: str) -> tuple[list[Event], int]:
    events, steps = [], 0
    lines = text.splitlines()
    if not lines or lines.pop(0) != "v1":
        raise Failure("invalid model trace version")
    for line in lines:
        parts = line.split()
        if len(parts) < 4 or parts[0] not in ("i", "t", "x"):
            raise Failure(f"invalid model trace record: {line}")
        steps += 1
        if parts[1] != str(steps):
            raise Failure("model trace step sequence is incomplete")
        fields = dict(token.split("=", 1) for token in parts[2:])
        if parts[0] != "i":
            continue
        events.append(Event(int(fields["pc"], 16), int(fields["inst"], 16),
            {int(k[1:]): int(v, 16) for k, v in fields.items() if re.fullmatch(r"x\d+", k)},
            [(int(k[1:], 16), int(v, 16)) for k, v in fields.items() if re.fullmatch(r"m[0-9a-f]+", k)],
            {int(k[1:], 16): int(v, 16) for k, v in fields.items() if re.fullmatch(r"c[0-9a-f]+", k)}))
    return events, steps


def parse_spike_event(line: str) -> Event:
    match = re.fullmatch(r"core\s+0:\s+[013]\s+0x([0-9a-f]+)\s+\(0x([0-9a-f]+)\)(.*)", line)
    if not match:
        raise Failure(f"unsupported Spike commit record: {line}")
    tokens = match[3].split()
    registers, stores, csrs = {}, [], {}
    index = 0
    while index < len(tokens):
        key = tokens[index]
        if re.fullmatch(r"x\d+", key):
            registers[int(key[1:])] = int(tokens[index + 1], 16)
            index += 2
        elif re.fullmatch(r"c\d+_\w+", key):
            csrs[int(key[1:].split("_")[0])] = int(tokens[index + 1], 16)
            index += 2
        elif key == "mem":
            address = int(tokens[index + 1], 16)
            index += 2
            if index < len(tokens) and tokens[index].startswith("0x"):
                stores.append((address, int(tokens[index], 16)))
                index += 1
        else:
            raise Failure(f"unsupported Spike commit field: {key}")
    return Event(int(match[1], 16), int(match[2], 16), registers, stores, csrs)


def debug_commands(steps: int) -> str:
    query = ["pc 0", "priv 0"] + [f"reg 0 {i}" for i in range(32)]
    query += [f"reg 0 {name}" for name, _ in CSRS.values()]
    lines = list(query)
    for _ in range(steps):
        lines += ["rs 1", *query]
    return "\n".join([*lines, "quit", ""])


def parse_spike(text: str, count: int) -> tuple[list[dict[str, int]], list[Event]]:
    values, events = [], []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("core"):
            events.append(parse_spike_event(line))
        elif re.fullmatch(r"0x[0-9a-f]+", line):
            values.append(int(line, 16))
        elif line in ("M", "S", "U"):
            values.append({"M": 3, "S": 1, "U": 0}[line])
    expected = count * len(FIELDS)
    if len(values) != expected:
        raise Failure(f"incomplete Spike state log: {len(values)} values, expected {expected}")
    if any(value > 0xffffffff for value in values):
        raise Failure("Spike state is not RV32")
    states = [dict(zip(FIELDS, values[start:start + len(FIELDS)]))
              for start in range(0, len(values), len(FIELDS))]
    if any(state["priv"] not in (0, 1, 3) for state in states):
        raise Failure("unsupported Spike privilege state")
    return states, events


def compare_states(model: list[dict[str, int]], reference: list[dict[str, int]],
                   start_step: int = 0) -> None:
    if len(model) != len(reference):
        raise Failure(f"state count differs: model {len(model)}, Spike {len(reference)}")
    for step, (left, right) in enumerate(zip(model, reference), start_step):
        for field in FIELDS:
            mask = MASKS[field]
            if left[field] & mask != right[field] & mask:
                label = LABELS.get(field, field)
                raise Failure(f"state after step {step}, {label}: model 0x{left[field]:08x}, "
                              f"Spike 0x{right[field]:08x}, mask 0x{mask:08x}")


def compare_events(model: list[Event], reference: list[Event]) -> None:
    if len(model) != len(reference):
        raise Failure(f"retired trace length differs: model {len(model)}, Spike {len(reference)}")
    for index, (left, right) in enumerate(zip(model, reference), 1):
        # CSR writes can be omitted by Spike when ignored/unchanged, logged via
        # an alias, or absent during trap entry. Compare their actual effects in
        # every independent CSR snapshot, not exact textual event presence.
        if (left.pc, left.instruction, left.registers, left.stores) != (
                right.pc, right.instruction, right.registers, right.stores):
            raise Failure(f"retired step {index}: model {left}, Spike {right}")


def symbols(elf: Path, nm: str, timeout: float) -> dict[str, int]:
    result = {}
    for line in run([nm, str(elf)], timeout=timeout).splitlines():
        fields = line.split()
        if len(fields) >= 3 and re.fullmatch(r"[0-9a-fA-F]+", fields[0]):
            result[fields[2]] = int(fields[0], 16)
    return result


def check_program(elf: Path, args: argparse.Namespace, work: Path, model: Path) -> str:
    names = symbols(elf, args.nm, args.timeout)
    checkpoint = args.checkpoint or ("pass_spin" if "pass_spin" in names else None)
    model_args = []
    if checkpoint:
        try:
            address = names[checkpoint] if checkpoint in names else number(checkpoint)
        except ValueError as error:
            raise Failure(f"unknown checkpoint {checkpoint}") from error
        model_args.append(f"--stop-pc={address}")
    elif "tohost" in names:
        model_args.append(f"--htif={names['tohost']}")
    else:
        raise Failure("ELF needs pass_spin, --checkpoint, or tohost")
    image, state_log, commands = work / "image.bin", work / "model.state", work / "spike.cmd"
    run([args.objcopy, "-O", "binary", str(elf), str(image)], timeout=args.timeout)
    trace = run([str(model), str(image), str(args.pc), str(args.budget), "--trace",
                 f"--state-log={state_log}", *model_args], timeout=args.timeout)
    states = parse_model_states(state_log.read_text())
    model_events, steps = parse_model_trace(trace)
    if len(states) != steps + 1:
        raise Failure("model state/trace step counts differ")
    commands.write_text(debug_commands(steps))
    spike_command = [args.spike, "-d", f"--debug-cmd={commands}", f"--isa={args.isa}",
        f"--priv={args.priv}", f"-m{args.memory}", f"--pc={args.pc}",
        "--log-commits", "--pmpregions=16", "--pmpgranularity=4", "--triggers=2", str(elf)]
    # Debugger state and commit records both go to stderr. There is no spinning
    # or best-effort timeout: exactly `steps` single steps, then quit normally.
    try:
        spike = subprocess.run(spike_command, capture_output=True, text=True,
                               timeout=args.timeout, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Failure(f"Spike did not complete: {error}") from error
    if spike.returncode:
        raise Failure(f"Spike exited {spike.returncode}: {spike.stderr[-1500:]}")
    (work / "spike.log").write_text(spike.stderr)
    reference, reference_events = parse_spike(spike.stderr, len(states))
    compare_events(model_events, reference_events)
    # Spike opens PMP entry zero at reset; the model resets all entries OFF.
    # Probes explicitly clear that platform setup before this named boundary.
    start = 0
    if "difftest_start" in names:
        start = next((i for i, state in enumerate(states)
                      if state["pc"] == names["difftest_start"]), -1)
        if start < 0:
            raise Failure("PMP initialization boundary was not reached")
    if args.verbose:
        print(f"    {elf.name}: state comparison starts after step {start}; "
              f"checkpoint {checkpoint or 'tohost'}; timeout {args.timeout:g}s")
    compare_states(states[start:], reference[start:], start)
    return f"{steps} steps, {len(model_events)} retired; {len(states) - start} compared states, x0-x31 + {len(CSRS)} CSR fields"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("programs", nargs="*", type=Path)
    parser.add_argument("--require", action="store_true")
    parser.add_argument("--budget", type=int, default=2000)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--checkpoint", help="ELF symbol or address (default: pass_spin)")
    parser.add_argument("--memory", default="0x80000000:0x1000000")
    parser.add_argument("--isa", default="rv32im_zicsr")
    parser.add_argument("--priv", default="msu")
    parser.add_argument("--pc", type=number, default=0x80000000)
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args(argv)
    if args.budget < 1 or args.timeout <= 0:
        parser.error("budget and timeout must be positive")
    args.spike = os.environ.get("SPIKE", "spike")
    args.nm = os.environ.get("RISCV_NM", "riscv64-unknown-elf-nm")
    args.objcopy = os.environ.get("RISCV_OBJCOPY", "riscv64-unknown-elf-objcopy")
    if not shutil.which(args.spike):
        print(f"{'FAIL' if args.require else 'SKIP'}: Spike not found: {args.spike}", file=sys.stderr)
        return 1 if args.require else 0
    try:
        run([*shlex.split(os.environ.get("CARGO", "cargo")), "build", "--quiet", "--bin", "chips"], timeout=args.timeout)
        with tempfile.TemporaryDirectory(prefix="chips-difftest-") as directory:
            work = Path(directory)
            if not args.programs:
                gcc = os.environ.get("RISCV_GCC", "riscv64-unknown-elf-gcc")
                for name in ("difftest", "pmp", "triggers", "delegation"):
                    elf = work / f"{name}.elf"
                    run([gcc, "-march=rv32im_zicsr", "-mabi=ilp32", "-nostdlib", "-nostartfiles",
                         "-T", str(ROOT / "scripts/difftest.ld"), "-o", str(elf),
                         str(ROOT / f"scripts/{name}.S")], timeout=args.timeout)
                    args.programs.append(elf)
            failures = 0
            for index, elf in enumerate(args.programs):
                program_work = work / str(index)
                program_work.mkdir()
                try:
                    summary = check_program(elf.resolve(), args, program_work, ROOT / "target/debug/chips")
                    print(f"  AGREE {elf.name}: {summary}")
                except (Failure, ValueError, KeyError, IndexError) as error:
                    failures += 1
                    print(f"  DIFFER {elf.name}: {error}")
            print(f"difftest: {len(args.programs) - failures} agreed, {failures} differed, out of {len(args.programs)}")
            return int(failures != 0)
    except Failure as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
