# Project TODO

This is the working backlog for the Rust golden model and its future RTL peer.
Items are ordered by dependency and verification value. The broader project
milestones remain in [ROADMAP.md](ROADMAP.md).

## Completed

- [x] Execute the RV32I base integer instruction set.
- [x] Execute the RV32M multiply/divide extension.
- [x] Implement all six Zicsr read/modify/write instructions.
- [x] Reject writes to architecturally read-only CSRs.
- [x] Trap misaligned instruction, load, and store addresses.
- [x] Reject reserved shift, `jalr`, fence, and system encodings.
- [x] Expose working 64-bit `cycle` and `instret` counters through their RV32
      low/high CSR pairs.
- [x] Route synchronous exceptions through machine trap entry using `mtvec`,
      `mepc`, `mcause`, and `mtval`, including `ecall`/`ebreak` when a handler is
      installed.
- [x] Implement `mret` and the `mstatus` machine-mode fields this hart needs
      (`MIE`, `MPIE`, `MPP`).
- [x] Give `time`/`timeh` a time source that advances once per step.
- [x] Implement CSR field semantics: WARL legalization for `mstatus`/`mtvec`/
      `mepc`, constant `misa`/`mhartid`, M-mode-only `mie` masks, inert `mip`,
      writable machine counters, and a trap for unimplemented CSR addresses.
- [x] Convert instruction coverage to table-driven tests covering every RV32I,
      RV32M, and Zicsr operation plus the architectural edge cases
      (`tests/isa_coverage.rs`, `tests/traps.rs`).
- [x] Improve the CLI with a configurable instruction limit, ABI register names,
      counter output, and trap diagnostics.
- [x] Add CI (`.github/workflows/ci.yml`): formatting, Clippy with warnings
      denied, rustdoc, tests on Linux/macOS/Windows in both profiles, and a
      `riscv-smoke` job that assembles `scripts/smoke.S` with the real RISC-V
      cross toolchain and runs the image on the model.

## Next

- [ ] Extend the toolchain smoke test into a real `riscv-tests` `rv32ui`/`rv32mi`
      runner (`tohost`/`ecall` pass-fail convention). CI assembles and runs one
      program today; the official suites need a runner and a target platform
      definition, and Spike is still absent as an external reference.
- [ ] Implement interrupt delivery: `mip`/`mie` currently cannot raise anything,
      and there is no CLINT/PLIC or memory-mapped `mtime`.
- [ ] Add the S/U privilege modes, which means `mstatus.MPP` stops being
      hardwired to M, `sret`/`sfence.vma` become legal, and `ecall` reports its
      originating mode.
- [ ] Give the memory model a decoded address map and access faults
      (`LoadAccessFault`/`StoreAccessFault`). It is currently a per-byte sparse
      map where every address is backed: correct for arithmetic, but it cannot
      model an unmapped region and will be slow for full-suite runs.
- [ ] Add a stable per-instruction trace format containing PC, instruction,
      register/CSR changes, and memory writes for differential testing.
- [ ] Add a README covering build, test, raw-binary generation, CLI usage, and
      what CI does and does not cover. Note that `cargo`/`rustc` are not on the
      default `PATH` in this development environment (`~/.cargo/bin`), and that no
      RISC-V cross toolchain, Spike, QEMU, Verilator, or yosys is installed here —
      `scripts/riscv-smoke.sh` skips itself locally and `--require` makes it fail,
      which is what CI uses.

## Later

- [ ] Implement the compressed `C` extension to reach the RV32IMC target.
  `Trap::Unsupported("C")` already reports the encoding when one appears.
- [ ] Build the first single-cycle RTL core and compare it against this model.
- [ ] Add constrained-random instruction generation and Spike differential
      testing.
- [ ] Add bus, boot ROM, RAM, UART, and interrupt-controller models for SoC
      integration, replacing the model time base with CLINT `mtime`.
