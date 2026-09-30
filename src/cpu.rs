//! CPU state and the RV32IMZicsr execute loop.
//!
//! `Cpu` owns the program counter, the integer register file, a CSR container,
//! the cycle/retired-instruction counters, and a model time base. `step`
//! fetches, decodes, and executes one instruction against a `Memory`.
//!
//! # Trap handling
//!
//! A synchronous exception or an `ecall`/`ebreak` enters the machine-mode
//! handler described by `mtvec`: `mepc`, `mcause` and `mtval` are written, the
//! interrupt-enable stack is pushed in `mstatus` (`MIE` into `MPIE`, `MIE`
//! cleared, `MPP` = M), and the PC is set to the `mtvec` base. `mret` unwinds
//! that state: `MIE` is restored from `MPIE`, `MPIE` is set, and the PC returns
//! to `mepc`. Synchronous exceptions always enter at the `mtvec` base, even
//! when `mtvec` selects vectored mode, because only interrupts are vectored.
//!
//! A handler counts as installed when `mtvec` is non-zero. With no handler the
//! model has no architecturally meaningful target to jump to, so exceptions are
//! reported to the caller as a [`Trap`] and `ecall`/`ebreak` end the
//! [`Cpu::run`] loop — which is what lets a bare-metal image halt itself.
//!
//! Only machine mode exists, so `mstatus.MPP` is hardwired to M and `mret`
//! always returns to machine mode. Interrupts are not delivered: `mip` reads
//! zero and `mie` only stores the M-mode enable bits. `wfi` retires
//! immediately because there is no interrupt source to wait for.

use crate::csr::{addr as csr_addr, Csr, MSTATUS_MIE, MSTATUS_MPIE};
use crate::isa::{self, Decoded};
use crate::mem::Memory;

/// Outcome of executing a single instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Instruction retired normally; PC already advanced.
    Continue,
    /// A trap was taken: the PC is now at the `mtvec` handler and the trap CSRs
    /// describe the cause.
    TrapTaken,
    /// `ecall` was executed with no handler installed — the program requested
    /// an environment call.
    Ecall,
    /// `ebreak` was executed with no handler installed — the program halted
    /// (debug breakpoint).
    Ebreak,
}

/// Reason the `run` loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Stopped on `ecall`.
    Ecall,
    /// Stopped on `ebreak`.
    Ebreak,
    /// Instruction budget exhausted — did not halt on its own.
    Limit,
}

/// A synchronous exception raised while executing an instruction.
///
/// The payload is the architectural `mtval` value plus what [`Trap::mtval`]
/// cannot express: for [`Trap::IllegalInstruction`] it is the faulting encoding
/// and for the breakpoint/ecall variants the faulting PC. The faulting PC of
/// any trap is available as `mtval` (`mepc` once routed) and as `Cpu::pc()`
/// before the handler runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    /// A fetch or taken control-flow target is not four-byte aligned.
    InstructionAddressMisaligned(u32),
    /// A fetch or taken control-flow target names no mapped region, or one the
    /// address space does not permit reading. Payload: the faulting address.
    InstructionAccessFault(u32),
    /// The encoding at the current PC is not a valid RV32IMZicsr instruction,
    /// or it names a CSR this model does not implement. Payload: the encoding.
    IllegalInstruction(u32),
    /// `ebreak` was executed and a trap handler is installed. Payload: the PC of
    /// the `ebreak`.
    Breakpoint(u32),
    /// A load address does not meet the accessed value's alignment.
    ///
    /// This model does **not** raise this: it completes misaligned data accesses
    /// instead, which the base ISA permits and `riscv-tests`' `ma_data`
    /// requires. The variant stays because `mcause` 4 is part of the
    /// architecture and a future implementation may prefer to trap.
    LoadAddressMisaligned(u32),
    /// A load names no mapped region, or one that does not permit reading.
    /// Payload: the faulting address.
    LoadAccessFault(u32),
    /// A store address does not meet the accessed value's alignment.
    ///
    /// As with [`Trap::LoadAddressMisaligned`], not raised by this model.
    StoreAddressMisaligned(u32),
    /// A store names no mapped region, or one that does not permit writing.
    /// Payload: the faulting address.
    StoreAccessFault(u32),
    /// `ecall` was executed and a trap handler is installed. Payload: the PC of
    /// the `ecall`.
    EnvironmentCallFromM(u32),
    /// A recognized encoding of an extension this model does not implement.
    /// Architecturally identical to an illegal instruction (`mcause` 2), kept
    /// separate so a driver can report *which* extension is missing.
    Unsupported(&'static str),
}

impl Trap {
    /// The `mcause` value written when this trap enters the machine-mode handler.
    pub fn mcause(&self) -> u32 {
        match self {
            Trap::InstructionAddressMisaligned(_) => 0,
            Trap::InstructionAccessFault(_) => 1,
            Trap::IllegalInstruction(_) | Trap::Unsupported(_) => 2,
            Trap::Breakpoint(_) => 3,
            Trap::LoadAddressMisaligned(_) => 4,
            Trap::LoadAccessFault(_) => 5,
            Trap::StoreAddressMisaligned(_) => 6,
            Trap::StoreAccessFault(_) => 7,
            Trap::EnvironmentCallFromM(_) => 11,
        }
    }

    /// The `mtval` value written when this trap enters the machine-mode handler.
    pub fn mtval(&self) -> u32 {
        match self {
            Trap::InstructionAddressMisaligned(val)
            | Trap::InstructionAccessFault(val)
            | Trap::LoadAddressMisaligned(val)
            | Trap::LoadAccessFault(val)
            | Trap::StoreAddressMisaligned(val)
            | Trap::StoreAccessFault(val)
            | Trap::Breakpoint(val) => *val,
            Trap::IllegalInstruction(encoding) => *encoding,
            // ECALL carries no address, and an unimplemented extension has no
            // encoding-specific value to report.
            Trap::EnvironmentCallFromM(_) | Trap::Unsupported(_) => 0,
        }
    }
}

/// A RISC-V CPU with RV32IM integer execution.
pub struct Cpu {
    pc: u32,
    x: [u32; 32],
    csr: Csr,
    cycle: u64,
    instret: u64,
    mtime: u64,
    /// Set when the instruction being executed wrote `minstret`, so that
    /// instruction is not counted against the counter it just set.
    instret_written: bool,
    /// The `mip` bits the address space's devices currently assert. Latched once
    /// per step, before the interrupt check, so a handler that reads `mip` sees
    /// the state that caused it to be entered.
    mip: u32,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Create a fresh CPU: PC = 0, all registers zero, empty CSR file, no trap
    /// handler installed (`mtvec` = 0) and no interrupts enabled.
    pub fn new() -> Self {
        Cpu {
            pc: 0,
            x: [0; 32],
            csr: Csr::new(),
            cycle: 0,
            instret: 0,
            mtime: 0,
            instret_written: false,
            mip: 0,
        }
    }

    /// Current program counter.
    pub fn pc(&self) -> u32 {
        self.pc
    }

    /// Set the program counter (e.g. to a reset vector).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
    }

    /// Read an integer register. `x0` is always zero.
    pub fn reg(&self, i: u32) -> u32 {
        self.x[(i as usize) & (isa::NUM_REGS - 1)]
    }

    /// Read-only access to the CSR file.
    pub fn csr(&self) -> &Csr {
        &self.csr
    }

    /// Number of attempted instruction steps, including trapping steps.
    pub fn cycle(&self) -> u64 {
        self.cycle
    }

    /// Number of instructions that completed without trapping or stopping.
    pub fn instret(&self) -> u64 {
        self.instret
    }

    /// Value returned by the `time`/`timeh` CSRs.
    ///
    /// The model advances it once per step. A real hart reads this from the
    /// memory-mapped `mtime` register of the interrupt controller, which arrives
    /// with the SoC phase; until then this is the time base.
    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    /// Set the model time base, e.g. from a future CLINT implementation.
    pub fn set_mtime(&mut self, mtime: u64) {
        self.mtime = mtime;
    }

    /// Is a trap handler installed? The model treats a non-zero `mtvec` as the
    /// presence of one.
    pub fn handler_installed(&self) -> bool {
        self.csr.read(csr_addr::MTVEC) != 0
    }

    #[inline]
    fn x(&self, rs: u32) -> u32 {
        self.x[(rs as usize) & (isa::NUM_REGS - 1)]
    }

    /// Write `rd`; writes to `x0` are discarded (it is hard-wired to zero).
    #[inline]
    fn write_rd(&mut self, rd: u32, val: u32) {
        if rd != 0 {
            self.x[(rd as usize) & (isa::NUM_REGS - 1)] = val;
        }
    }

    /// Execute instructions until an `ecall`/`ebreak` or the instruction
    /// budget is exhausted. Taken traps do not stop the loop.
    pub fn run(&mut self, mem: &mut Memory, budget: u64) -> Result<StopReason, Trap> {
        for _ in 0..budget {
            match self.step(mem)? {
                StepOutcome::Continue | StepOutcome::TrapTaken => {}
                StepOutcome::Ecall => return Ok(StopReason::Ecall),
                StepOutcome::Ebreak => return Ok(StopReason::Ebreak),
            }
        }
        Ok(StopReason::Limit)
    }

    /// Fetch, decode, and execute a single instruction.
    pub fn step(&mut self, mem: &mut Memory) -> Result<StepOutcome, Trap> {
        let pc = self.pc;
        self.cycle = self.cycle.wrapping_add(1);
        self.instret_written = false;

        // Refresh the values the interrupt controller owns before anything can
        // observe them, and let it advance the time base. With no CLINT in the
        // address space the model keeps its own counter, which is what an
        // instruction-level test on permissive memory sees.
        self.mip = mem.pending_interrupts() & Csr::mip_mask();
        match mem.time_base() {
            Some(t) => self.mtime = t,
            None => self.mtime = self.mtime.wrapping_add(1),
        }
        mem.tick_time_base();

        // An enabled, pending interrupt is taken before the next instruction
        // starts, so `mepc` points at the instruction that did not run.
        if let Some(cause) = self.pending_interrupt() {
            return self.handle_interrupt(cause);
        }

        if pc & 0b11 != 0 {
            return self.handle_trap(Trap::InstructionAddressMisaligned(pc));
        }

        let raw = mem
            .fetch_u32(pc)
            .map_err(|fault| Trap::InstructionAccessFault(fault.addr))?;
        let inst = isa::decode(raw);
        let outcome = match self.execute(inst, mem) {
            Ok(outcome) => outcome,
            Err(trap) => return self.handle_trap(trap),
        };
        // An instruction that wrote `minstret` does not count itself: the value
        // it wrote is what the next reader must see, not one more than that.
        if outcome == StepOutcome::Continue && !self.instret_written {
            self.instret = self.instret.wrapping_add(1);
        }
        Ok(outcome)
    }

    /// The cause of the interrupt that should be taken now, if any.
    ///
    /// An interrupt is taken when it is pending in `mip`, enabled in `mie`, and
    /// the global enable `mstatus.MIE` is set. When several qualify, the highest
    /// numbered wins, which is the platform's fixed priority: MEI (11) over MTI
    /// (7) over MSI (3).
    fn pending_interrupt(&self) -> Option<u32> {
        let enabled = self.mip & self.csr.read(csr_addr::MIE);
        if enabled == 0 || self.csr.read(csr_addr::MSTATUS) & MSTATUS_MIE == 0 {
            return None;
        }
        // The highest set bit of `enabled`, as the bit's own value: for an
        // interrupt, `mcause` *is* the bit number, not an index.
        Some(1u32 << (31 - enabled.leading_zeros()))
    }

    /// Take an interrupt, entering the machine-mode handler.
    ///
    /// Differs from a synchronous exception in two ways that matter: `mcause` is
    /// the interrupt bit rather than a reason code, and `mtval` is zero because
    /// an interrupt has no faulting address. In vectored mode the entry point is
    /// `mtvec` base + 4 * cause, which is the one case where vectoring applies.
    fn handle_interrupt(&mut self, cause: u32) -> Result<StepOutcome, Trap> {
        self.csr.write(csr_addr::MEPC, self.pc);
        self.csr.write(csr_addr::MCAUSE, cause);
        self.csr.write(csr_addr::MTVAL, 0);
        self.push_interrupt_enable();

        let mtvec = self.csr.read(csr_addr::MTVEC);
        // Bit 0 of mtvec selects the mode: 1 is vectored, and only interrupts
        // are ever vectored.
        let entry = if mtvec & 0b11 == 1 {
            mtvec.wrapping_add(4 * cause)
        } else {
            mtvec
        };
        self.pc = entry & !0b11;
        Ok(StepOutcome::TrapTaken)
    }

    /// `MPIE <- MIE; MIE <- 0`, the interrupt-enable half of trap entry.
    fn push_interrupt_enable(&mut self) {
        let mstatus = self.csr.read(csr_addr::MSTATUS);
        let mut next = mstatus & !(MSTATUS_MIE | MSTATUS_MPIE);
        if mstatus & MSTATUS_MIE != 0 {
            next |= MSTATUS_MPIE;
        }
        self.csr.write(csr_addr::MSTATUS, next);
    }

    /// Deliver a trap, or report it when there is nowhere to deliver it.
    fn handle_trap(&mut self, trap: Trap) -> Result<StepOutcome, Trap> {
        if !self.handler_installed() {
            // No handler: `ecall`/`ebreak` end the run loop so a bare-metal
            // image can halt itself, and every other exception is the caller's
            // problem.
            return match trap {
                Trap::EnvironmentCallFromM(_) => Ok(StepOutcome::Ecall),
                Trap::Breakpoint(_) => Ok(StepOutcome::Ebreak),
                other => Err(other),
            };
        }

        self.csr.write(csr_addr::MEPC, self.pc);
        self.csr.write(csr_addr::MCAUSE, trap.mcause());
        self.csr.write(csr_addr::MTVAL, trap.mtval());
        self.push_interrupt_enable();

        // A synchronous exception enters at BASE even in vectored mode.
        self.pc = self.csr.read(csr_addr::MTVEC) & !0b11;
        Ok(StepOutcome::TrapTaken)
    }

    fn execute(&mut self, inst: Decoded, mem: &mut Memory) -> Result<StepOutcome, Trap> {
        let pc = self.pc;
        let Decoded {
            raw,
            opcode,
            rd,
            rs1,
            rs2,
            funct3,
            funct7,
            funct12: _,
            imm,
        } = inst;

        match opcode {
            isa::opcode::OP_IMM => {
                let a = self.x(rs1);
                let val = match funct3 {
                    0b000 => a.wrapping_add(imm as u32),               // addi
                    0b001 if funct7 == 0 => a << ((raw >> 20) & 0x1f), // slli
                    0b010 => ((a as i32) < imm) as u32,                // slti
                    0b011 => (a < imm as u32) as u32,                  // sltiu
                    0b100 => a ^ imm as u32,                           // xori
                    0b101 if funct7 == 0 || funct7 == isa::ALT_FUNCT7 => {
                        let shamt = (raw >> 20) & 0x1f;
                        if funct7 == isa::ALT_FUNCT7 {
                            ((a as i32) >> shamt) as u32 // srai
                        } else {
                            a >> shamt // srli
                        }
                    }
                    0b110 => a | imm as u32, // ori
                    0b111 => a & imm as u32, // andi
                    _ => return Err(Trap::IllegalInstruction(raw)),
                };
                self.write_rd(rd, val);
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::OP => {
                let a = self.x(rs1);
                let b = self.x(rs2);
                let val = if funct7 == isa::M_FUNCT7 {
                    muldiv(a, b, funct3)
                } else {
                    match (funct7, funct3) {
                        (0, 0b000) => a.wrapping_add(b),
                        (isa::ALT_FUNCT7, 0b000) => a.wrapping_sub(b),
                        (0, 0b001) => a << (b & 0x1f),
                        (0, 0b010) => ((a as i32) < (b as i32)) as u32,
                        (0, 0b011) => (a < b) as u32,
                        (0, 0b100) => a ^ b,
                        (0, 0b101) => a >> (b & 0x1f),
                        (isa::ALT_FUNCT7, 0b101) => ((a as i32) >> (b & 0x1f)) as u32,
                        (0, 0b110) => a | b,
                        (0, 0b111) => a & b,
                        _ => return Err(Trap::IllegalInstruction(raw)),
                    }
                };
                self.write_rd(rd, val);
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::LOAD => {
                let addr = self.x(rs1).wrapping_add(imm as u32);
                let load = |m: &Memory, a: u32, n: u32| -> Result<u64, Trap> {
                    m.load(a, n).map_err(|f| Trap::LoadAccessFault(f.addr))
                };
                let val = match funct3 {
                    isa::funct3::LB => load(mem, addr, 1)? as u8 as i8 as i32 as u32,
                    isa::funct3::LH => load(mem, addr, 2)? as u16 as i16 as i32 as u32,
                    isa::funct3::LW => load(mem, addr, 4)? as u32,
                    isa::funct3::LBU => load(mem, addr, 1)? as u32,
                    isa::funct3::LHU => load(mem, addr, 2)? as u32,
                    _ => return Err(Trap::IllegalInstruction(raw)),
                };
                self.write_rd(rd, val);
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::STORE => {
                let addr = self.x(rs1).wrapping_add(imm as u32);
                let val = self.x(rs2);
                let store = |m: &mut Memory, a: u32, n: u32, v: u64| -> Result<(), Trap> {
                    m.store(a, n, v).map_err(|f| Trap::StoreAccessFault(f.addr))
                };
                match funct3 {
                    isa::funct3::SB => store(mem, addr, 1, val as u64)?,
                    isa::funct3::SH => store(mem, addr, 2, val as u64)?,
                    isa::funct3::SW => store(mem, addr, 4, val as u64)?,
                    _ => return Err(Trap::IllegalInstruction(raw)),
                }
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::BRANCH => {
                let a = self.x(rs1);
                let b = self.x(rs2);
                let taken = match funct3 {
                    isa::funct3::BEQ => a == b,
                    isa::funct3::BNE => a != b,
                    isa::funct3::BLT => (a as i32) < (b as i32),
                    isa::funct3::BGE => (a as i32) >= (b as i32),
                    isa::funct3::BLTU => a < b,
                    isa::funct3::BGEU => a >= b,
                    _ => return Err(Trap::IllegalInstruction(raw)),
                };
                self.pc = if taken {
                    let target = pc.wrapping_add(imm as u32);
                    require_instruction_alignment(target)?;
                    target
                } else {
                    pc.wrapping_add(4)
                };
                Ok(StepOutcome::Continue)
            }

            isa::opcode::JAL => {
                let target = pc.wrapping_add(imm as u32);
                require_instruction_alignment(target)?;
                self.write_rd(rd, pc.wrapping_add(4));
                self.pc = target;
                Ok(StepOutcome::Continue)
            }

            isa::opcode::JALR => {
                if funct3 != 0 {
                    return Err(Trap::IllegalInstruction(raw));
                }
                let target = self.x(rs1).wrapping_add(imm as u32) & !1;
                require_instruction_alignment(target)?;
                self.write_rd(rd, pc.wrapping_add(4));
                self.pc = target;
                Ok(StepOutcome::Continue)
            }

            isa::opcode::LUI => {
                self.write_rd(rd, imm as u32);
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::AUIPC => {
                self.write_rd(rd, pc.wrapping_add(imm as u32));
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::MISC_MEM => {
                // FENCE / FENCE.I: no memory-order or I-cache side effect for a
                // single-hart integer functional model.
                match funct3 {
                    0b000 if valid_fence_encoding(raw) => {
                        self.pc = pc.wrapping_add(4);
                        Ok(StepOutcome::Continue)
                    }
                    0b001 if raw == 0x0000_100f => {
                        self.pc = pc.wrapping_add(4);
                        Ok(StepOutcome::Continue)
                    }
                    _ => Err(Trap::IllegalInstruction(raw)),
                }
            }

            isa::opcode::SYSTEM => {
                if funct3 == 0 {
                    self.execute_system(&inst, pc)
                } else {
                    self.execute_csr(&inst, pc)
                }
            }

            // Recognized encodings of extensions this model does not implement.
            // Architecturally these are illegal instructions (mcause 2); naming
            // the extension makes a failing run diagnosable.
            isa::opcode::LOAD_FP | isa::opcode::STORE_FP => Err(Trap::Unsupported("F/D")),
            isa::opcode::AMO => Err(Trap::Unsupported("A")),
            isa::opcode::OP_FP => Err(Trap::Unsupported("F/D")),
            isa::opcode::OP_V => Err(Trap::Unsupported("V")),
            // A 16-bit compressed instruction: its low two bits are never 0b11,
            // so it can only appear under one of these three opcodes.
            0b000..=0b010 => Err(Trap::Unsupported("C")),

            _ => Err(Trap::IllegalInstruction(raw)),
        }
    }

    /// Execute a `funct3 = 0` SYSTEM instruction: `ecall`, `ebreak`, `mret`,
    /// `wfi`, or an illegal encoding such as `sret`/`sfence.vma`.
    fn execute_system(&mut self, inst: &Decoded, pc: u32) -> Result<StepOutcome, Trap> {
        // Everything this model implements requires rd = rs1 = 0; other values
        // in those fields are reserved encodings.
        let no_operands = inst.rd == 0 && inst.rs1 == 0;

        match inst.funct12 {
            isa::system::ECALL if no_operands => Err(Trap::EnvironmentCallFromM(pc)),
            isa::system::EBREAK if no_operands => Err(Trap::Breakpoint(pc)),
            isa::system::MRET if no_operands => self.execute_mret(),
            // WFI is legal in machine mode and is only a hint: it may complete
            // immediately. Retiring it is not a shortcut past an interrupt,
            // because the interrupt check happens at the start of the next step,
            // so a pending enabled interrupt is still taken before whatever
            // follows. An interrupt that is pending but *disabled* correctly
            // does not disturb the wait.
            isa::system::WFI if no_operands => {
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }
            // SRET (0x102) and SFENCE.VMA (0x120…) need S-mode and virtual
            // memory, and every other encoding here is reserved.
            _ => Err(Trap::IllegalInstruction(inst.raw)),
        }
    }

    /// `mret`: restore the interrupt-enable stack and return to `mepc`.
    fn execute_mret(&mut self) -> Result<StepOutcome, Trap> {
        let mstatus = self.csr.read(csr_addr::MSTATUS);
        let mut next = mstatus & !(MSTATUS_MIE | MSTATUS_MPIE);
        if mstatus & MSTATUS_MPIE != 0 {
            next |= MSTATUS_MIE; // MIE <- MPIE
        }
        next |= MSTATUS_MPIE; // MPIE <- 1
                              // MPP is hardwired to M, so `mret` always returns to machine mode.
        self.csr.write(csr_addr::MSTATUS, next);

        self.pc = self.csr.read(csr_addr::MEPC);
        Ok(StepOutcome::Continue)
    }

    /// Execute one of the six Zicsr read/modify/write instructions.
    fn execute_csr(&mut self, inst: &Decoded, pc: u32) -> Result<StepOutcome, Trap> {
        let address = inst.funct12;
        if !Csr::exists(address) {
            return Err(Trap::IllegalInstruction(inst.raw));
        }

        let immediate = inst.funct3 & 0b100 != 0;
        let source = if immediate {
            inst.rs1 // The rs1 field encodes the five-bit immediate (zimm).
        } else {
            self.x(inst.rs1)
        };

        // CSRRW/CSRRWI with rd = x0 must not read the CSR: a read may have side
        // effects, and the architecture forbids them here. Everything else needs
        // the old value.
        let reads = !(inst.funct3 & 0b011 == 0b001 && inst.rd == 0);
        let old = if reads { self.read_csr(address) } else { 0 };

        // A write happens for CSRRW/CSRRWI always, and for CSRRS/CSRRC only when
        // the source field is non-zero (a pure read).
        let write = match inst.funct3 & 0b011 {
            0b001 => Some(source),                         // csrrw(i)
            0b010 if inst.rs1 != 0 => Some(old | source),  // csrrs(i)
            0b011 if inst.rs1 != 0 => Some(old & !source), // csrrc(i)
            0b010 | 0b011 => None,                         // rs1 = x0: read only, never a write
            _ => return Err(Trap::IllegalInstruction(inst.raw)),
        };

        if let Some(value) = write {
            // csr[11:10] = 0b11 denotes a read-only CSR; writing one is illegal.
            if Csr::is_read_only(address) {
                return Err(Trap::IllegalInstruction(inst.raw));
            }
            self.write_csr(address, value);
        }

        self.write_rd(inst.rd, old);
        self.pc = pc.wrapping_add(4);
        Ok(StepOutcome::Continue)
    }

    /// Read a CSR, servicing the counter and time registers from CPU state.
    fn read_csr(&self, address: u32) -> u32 {
        match address {
            csr_addr::MCYCLE | csr_addr::CYCLE => self.cycle as u32,
            csr_addr::MCYCLEH | csr_addr::CYCLEH => (self.cycle >> 32) as u32,
            csr_addr::MINSTRET | csr_addr::INSTRET => self.instret as u32,
            csr_addr::MINSTRETH | csr_addr::INSTRETH => (self.instret >> 32) as u32,
            csr_addr::TIME => self.mtime as u32,
            csr_addr::TIMEH => (self.mtime >> 32) as u32,
            // Driven by the interrupt controller, not stored.
            csr_addr::MIP => self.mip,
            _ => self.csr.read(address),
        }
    }

    /// Write a CSR, servicing the writable machine counters from CPU state.
    /// The read-only unprivileged aliases never reach here: `execute_csr`
    /// rejects them first.
    fn write_csr(&mut self, address: u32, value: u32) {
        match address {
            csr_addr::MCYCLE => self.cycle = (self.cycle & !0xFFFF_FFFF) | value as u64,
            csr_addr::MCYCLEH => self.cycle = (self.cycle & 0xFFFF_FFFF) | ((value as u64) << 32),
            csr_addr::MINSTRET => {
                self.instret = (self.instret & !0xFFFF_FFFF) | value as u64;
                self.instret_written = true;
            }
            csr_addr::MINSTRETH => {
                self.instret = (self.instret & 0xFFFF_FFFF) | ((value as u64) << 32);
                self.instret_written = true;
            }
            _ => self.csr.write(address, value),
        }
    }
}

/// With IALIGN = 32 an instruction address must be four-byte aligned.
///
/// This is architecturally fixed, not implementation-defined, so unlike misaligned
/// *data* accesses it always traps. With the `C` extension IALIGN drops to 16 and
/// this rule changes with it.
#[inline]
fn require_instruction_alignment(addr: u32) -> Result<(), Trap> {
    if addr & 0b11 == 0 {
        Ok(())
    } else {
        Err(Trap::InstructionAddressMisaligned(addr))
    }
}

#[inline]
fn valid_fence_encoding(raw: u32) -> bool {
    let fm = (raw >> 28) & 0xf;
    let predecessor = (raw >> 24) & 0xf;
    let successor = (raw >> 20) & 0xf;
    fm == 0 || (fm == 8 && predecessor == 0b0011 && successor == 0b0011)
}

/// Execute the M-extension multiply/divide instructions.
fn muldiv(a: u32, b: u32, funct3: u32) -> u32 {
    match funct3 {
        0b000 => a.wrapping_mul(b), // mul
        0b001 => {
            let p = (a as i32 as i64) * (b as i32 as i64);
            ((p >> 32) as i32) as u32 // mulh
        }
        0b010 => {
            let p = (a as i32 as i64) * (b as i64);
            ((p >> 32) as i32) as u32 // mulhsu
        }
        0b011 => (((a as u64) * (b as u64)) >> 32) as u32, // mulhu
        0b100 => signed_div(a, b),                         // div
        0b101 => a.checked_div(b).unwrap_or(u32::MAX),     // divu by zero → all ones
        0b110 => signed_rem(a, b),                         // rem
        0b111 => a.checked_rem(b).unwrap_or(a),            // remu by zero → dividend
        _ => a, // unreachable: opcode dispatch guarantees M_FUNCT7
    }
}

/// Signed division with RISC-V semantics: div by zero yields -1, and the
/// signed-minimum / -1 overflow case yields the dividend (no trap).
fn signed_div(a: u32, b: u32) -> u32 {
    let sa = a as i32;
    let sb = b as i32;
    if sb == 0 {
        u32::MAX // -1
    } else {
        sa.wrapping_div(sb) as u32
    }
}

/// Signed remainder with RISC-V semantics: rem by zero yields the dividend,
/// and the signed-minimum / -1 overflow case yields 0.
fn signed_rem(a: u32, b: u32) -> u32 {
    let sa = a as i32;
    let sb = b as i32;
    if sb == 0 {
        a
    } else {
        sa.wrapping_rem(sb) as u32
    }
}
