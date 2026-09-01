//! `chips` — RISC-V functional model (Track A).
//!
//! This crate is the software golden reference for the RISC-V chip project.
//! It implements an executable RV32IM core: instruction decode, a register
//! file, a byte-addressable memory model, a CSR container, and an execute
//! loop. It is the reference against which the hardware RTL is differentially
//! tested (see `ROADMAP.md`, phase P3).
//!
//! Not yet implemented: Zicsr/CSR instructions, `C` (compressed), `A`
//! (atomics), `F`/`D` (floating point), virtual memory (`satp`). Encountering
//! an unimplemented extension raises [`cpu::Trap::Unsupported`] rather than
//! silently producing a wrong result.

pub mod cpu;
pub mod csr;
pub mod isa;
pub mod mem;

pub use cpu::{Cpu, StopReason, Trap};
pub use mem::Memory;
