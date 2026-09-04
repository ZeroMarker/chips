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

## Next

- [ ] Route synchronous exceptions through machine trap entry using `mepc`,
      `mcause`, `mtval`, and `mtvec`.
- [ ] Implement `mret` and the required `mstatus` machine-mode fields.
- [ ] Define a time source for the `time`/`timeh` CSRs.
- [ ] Convert instruction coverage to table-driven tests and cover every RV32I,
      RV32M, and Zicsr operation plus architectural edge cases.
- [ ] Add a runner for the official `riscv-tests` `rv32ui` and `rv32mi` suites.
- [ ] Add CI for formatting, Clippy, unit tests, and ISA compliance tests.
- [ ] Add a stable per-instruction trace format containing PC, instruction,
      register/CSR changes, and memory writes for differential testing.
- [ ] Improve the CLI with configurable instruction limits, ABI register names,
      and optional trace output.
- [ ] Add a README covering build, test, raw-binary generation, and CLI usage.

## Later

- [ ] Implement the compressed `C` extension to reach the RV32IMC target.
- [ ] Build the first single-cycle RTL core and compare it against this model.
- [ ] Add constrained-random instruction generation and Spike differential
      testing.
- [ ] Add bus, boot ROM, RAM, UART, and interrupt-controller models for SoC
      integration.
