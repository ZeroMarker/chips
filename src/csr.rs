//! Control and status register (CSR) container.
//!
//! CSR *addressing* is modeled here with the standard addresses and a sparse
//! backing store, together with the field semantics the architecture requires:
//!
//! - **Existence**: accessing a CSR that `exists` does not report is an illegal
//!   instruction. The caller checks this before touching the file.
//! - **Read-only CSRs**: writes to the machine-information registers, to `misa`,
//!   to `mip`, and to anything whose address has `csr[11:10] == 0b11` are
//!   discarded. The Zicsr instructions trap instead where the architecture
//!   requires it (see `cpu::execute_csr`); discarding here keeps the store
//!   consistent no matter who calls it.
//! - **WARL fields**: registers such as `mstatus`, `mtvec` and `mepc` only
//!   store and return values the architecture permits, so a write of an
//!   unsupported value is legalized rather than trapped.
//!
//! The counter/time CSRs (`mcycle`, `minstret`, `cycle`, `time`, …) are listed
//! as existing but hold no storage: the CPU services them from its own state,
//! which is why reading one here would return 0.

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
    // Machine counters (read/write).
    pub const MCYCLE: u32 = 0xB00;
    pub const MINSTRET: u32 = 0xB02;
    pub const MCYCLEH: u32 = 0xB80;
    pub const MINSTRETH: u32 = 0xB82;
    // Counters (unprivileged read-only aliases).
    pub const CYCLE: u32 = 0xC00;
    pub const TIME: u32 = 0xC01;
    pub const INSTRET: u32 = 0xC02;
    pub const CYCLEH: u32 = 0xC80;
    pub const TIMEH: u32 = 0xC81;
    pub const INSTRETH: u32 = 0xC82;
}

/// `misa` for this hart: MXL = 1 (32-bit registers), extensions `I` and `M`.
/// Zicsr is implied by the base and has no `misa` bit.
pub const MISA_VALUE: u32 = (1 << 30) | (1 << 8) | (1 << 12);

/// `mstatus.MIE` — machine-mode interrupt enable.
pub const MSTATUS_MIE: u32 = 1 << 3;
/// `mstatus.MPIE` — interrupt enable saved on trap entry.
pub const MSTATUS_MPIE: u32 = 1 << 7;
/// `mstatus.MPP` — previous privilege. `xPP` is a WARL field that can only hold
/// mode x or an implemented lower mode, so with M as the only implemented mode
/// MPP legalizes to (and is effectively hardwired at) M.
pub const MSTATUS_MPP_M: u32 = 0b11 << 11;

/// Bits of `mstatus` this model stores; everything else reads as zero. MPP has
/// no writable alternatives to legalize to, so it is constant here.
const MSTATUS_WRITABLE: u32 = MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP_M;

/// `mie` bits for the machine-mode interrupt sources: MEIE (11), MTIE (7) and
/// MSIE (3). Supervisory and user-level enable bits are absent because those
/// privilege modes do not exist in this model.
const MIE_WRITABLE: u32 = (1 << 11) | (1 << 7) | (1 << 3);

/// `mepc` holds an instruction address; with IALIGN = 32 the low two bits are
/// always zero.
const MEPC_MASK: u32 = !0b11;

/// A sparse CSR register file with architectural field semantics.
#[derive(Debug, Default)]
pub struct Csr {
    data: BTreeMap<u32, u32>,
}

impl Csr {
    /// Create an empty CSR file.
    pub fn new() -> Self {
        Self::default()
    }

    /// Does this model implement `csr`? Accessing any other address is an
    /// illegal instruction.
    pub fn exists(csr: u32) -> bool {
        matches!(
            csr,
            addr::MVENDORID
                | addr::MARCHID
                | addr::MIMPID
                | addr::MHARTID
                | addr::MSTATUS
                | addr::MISA
                | addr::MIE
                | addr::MTVEC
                | addr::MSCRATCH
                | addr::MEPC
                | addr::MCAUSE
                | addr::MTVAL
                | addr::MIP
                | addr::MCYCLE
                | addr::MINSTRET
                | addr::MCYCLEH
                | addr::MINSTRETH
                | addr::CYCLE
                | addr::TIME
                | addr::INSTRET
                | addr::CYCLEH
                | addr::TIMEH
                | addr::INSTRETH
        )
    }

    /// Is `csr` read-only? The architecture encodes that in `csr[11:10]`.
    pub fn is_read_only(csr: u32) -> bool {
        (csr >> 10) & 0b11 == 0b11
    }

    /// Read a CSR, applying the register's read mask. Unwritten CSRs read as 0.
    pub fn read(&self, csr: u32) -> u32 {
        let raw = self.data.get(&csr).copied().unwrap_or(0);
        match csr {
            // Fixed-value registers.
            addr::MISA => MISA_VALUE,
            // No interrupt sources exist yet, so every pending bit reads zero.
            addr::MIP => 0,
            // MPP is hardwired to M; MIE/MPIE are normal storage.
            addr::MSTATUS => (raw & MSTATUS_WRITABLE) | MSTATUS_MPP_M,
            addr::MIE => raw & MIE_WRITABLE,
            addr::MTVEC => legalize_mtvec(raw),
            addr::MEPC => raw & MEPC_MASK,
            _ => raw,
        }
    }

    /// Write a CSR, legalizing WARL fields and discarding read-only ones.
    pub fn write(&mut self, csr: u32, val: u32) {
        if Self::is_read_only(csr) {
            return;
        }
        let stored = match csr {
            // Constant registers: writes are ignored (a legal WARL outcome).
            addr::MISA
            | addr::MIP
            | addr::MVENDORID
            | addr::MARCHID
            | addr::MIMPID
            | addr::MHARTID => return,
            addr::MSTATUS => (val & MSTATUS_WRITABLE) | MSTATUS_MPP_M,
            addr::MIE => val & MIE_WRITABLE,
            addr::MTVEC => legalize_mtvec(val),
            addr::MEPC => val & MEPC_MASK,
            _ => val,
        };
        self.data.insert(csr, stored);
    }
}

/// `mtvec` is WARL. Modes 0 (direct) and 1 (vectored) exist; modes 2 and 3 are
/// reserved and legalize to 0. The base needs no extra masking because it
/// occupies `mtvec[31:2]`, which is four-byte aligned by construction.
fn legalize_mtvec(val: u32) -> u32 {
    let mode = if val & 0b11 == 1 { 1 } else { 0 };
    (val & !0b11) | mode
}
