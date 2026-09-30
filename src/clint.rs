//! The core-local interruptor (CLINT).
//!
//! A CLINT is where a hart's timer and software-interrupt pending bits actually
//! live. The model needs it for two reasons that the CPU cannot supply on its
//! own: `mtime` has to become a readable *and writable* memory-mapped register
//! rather than a counter the model fabricates, and something has to be able to
//! assert `mip` so that interrupt delivery can be exercised at all.
//!
//! # Layout
//!
//! The SiFive FE310 arrangement, which is what the rest of the RISC-V world
//! settled on:
//!
//! | Offset | Register |
//! |--------|----------|
//! | `0x0200_0000` | `msip` — machine software interrupt pending, bit 0 |
//! | `0x0200_4000` | `mtimecmp` — timer deadline, 64-bit |
//! | `0x0200_BFF8` | `mtime` — the time base, 64-bit |
//!
//! `mtimecmp` reads back as all ones when unset, so a freshly reset hart is not
//! immediately interrupted.
//!
//! # The time base ticks once per step
//!
//! Real `mtime` is driven by a clock. Here it advances by one per instruction
//! retired, which is the same convention the model used before a CLINT existed
//! and keeps runs deterministic and reproducible. A guest that writes `mtime`
//! sets it until the next step, so a deadline can still be expressed.

use std::cell::Cell;

use crate::mem::Device;

/// `mip` bit for the machine software interrupt.
pub const MIP_MSIP: u32 = 1 << 3;
/// `mip` bit for the machine timer interrupt.
pub const MIP_MTIP: u32 = 1 << 7;
/// `mip` bit for the machine external interrupt.
pub const MIP_MEIP: u32 = 1 << 11;

/// Every `mip` bit a CLINT can assert. `MEIP` belongs to a PLIC, which this
/// model does not have, so it is never actually raised; it is listed because
/// `mie` allows it to be enabled and a pending bit that could never be set would
/// be a silent lie in the CSR file.
pub const CLINT_MASK: u32 = MIP_MSIP | MIP_MTIP | MIP_MEIP;

/// A core-local interruptor: `msip`, `mtimecmp`, and `mtime`.
#[derive(Debug)]
pub struct Clint {
    msip: Cell<bool>,
    mtimecmp: Cell<u64>,
    mtime: Cell<u64>,
}

impl Default for Clint {
    fn default() -> Self {
        Self::new()
    }
}

impl Clint {
    /// A reset CLINT: no software interrupt, `mtime` at zero, and `mtimecmp`
    /// parked at the maximum so no timer interrupt is pending.
    pub fn new() -> Self {
        Clint {
            msip: Cell::new(false),
            mtimecmp: Cell::new(u64::MAX),
            mtime: Cell::new(0),
        }
    }

    /// The current time base.
    pub fn mtime(&self) -> u64 {
        self.mtime.get()
    }

    /// Set the time base, as a guest writing `mtime` would.
    pub fn set_mtime(&self, value: u64) {
        self.mtime.set(value);
    }

    /// Advance the time base by one instruction's worth. Called by the CPU once
    /// per step, after latching, so a guest that reads `mtime` sees the value
    /// for the step it is running in.
    pub fn tick_time(&self) {
        self.mtime.set(self.mtime.get().wrapping_add(1));
    }

    /// The timer deadline.
    pub fn mtimecmp(&self) -> u64 {
        self.mtimecmp.get()
    }

    /// Is the machine software interrupt asserted?
    pub fn software_interrupt_pending(&self) -> bool {
        self.msip.get()
    }

    /// Assert or clear the machine software interrupt.
    pub fn set_software_interrupt_pending(&self, pending: bool) {
        self.msip.set(pending);
    }

    /// Is the timer interrupt pending? The CLINT asserts `MTIP` for as long as
    /// `mtime >= mtimecmp`, which is what makes a deadline a level rather than
    /// an edge: a missed deadline stays pending.
    pub fn timer_interrupt_pending(&self) -> bool {
        self.mtime.get() >= self.mtimecmp.get()
    }
}

impl Device for Clint {
    fn read(&self, offset: u32, len: u32) -> u64 {
        let value = match offset {
            MSIP => u64::from(self.msip.get()),
            MTIMECMP => self.mtimecmp.get(),
            MTIME => self.mtime.get(),
            _ => 0,
        };
        // Sub-word reads take the low bytes of the register, matching the
        // little-endian rule the rest of the address space uses.
        let mask = if len >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * len)) - 1
        };
        value & mask
    }

    fn write(&self, offset: u32, _len: u32, value: u64) {
        match offset {
            // Only bit 0 of msip is implemented; the rest of the word reads as
            // zero and writes to it are discarded, which is a legal WARL result.
            MSIP => self.msip.set(value & 1 != 0),
            MTIMECMP => self.mtimecmp.set(value),
            MTIME => self.mtime.set(value),
            _ => {}
        }
    }

    fn interrupt_pending(&self) -> u32 {
        let mut pending = 0;
        if self.software_interrupt_pending() {
            pending |= MIP_MSIP;
        }
        if self.timer_interrupt_pending() {
            pending |= MIP_MTIP;
        }
        pending
    }

    fn time_base(&self) -> Option<u64> {
        Some(self.mtime.get())
    }

    fn tick(&self) {
        self.tick_time();
    }
}

/// Byte offset of `msip` within a CLINT region.
pub const MSIP: u32 = 0x0000;
/// Byte offset of `mtimecmp` within a CLINT region.
pub const MTIMECMP: u32 = 0x4000;
/// Byte offset of `mtime` within a CLINT region.
pub const MTIME: u32 = 0xBFF8;

/// Total size a CLINT region needs to cover all three registers.
pub const SIZE: u64 = 0xC000;
