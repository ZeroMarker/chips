//! `chips` — RISC-V functional model (Track A).
//!
//! This crate is the software golden reference for the RISC-V chip project.
//! It implements an executable RV32IMZicsr core: instruction decode, a register
//! file, a byte-addressable memory model with a decoded address map, a CSR file
//! with WARL field semantics, M/S trap entry through `mtvec`/`stvec`, and an
//! execute loop. It is the reference against which the hardware RTL is
//! differentially tested (see `docs/ROADMAP.md`, phase P3).
//!
//! Trap delivery is summarized in [`cpu`]: `medeleg`/`mideleg` select the
//! target, the matching EPC/cause/tval CSRs describe it, and MRET/SRET unwind.
//!
//! Memory comes in two shapes, described in [`mem`]: [`Memory::permissive`]
//! backs the whole address space so instruction-level tests cannot be derailed
//! by layout, while [`Memory::from_regions`] installs a decoded map so an
//! unmapped access raises an access fault.
//!
//! Not yet implemented: `C` (compressed), `A` (atomics), `F`/`D` (floating
//! point), virtual memory, and external Debug Mode. M/S/U privileges, delegated
//! traps, CLINT, software-injected supervisor interrupts, and sixteen PMP
//! entries with MPRV data-access checks are implemented. Two native type-2
//! address triggers raise precise breakpoint exceptions with MIE/SIE gating.
//! Encodings that require a missing extension raise [`cpu::Trap::Unsupported`]
//! or [`cpu::Trap::IllegalInstruction`] rather than silently producing a wrong
//! result. `time`/`timeh` follow a mapped CLINT, with a per-step fallback time
//! base when no CLINT exists. Address translation remains Bare-only.

pub mod clint;
pub mod cpu;
pub mod csr;
pub mod htif;
pub mod isa;
pub mod mem;
pub mod platform;
pub mod pmp;
pub mod trace;
pub mod triggers;

pub use clint::Clint;
pub use cpu::{Cpu, StepOutcome, StopReason, Trap};
pub use htif::{Htif, Outcome};
pub use mem::{Access, AccessFault, Device, Memory, Permissions, Region};
pub use trace::{Change, Record, StepResult, Trace};
