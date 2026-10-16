//! Control and status register (CSR) container.
//!
//! CSR *addressing* is modeled here with the standard addresses and a sparse
//! backing store, together with the field semantics the architecture requires:
//!
//! - **Existence**: accessing a CSR that `exists` does not report is an illegal
//!   instruction. The caller checks this before touching the file.
//! - **Privilege**: each address implies a minimum privilege from its `csr[9:8]`
//!   field, and an access from below it is an illegal instruction. See
//!   [`Csr::required_privilege`]; the check itself belongs to the CPU, which
//!   knows the current mode.
//! - **Read-only CSRs**: writes to the machine-information registers, to `misa`,
//!   and to anything whose address has `csr[11:10] == 0b11` are
//!   discarded. The Zicsr instructions trap instead where the architecture
//!   requires it (see `cpu::execute_csr`); discarding here keeps the store
//!   consistent no matter who calls it.
//! - **WARL fields**: registers such as `mstatus`, `mtvec` and `mepc` only
//!   store and return values the architecture permits, so a write of an
//!   unsupported value is legalized rather than trapped.
//!
//! # S-mode aliases
//!
//! `sstatus`, `sie`, and `sip` are narrower views onto `mstatus`, `mie`, and
//! `mip`. Interrupt views expose only bits delegated in `mideleg`. Writing through an alias therefore
//! updates only the fields that alias can see and leaves the rest alone, which
//! is what the architecture requires.
//!
//! The counter/time CSRs (`mcycle`, `minstret`, `cycle`, `time`, …) are listed
//! as existing but hold no storage: the CPU services them from its own state,
//! which is why reading one here would return 0.

use std::collections::BTreeMap;

use crate::isa::Privilege;
use crate::pmp::Pmp;
use crate::triggers::Triggers;

/// Standard CSR addresses (unprivileged, supervisor, and machine level).
pub mod addr {
    // Unprivileged performance counters: readable from every mode, writable from
    // none. The machine aliases below are the writable forms.
    pub const CYCLE: u32 = 0xC00;
    pub const TIME: u32 = 0xC01;
    pub const INSTRET: u32 = 0xC02;
    pub const CYCLEH: u32 = 0xC80;
    pub const TIMEH: u32 = 0xC81;
    pub const INSTRETH: u32 = 0xC82;

    // Supervisor trap/status.
    pub const SSTATUS: u32 = 0x100;
    pub const SIE: u32 = 0x104;
    pub const SIP: u32 = 0x144;
    pub const STVEC: u32 = 0x105;
    pub const SSCRATCH: u32 = 0x140;
    pub const SEPC: u32 = 0x141;
    pub const SCAUSE: u32 = 0x142;
    pub const STVAL: u32 = 0x143;
    pub const SATP: u32 = 0x180;

    /// Non-maskable interrupt status. A hart with no NMI source reports zero and
    /// ignores writes, which is a legal WARL outcome rather than a missing
    /// register: `riscv-tests` probes it to find out whether NMI exists.
    pub const MNSTATUS: u32 = 0x744;
    /// Machine counter inhibit. Bit 1 inhibits `cycle`, bit 2 inhibits
    /// `instret`. Unlike most of the optional registers this one has behaviour
    /// the model can honour, so it is implemented rather than stubbed.
    pub const MCOUNTINHIBIT: u32 = 0x320;

    // Machine information.
    pub const MVENDORID: u32 = 0xF11;
    pub const MARCHID: u32 = 0xF12;
    pub const MIMPID: u32 = 0xF13;
    pub const MHARTID: u32 = 0xF14;
    // Machine trap/status.
    pub const MSTATUS: u32 = 0x300;
    pub const MISA: u32 = 0x301;
    pub const MEDELEG: u32 = 0x302;
    pub const MIDELEG: u32 = 0x303;
    pub const MIE: u32 = 0x304;
    pub const MTVEC: u32 = 0x305;
    pub const MSCRATCH: u32 = 0x340;
    pub const MEPC: u32 = 0x341;
    pub const MCAUSE: u32 = 0x342;
    pub const MTVAL: u32 = 0x343;
    pub const MIP: u32 = 0x344;
    /// First of four packed RV32 PMP configuration CSRs (0x3A0..=0x3A3).
    pub const PMPCFG0: u32 = 0x3A0;
    /// First of sixteen PMP address CSRs (0x3B0..=0x3BF).
    pub const PMPADDR0: u32 = 0x3B0;
    /// First native trigger CSR; offsets 0..=4 are tselect/tdata1-3/tinfo.
    pub const TSELECT: u32 = 0x7A0;
    pub const TDATA1: u32 = 0x7A1;
    pub const TDATA2: u32 = 0x7A2;
    pub const TDATA3: u32 = 0x7A3;
    pub const TINFO: u32 = 0x7A4;
    // Machine counters (read/write).
    pub const MCYCLE: u32 = 0xB00;
    pub const MINSTRET: u32 = 0xB02;
    pub const MCYCLEH: u32 = 0xB80;
    pub const MINSTRETH: u32 = 0xB82;
}

/// `misa` for this hart: MXL = 1 (32-bit registers), extensions `I` and `M`.
/// Zicsr is implied by the base and has no `misa` bit.
pub const MISA_VALUE: u32 = (1 << 30) | (1 << 8) | (1 << 12);

// `mstatus` fields. The S and M variants of the interrupt-enable stack are
// separate bits of the same register.
pub const MSTATUS_SIE: u32 = 1 << 1;
pub const MSTATUS_MIE: u32 = 1 << 3;
pub const MSTATUS_SPIE: u32 = 1 << 5;
pub const MSTATUS_MPIE: u32 = 1 << 7;
/// `mstatus.SPP` — previous privilege for `sret`. One bit: U or S, never M.
pub const MSTATUS_SPP: u32 = 1 << 8;
/// Intercept SRET in S-mode; M-mode can still execute it.
pub const MSTATUS_TSR: u32 = 1 << 22;
/// Use MPP as the effective privilege for explicit data accesses.
pub const MSTATUS_MPRV: u32 = 1 << 17;
/// `mstatus.MPP` — previous privilege for `mret`. Two bits, so it can hold M.
pub const MSTATUS_MPP_SHIFT: u32 = 11;
pub const MSTATUS_MPP: u32 = 0b11 << MSTATUS_MPP_SHIFT;

/// `sstatus` is `mstatus` minus the fields the supervisor may not touch, so it
/// sees `SIE`/`SPIE`/`SPP` and nothing else. `UXL`/`SXL` are not implemented and
/// therefore not present.
const SSTATUS_MASK: u32 = MSTATUS_SIE | MSTATUS_SPIE | MSTATUS_SPP;

/// Bits of `mstatus` this model stores; everything else reads as zero.
const MSTATUS_WRITABLE: u32 = MSTATUS_SIE
    | MSTATUS_MIE
    | MSTATUS_SPIE
    | MSTATUS_MPIE
    | MSTATUS_SPP
    | MSTATUS_MPP
    | MSTATUS_TSR
    | MSTATUS_MPRV;

/// Supervisor pending bits. M-mode can inject all three; S-mode can write
/// only SSIP, and only when it has been delegated.
pub const SUPERVISOR_INTERRUPTS: u32 = (1 << 1) | (1 << 5) | (1 << 9);
const INTERRUPT_MASK: u32 = (1 << 3) | (1 << 7) | (1 << 11) | SUPERVISOR_INTERRUPTS;
/// Exceptions this model can delegate. M-mode ECALL (11) cannot be delegated.
const EXCEPTION_DELEGATION_MASK: u32 = 0x3ff;

/// `mcountinhibit` implements CY (bit 1) and IR (bit 2). The rest are reserved.
const MCOUNTINHIBIT_WRITABLE: u32 = (1 << 1) | (1 << 2);

/// `mepc` and `sepc` hold an instruction address; with IALIGN = 32 the low two
/// bits are always zero.
const EPC_MASK: u32 = !0b11;

/// A sparse CSR register file with architectural field semantics.
#[derive(Debug, Default)]
pub struct Csr {
    data: BTreeMap<u32, u32>,
    pmp: Pmp,
    triggers: Triggers,
}

impl Csr {
    /// Create an empty CSR file.
    pub fn new() -> Self {
        Self::default()
    }

    /// Does this model implement `csr`? Accessing any other address is an
    /// illegal instruction.
    pub fn exists(csr: u32) -> bool {
        (addr::TSELECT..=addr::TINFO).contains(&csr)
            || (addr::PMPCFG0..addr::PMPCFG0 + 4).contains(&csr)
            || (addr::PMPADDR0..addr::PMPADDR0 + 16).contains(&csr)
            || matches!(
                csr,
                // Unprivileged counters.
                addr::CYCLE
                | addr::TIME
                | addr::INSTRET
                | addr::CYCLEH
                | addr::TIMEH
                | addr::INSTRETH
                // Supervisor.
                | addr::SSTATUS
                | addr::SIE
                | addr::SIP
                | addr::STVEC
                | addr::SSCRATCH
                | addr::SEPC
                | addr::SCAUSE
                | addr::STVAL
                | addr::SATP
                // Machine information.
                | addr::MVENDORID
                | addr::MARCHID
                | addr::MIMPID
                | addr::MHARTID
                | addr::MSTATUS
                | addr::MISA
                | addr::MEDELEG
                | addr::MIDELEG
                | addr::MIE
                | addr::MTVEC
                | addr::MSCRATCH
                | addr::MEPC
                | addr::MCAUSE
                | addr::MTVAL
                | addr::MIP
                | addr::MNSTATUS
                | addr::MCOUNTINHIBIT
                | addr::MCYCLE
                | addr::MINSTRET
                | addr::MCYCLEH
                | addr::MINSTRETH
            )
    }

    /// The privilege a register requires, from its address.
    ///
    /// `csr[9:8]` encodes the level: `0b00` is unprivileged (`fcsr` 0x003,
    /// `cycle` 0xC00), `0b01` is supervisor (`sstatus` 0x100, `satp` 0x180),
    /// `0b10` is reserved, and `0b11` is machine (`mstatus` 0x300, `mcycle`
    /// 0xB00, `mhartid` 0xF14).
    ///
    /// Reserved is treated as machine-only, which is the safe direction — a
    /// register this model does not implement should not become reachable
    /// because of how its address happens to read.
    pub fn required_privilege(csr: u32) -> Privilege {
        match (csr >> 8) & 0b11 {
            0b00 => Privilege::User,
            0b01 => Privilege::Supervisor,
            _ => Privilege::Machine,
        }
    }

    /// Is `csr` read-only? The architecture encodes that in `csr[11:10]`.
    pub fn is_read_only(csr: u32) -> bool {
        (csr >> 10) & 0b11 == 0b11
    }

    /// Read a CSR, applying the register's read mask. Unwritten CSRs read as 0.
    ///
    /// For `mip`/`sip`, this returns the software-writable pending bits.
    /// [`crate::cpu::Cpu`] ORs them with the live device signals on a CSR read.
    ///
    /// The caller performs the privilege check, so an access from too low a level
    /// is reported as an illegal instruction rather than as a read of state that
    /// belongs to a more privileged mode.
    pub fn read(&self, csr: u32) -> u32 {
        if (addr::TSELECT..=addr::TINFO).contains(&csr) {
            return self.triggers.read(csr - addr::TSELECT);
        }
        if (addr::PMPCFG0..addr::PMPCFG0 + 4).contains(&csr) {
            return self.pmp.read_config((csr - addr::PMPCFG0) as usize);
        }
        if (addr::PMPADDR0..addr::PMPADDR0 + 16).contains(&csr) {
            return self.pmp.read_address((csr - addr::PMPADDR0) as usize);
        }
        let raw = self.data.get(&storage_for(csr)).copied().unwrap_or(0);
        match csr {
            // Fixed-value registers.
            addr::MISA => MISA_VALUE,
            addr::MIP => raw & SUPERVISOR_INTERRUPTS,
            addr::SIP => raw & self.read(addr::MIDELEG),
            addr::SIE => raw & self.read(addr::MIDELEG),
            addr::MEDELEG => raw & EXCEPTION_DELEGATION_MASK,
            addr::MIDELEG => raw & SUPERVISOR_INTERRUPTS,
            // No NMI source exists, so this is constantly zero however it is
            // written. Reporting it as a register that exists but does nothing
            // is what lets software feature-detect instead of trapping.
            addr::MNSTATUS => 0,
            // The narrow views.
            addr::SSTATUS => raw & SSTATUS_MASK,
            addr::MIE => raw & INTERRUPT_MASK,
            addr::MSTATUS => legalize_mstatus(raw),
            addr::MCOUNTINHIBIT => raw & MCOUNTINHIBIT_WRITABLE,
            addr::MTVEC | addr::STVEC => legalize_vector(raw),
            addr::MEPC | addr::SEPC => raw & EPC_MASK,
            addr::SATP => legalize_satp(raw),
            _ => raw,
        }
    }

    /// Write a CSR, legalizing WARL fields and discarding read-only ones.
    ///
    /// Writing through an alias (`sstatus`, `sie`) updates only the fields that
    /// alias can see, so a supervisor cannot disturb `mstatus.MIE` by writing
    /// `sstatus`.
    pub fn write(&mut self, csr: u32, val: u32) {
        if (addr::TSELECT..=addr::TINFO).contains(&csr) {
            self.triggers.write(csr - addr::TSELECT, val);
            return;
        }
        if (addr::PMPCFG0..addr::PMPCFG0 + 4).contains(&csr) {
            self.pmp.write_config((csr - addr::PMPCFG0) as usize, val);
            return;
        }
        if (addr::PMPADDR0..addr::PMPADDR0 + 16).contains(&csr) {
            self.pmp.write_address((csr - addr::PMPADDR0) as usize, val);
            return;
        }
        if Self::is_read_only(csr) {
            return;
        }
        match csr {
            // Constant registers: writes are ignored, which is a legal WARL
            // outcome rather than a fault.
            addr::MISA
            | addr::MNSTATUS
            | addr::MVENDORID
            | addr::MARCHID
            | addr::MIMPID
            | addr::MHARTID => return,
            _ => {}
        }

        let slot = storage_for(csr);
        let previous = self.data.get(&slot).copied().unwrap_or(0);
        let merged = match csr {
            addr::SSTATUS => previous & !SSTATUS_MASK | (val & SSTATUS_MASK),
            addr::SIE => {
                let mask = self.read(addr::MIDELEG);
                previous & !mask | (val & mask)
            }
            addr::SIP => {
                let mask = self.read(addr::MIDELEG) & (1 << 1);
                previous & !mask | (val & mask)
            }
            _ => val,
        };
        let stored = match slot {
            addr::MSTATUS => legalize_mstatus(merged),
            addr::MIE => merged & INTERRUPT_MASK,
            addr::MIP => merged & SUPERVISOR_INTERRUPTS,
            addr::MEDELEG => merged & EXCEPTION_DELEGATION_MASK,
            addr::MIDELEG => merged & SUPERVISOR_INTERRUPTS,
            addr::MTVEC | addr::STVEC => legalize_vector(merged),
            addr::MEPC | addr::SEPC => merged & EPC_MASK,
            addr::SATP => legalize_satp(merged),
            _ => merged,
        };
        self.data.insert(slot, stored);
    }

    /// Native address trigger state for CPU access checks.
    pub fn triggers(&self) -> &Triggers {
        &self.triggers
    }

    /// Physical memory protection state for CPU access checks.
    pub fn pmp(&self) -> &Pmp {
        &self.pmp
    }

    /// The `mip` bits this machine can ever report.
    ///
    /// The CPU applies this mask to signals collected from the address space.
    /// Software-injected supervisor interrupts are stored separately.
    pub fn mip_mask() -> u32 {
        INTERRUPT_MASK
    }
}

/// Where a CSR's bits actually live.
///
/// `sstatus`, `sie`, and `sip` are windows onto the machine registers rather than
/// registers in their own right; everything else has dedicated storage.
fn storage_for(csr: u32) -> u32 {
    match csr {
        addr::SSTATUS => addr::MSTATUS,
        addr::SIE => addr::MIE,
        addr::SIP => addr::MIP,
        other => other,
    }
}

/// Legalize `mstatus`: keep the implemented fields, and turn the reserved `MPP`
/// encoding into U.
///
/// The architecture says a reserved `MPP` should read as the least privileged
/// supported mode rather than trapping, and with M, S, and U all implemented
/// that is U.
fn legalize_mstatus(raw: u32) -> u32 {
    let mut value = raw & MSTATUS_WRITABLE;
    if Privilege::from_encoding((raw >> MSTATUS_MPP_SHIFT) & 0b11).is_none() {
        value &= !MSTATUS_MPP;
    }
    value
}

/// Legalize a `tvec` mode field: 0 (direct) and 1 (vectored) exist, and the
/// reserved values 2 and 3 become 0. The base needs no masking because it
/// occupies `tvec[31:2]`, which is four-byte aligned by construction.
fn legalize_vector(raw: u32) -> u32 {
    let mode = if raw & 0b11 == 1 { 1 } else { 0 };
    (raw & !0b11) | mode
}

/// This RV32 model implements only Bare, with no ASIDs or page-table root.
/// Unsupported Sv32 writes are ignored; all representable state reads as zero.
fn legalize_satp(_raw: u32) -> u32 {
    0
}
