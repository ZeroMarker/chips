//! `chips` — RISC-V functional model (Track A).
//!
//! This crate is the software golden reference for the RISC-V chip project.
//! It implements an executable RV32IMZicsr core: instruction decode, a register
//! file, a byte-addressable memory model, a CSR file with WARL field semantics,
//! machine-mode trap entry through `mtvec`, and an execute loop. It is the
//! reference against which the hardware RTL is differentially tested (see
//! `docs/ROADMAP.md`, phase P3).
//!
//! Trap delivery is summarized in [`cpu`]: a non-zero `mtvec` means a handler is
//! installed, `mepc`/`mcause`/`mtval` describe the cause, and `mret` unwinds.
//!
//! Not yet implemented: `C` (compressed), `A` (atomics), `F`/`D` (floating
//! point), virtual memory, the S/U privilege modes, and interrupt delivery.
//! Encodings that require a missing extension raise [`cpu::Trap::Unsupported`]
//! or [`cpu::Trap::IllegalInstruction`] rather than silently producing a wrong
//! result. `time`/`timeh` read a time base that advances once per step; the
//! writable memory-mapped `mtime` and the interrupt controller arrive with the
//! SoC phase.

pub mod cpu;
pub mod csr;
pub mod isa;
pub mod mem;

pub use cpu::{Cpu, StepOutcome, StopReason, Trap};
pub use mem::Memory;
