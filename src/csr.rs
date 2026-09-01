//! Control and status register (CSR) container.
//!
//! CSR *addressing* is modeled here with the standard addresses and a sparse
//! backing store, so the register file and trap plumbing can be wired before
//! the `Zicsr` instructions themselves are implemented. Reading an unwritten
//! CSR returns `0`.

use std::collections::BTreeMap;

/// Standard CSR addresses (unprivileged and machine-level).
pub mod addr {
    // Machine information.
    pub const MVENDORID: u32 = 0xF11;
    pub const MARCHID: u32 = 0xF12;
    pub const MIMPID: u32 = 0xF13;
    pub const MHARTID: u32 = 0xF14;
    // Machine trap/status.
    pub const MSTATUS: u32 = 0x300;
    pub const MISA: u32 = 0x301;
    pub const MIE: u32 = 0x304;
    pub const MTVEC: u32 = 0x305;
    pub const MSCRATCH: u32 = 0x340;
    pub const MEPC: u32 = 0x341;
    pub const MCAUSE: u32 = 0x342;
    pub const MTVAL: u32 = 0x343;
    pub const MIP: u32 = 0x344;
    // Counters (unprivileged read-only).
    pub const CYCLE: u32 = 0xC00;
    pub const TIME: u32 = 0xC01;
    pub const INSTRET: u32 = 0xC02;
}

/// A sparse CSR register file.
#[derive(Debug, Default)]
pub struct Csr {
    data: BTreeMap<u32, u32>,
}

impl Csr {
    /// Create an empty CSR file.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a CSR; unwritten CSRs read as 0.
    pub fn read(&self, csr: u32) -> u32 {
        self.data.get(&csr).copied().unwrap_or(0)
    }

    /// Write a CSR.
    pub fn write(&mut self, csr: u32, val: u32) {
        self.data.insert(csr, val);
    }
}
