//! CPU state and the RV32IMZicsr execute loop.
//!
//! `Cpu` owns the program counter, the integer register file, a CSR container,
//! the cycle/retired-instruction counters, and a model time base. `step`
//! fetches, decodes, and executes one instruction against a `Memory`.
//!
//! # Trap handling
//!
//! Traps from S/U route to S-mode when delegated, otherwise to M-mode. Traps
//! originating in M never delegate. Entry saves EPC/cause/tval and the target
//! mode's interrupt-enable stack; xRET restores it and clears xPP to U.
//! Interrupts use the architectural high-bit cause encoding and vector by the
//! cause number. A zero target tvec keeps the bare-metal halt/error convention
//! for synchronous traps. WFI completes immediately as a hint.

use crate::csr::{
    addr as csr_addr, Csr, MSTATUS_MIE, MSTATUS_MPIE, MSTATUS_MPP, MSTATUS_MPP_SHIFT, MSTATUS_SIE,
    MSTATUS_SPIE, MSTATUS_SPP, MSTATUS_TSR,
};
use crate::isa::{self, Decoded, Privilege};
use crate::mem::Memory;
use crate::trace::{Change, Record, StepResult, Trace};

/// Outcome of executing a single instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Instruction retired normally; PC already advanced.
    Continue,
    /// A trap was taken: the PC is now at the selected M/S handler and the trap CSRs
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
    /// `ecall` was executed. Payload: the mode it was made from and its PC.
    ///
    /// The mode is what determines the cause: an `ecall` from U is cause 8, from
    /// S cause 9, and from M cause 11.
    EnvironmentCall { mode: Privilege, pc: u32 },
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
            // Environment call: 8 from U, 9 from S, 11 from M. The offset is
            // fixed by the architecture, so it is the mode's encoding that
            // shifts, not an enumeration index.
            Trap::EnvironmentCall { mode, .. } => 8 + mode.encoding(),
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
            Trap::EnvironmentCall { .. } | Trap::Unsupported(_) => 0,
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
    /// The privilege the hart is currently running at. Reset is Machine, and the
    /// only way down is `mret` returning to whatever `MPP` holds; the only way up
    /// is a trap.
    privilege: Privilege,
    /// Where to send a per-instruction trace, if anywhere.
    trace: Option<Box<dyn Trace>>,
    /// Changes made by the step in progress, drained into a [`Record`] when the
    /// step ends. Collecting them here rather than diffing snapshots afterwards
    /// means a register written with the value it already held still shows up,
    /// which is what a hardware comparison needs.
    changes: Vec<Change>,
    /// How many steps have been started. Used as the trace's step number.
    step_count: u64,
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
            privilege: Privilege::Machine,
            trace: None,
            changes: Vec::new(),
            step_count: 0,
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
    /// Follows CLINT when present, otherwise advances once per step.
    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    /// Set the fallback model time base (a mapped CLINT overrides it).
    pub fn set_mtime(&mut self, mtime: u64) {
        self.mtime = mtime;
    }

    /// Is a trap handler installed? The model treats a non-zero `mtvec` as the
    /// presence of one.
    pub fn handler_installed(&self) -> bool {
        self.csr.read(csr_addr::MTVEC) != 0
    }

    /// The privilege the hart is currently running at.
    pub fn privilege(&self) -> Privilege {
        self.privilege
    }

    /// Send a per-instruction trace to `sink`, or stop tracing with `None`.
    ///
    /// Tracing costs a `Vec` push per state change, so it is off by default: a
    /// run that nobody is comparing against should not pay for it.
    pub fn set_trace(&mut self, sink: Option<Box<dyn Trace>>) {
        self.trace = sink;
    }

    /// Is a trace attached?
    pub fn is_tracing(&self) -> bool {
        self.trace.is_some()
    }

    /// Run at `privilege` from the current PC.
    ///
    /// This is how a test or a bootloader enters a lower mode directly, and how
    /// a trap handler resumes user code. It does not touch `MPP`/`SPP`: those
    /// describe what a *return* instruction will go back to, and setting them
    /// here would let a caller fake a return it never made.
    pub fn set_privilege(&mut self, privilege: Privilege) {
        self.privilege = privilege;
    }

    #[inline]
    fn x(&self, rs: u32) -> u32 {
        self.x[(rs as usize) & (isa::NUM_REGS - 1)]
    }

    /// Write `rd`; writes to `x0` are discarded (it is hard-wired to zero).
    #[inline]
    fn write_rd(&mut self, rd: u32, val: u32) {
        if rd != 0 {
            let index = (rd as usize) & (isa::NUM_REGS - 1);
            self.x[index] = val;
            self.changes.push(Change::Register {
                index: rd & (isa::NUM_REGS as u32 - 1),
                value: val,
            });
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
    ///
    /// Every exit path produces exactly one trace record. The steps that can
    /// fail before the instruction is even fetched — a misaligned or unmapped
    /// fetch, or an interrupt — are reported with the instruction word they did
    /// not get, because "the hardware jumped somewhere else" is only diagnosable
    /// if the trace says where it expected to be.
    pub fn step(&mut self, mem: &mut Memory) -> Result<StepOutcome, Trap> {
        let pc = self.pc;
        self.cycle = self.cycle.wrapping_add(1);
        self.instret_written = false;
        self.changes.clear();
        self.step_count += 1;

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
            let result = self.handle_interrupt(cause);
            return self.finish_step(result, pc, 0);
        }

        if pc & 0b11 != 0 {
            let result = self.handle_trap(Trap::InstructionAddressMisaligned(pc));
            return self.finish_step(result, pc, 0);
        }

        let raw = match mem.fetch_u32(pc) {
            Ok(raw) => raw,
            Err(fault) => {
                let result = self.handle_trap(Trap::InstructionAccessFault(fault.addr));
                return self.finish_step(result, pc, 0);
            }
        };
        let inst = isa::decode(raw);
        // Sequenced rather than nested: `handle_trap` needs `&mut self` and
        // `execute` borrows it, so the two cannot overlap.
        let executed = self.execute(inst, mem);
        let outcome = match executed {
            Ok(outcome) => {
                // An instruction that wrote `minstret` does not count itself:
                // the value it wrote is what the next reader must see, not one
                // more than that.
                if outcome == StepOutcome::Continue
                    && !self.instret_written
                    && !self.instret_inhibited()
                {
                    self.instret = self.instret.wrapping_add(1);
                }
                Ok(outcome)
            }
            // An exception is delivered, not returned: the point of the handler
            // is that a bare-metal program can react to it. A trap with nowhere
            // to go still comes back as Err, and `finish_step` records it before
            // the caller sees it.
            Err(trap) => self.handle_trap(trap),
        };
        self.finish_step(outcome, pc, raw)
    }

    /// Emit the trace record for the step that just ended and return the
    /// instruction's own result unchanged.
    fn finish_step(
        &mut self,
        outcome: Result<StepOutcome, Trap>,
        pc: u32,
        instruction: u32,
    ) -> Result<StepOutcome, Trap> {
        if self.trace.is_none() {
            // Skip building the record entirely when nobody is watching, so an
            // untraced run pays nothing for the change vector.
            self.changes.clear();
            return outcome;
        }

        let step = self.step_count;
        let result = match &outcome {
            Ok(StepOutcome::Continue) => StepResult::Retired,
            // A taken trap is not a retirement, but the run continues.
            Ok(StepOutcome::TrapTaken) => {
                let supervisor = self.privilege == Privilege::Supervisor;
                StepResult::Trap {
                    cause: self.csr.read(if supervisor {
                        csr_addr::SCAUSE
                    } else {
                        csr_addr::MCAUSE
                    }),
                    tval: self.csr.read(if supervisor {
                        csr_addr::STVAL
                    } else {
                        csr_addr::MTVAL
                    }),
                }
            }
            // ecall/ebreak with no handler: the run stops here.
            Ok(_) => StepResult::Halt,
            // A trap that had nowhere to go is reported to the caller. It never
            // reached a handler, so there are no trap CSRs to report; the cause
            // the caller already has is the only useful thing.
            Err(trap) => StepResult::Trap {
                cause: trap.mcause(),
                tval: trap.mtval(),
            },
        };

        let record = Record {
            step,
            pc,
            instruction,
            outcome: result,
            changes: std::mem::take(&mut self.changes),
        };
        if let Some(sink) = self.trace.as_mut() {
            sink.record(&record);
        }
        outcome
    }

    /// Is the instruction counter inhibited by `mcountinhibit.IR`?
    ///
    /// The other inhibit bit, `CY`, applies to `cycle`; the model does not
    /// implement it, because `cycle` here is a step counter rather than a clock
    /// and gating it would make the two tools' counters incomparable for no
    /// architectural gain.
    fn instret_inhibited(&self) -> bool {
        self.csr.read(csr_addr::MCOUNTINHIBIT) & (1 << 2) != 0
    }

    /// Select the highest-priority eligible interrupt. Machine-targeted
    /// interrupts preempt supervisor-targeted ones regardless of their number.
    fn pending_interrupt(&self) -> Option<u32> {
        let pending = (self.mip | self.csr.read(csr_addr::MIP)) & self.csr.read(csr_addr::MIE);
        let delegated = self.csr.read(csr_addr::MIDELEG);
        let status = self.csr.read(csr_addr::MSTATUS);
        let machine_enabled = self.privilege < Privilege::Machine || status & MSTATUS_MIE != 0;
        let supervisor_enabled = self.privilege < Privilege::Supervisor
            || (self.privilege == Privilege::Supervisor && status & MSTATUS_SIE != 0);
        let eligible = (if machine_enabled {
            pending & !delegated
        } else {
            0
        }) | (if supervisor_enabled {
            pending & delegated
        } else {
            0
        });
        // Privileged ISA fixed priority: MEI, MSI, MTI, SEI, SSI, STI.
        [11, 3, 7, 9, 1, 5]
            .into_iter()
            .find(|cause| eligible & !delegated & (1 << cause) != 0)
            .or_else(|| {
                [9, 1, 5]
                    .into_iter()
                    .find(|cause| eligible & delegated & (1 << cause) != 0)
            })
    }

    fn handle_interrupt(&mut self, cause: u32) -> Result<StepOutcome, Trap> {
        let supervisor = self.csr.read(csr_addr::MIDELEG) & (1 << cause) != 0;
        self.enter_trap(supervisor, (1 << 31) | cause, 0, true);
        Ok(StepOutcome::TrapTaken)
    }

    /// Deliver synchronous traps to their delegated target. A missing target
    /// handler retains the driver's explicit halt/error convention.
    fn handle_trap(&mut self, trap: Trap) -> Result<StepOutcome, Trap> {
        let supervisor = self.privilege != Privilege::Machine
            && self.csr.read(csr_addr::MEDELEG) & (1 << trap.mcause()) != 0;
        let vector = if supervisor {
            csr_addr::STVEC
        } else {
            csr_addr::MTVEC
        };
        if self.csr.read(vector) == 0 {
            return match trap {
                Trap::EnvironmentCall { .. } => Ok(StepOutcome::Ecall),
                Trap::Breakpoint(_) => Ok(StepOutcome::Ebreak),
                other => Err(other),
            };
        }
        self.enter_trap(supervisor, trap.mcause(), trap.mtval(), false);
        Ok(StepOutcome::TrapTaken)
    }

    /// Save only the target privilege's trap state and enable stack. All writes
    /// pass through the trace collector, including entry into S-mode.
    fn enter_trap(&mut self, supervisor: bool, cause: u32, tval: u32, interrupt: bool) {
        let (epc, cause_csr, tval_csr, vector, enable, saved, previous) = if supervisor {
            (
                csr_addr::SEPC,
                csr_addr::SCAUSE,
                csr_addr::STVAL,
                csr_addr::STVEC,
                MSTATUS_SIE,
                MSTATUS_SPIE,
                MSTATUS_SPP,
            )
        } else {
            (
                csr_addr::MEPC,
                csr_addr::MCAUSE,
                csr_addr::MTVAL,
                csr_addr::MTVEC,
                MSTATUS_MIE,
                MSTATUS_MPIE,
                MSTATUS_MPP,
            )
        };
        self.write_csr(epc, self.pc);
        self.write_csr(cause_csr, cause);
        self.write_csr(tval_csr, tval);
        let status = self.csr.read(csr_addr::MSTATUS);
        let mut next = status & !(enable | saved | previous);
        if status & enable != 0 {
            next |= saved;
        }
        if supervisor {
            if self.privilege == Privilege::Supervisor {
                next |= MSTATUS_SPP;
            }
        } else {
            next |= self.privilege.encoding() << MSTATUS_MPP_SHIFT;
        }
        self.write_csr(csr_addr::MSTATUS, next);
        self.privilege = if supervisor {
            Privilege::Supervisor
        } else {
            Privilege::Machine
        };
        let tvec = self.csr.read(vector);
        self.pc = (tvec & !0b11).wrapping_add(if interrupt && tvec & 0b11 == 1 {
            4 * (cause & !(1 << 31))
        } else {
            0
        });
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
                let width = match funct3 {
                    isa::funct3::SB => 1,
                    isa::funct3::SH => 2,
                    isa::funct3::SW => 4,
                    _ => return Err(Trap::IllegalInstruction(raw)),
                };
                store(mem, addr, width, val as u64)?;
                // Recorded only after the store succeeded, so a faulting store
                // never appears in a trace as though it had written.
                self.changes.push(Change::Memory {
                    address: addr,
                    len: width,
                    value: u64::from(val),
                });
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
    /// `sret`, `sfence.vma`, or `wfi`.
    fn execute_system(&mut self, inst: &Decoded, pc: u32) -> Result<StepOutcome, Trap> {
        // Everything this model implements requires rd = rs1 = 0; other values
        // in those fields are reserved encodings.
        let no_operands = inst.rd == 0 && inst.rs1 == 0;

        match inst.funct12 {
            // The cause depends on where the call was made from, so `ecall` is
            // the one instruction whose identity depends on machine state.
            isa::system::ECALL if no_operands => Err(Trap::EnvironmentCall {
                mode: self.privilege,
                pc,
            }),
            isa::system::EBREAK if no_operands => Err(Trap::Breakpoint(pc)),
            // MRET is legal only at M and resumes the saved MPP privilege.
            isa::system::MRET if no_operands && self.privilege == Privilege::Machine => {
                self.execute_return(Privilege::Machine)
            }
            // SRET is legal at S and above. It returns to SPP, which cannot
            // express M, so S is the highest privilege it can return to.
            isa::system::SRET
                if no_operands
                    && self.privilege >= Privilege::Supervisor
                    && (self.privilege == Privilege::Machine
                        || self.csr.read(csr_addr::MSTATUS) & MSTATUS_TSR == 0) =>
            {
                self.execute_return(Privilege::Supervisor)
            }
            // WFI is legal in every mode and is only a hint: it may complete
            // immediately. Retiring it is not a shortcut past an interrupt,
            // because the interrupt check happens at the start of the next step,
            // so a pending enabled interrupt is still taken before whatever
            // follows. An interrupt that is pending but *disabled* correctly
            // does not disturb the wait.
            isa::system::WFI if no_operands => {
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }
            // Bare-only implementation: SFENCE.VMA is unsupported in every
            // mode, SUM/MXR/TVM are read-only zero, and satp reads zero. This
            // matches the Bare path of riscv-tests' illegal-instruction test.
            _ => Err(Trap::IllegalInstruction(inst.raw)),
        }
    }

    /// `mret` (from M) or `sret` (from S): restore the privilege and the
    /// interrupt-enable stack, and jump to the matching EPC.
    ///
    /// The two differ only in which fields they read, which is why they share an
    /// implementation: `level` selects `MPP`/`MIE`/`MPIE`/`mepc` or
    /// `SPP`/`SIE`/`SPIE`/`sepc`.
    fn execute_return(&mut self, level: Privilege) -> Result<StepOutcome, Trap> {
        let machine = level == Privilege::Machine;
        let mstatus = self.csr.read(csr_addr::MSTATUS);

        // The enable bit of the mode being returned to, and the saved enable bit
        // of the mode being left.
        let (enable, saved) = if machine {
            (MSTATUS_MIE, MSTATUS_MPIE)
        } else {
            (MSTATUS_SIE, MSTATUS_SPIE)
        };
        let mut next = mstatus & !(enable | saved);
        if mstatus & saved != 0 {
            next |= enable; // xIE <- xPIE
        }
        next |= saved; // xPIE <- 1

        // Where to go back to. MPP is two bits and can name any mode; SPP is one
        // bit and names U or S, never M.
        let target = if machine {
            Privilege::from_encoding((mstatus >> MSTATUS_MPP_SHIFT) & 0b11)
        } else if mstatus & MSTATUS_SPP != 0 {
            Some(Privilege::Supervisor)
        } else {
            Some(Privilege::User)
        }
        .unwrap_or(Privilege::User);

        // Reset the saved privilege to the least privileged supported mode.
        next &= if machine { !MSTATUS_MPP } else { !MSTATUS_SPP };
        self.write_csr(csr_addr::MSTATUS, next);

        self.privilege = target;
        self.pc = if machine {
            self.csr.read(csr_addr::MEPC)
        } else {
            self.csr.read(csr_addr::SEPC)
        };
        Ok(StepOutcome::Continue)
    }

    /// Execute one of the six Zicsr read/modify/write instructions.
    fn execute_csr(&mut self, inst: &Decoded, pc: u32) -> Result<StepOutcome, Trap> {
        let address = inst.funct12;
        if !Csr::exists(address) {
            return Err(Trap::IllegalInstruction(inst.raw));
        }
        // A register above the current privilege is invisible, not merely
        // unwritable: reading `mstatus` from U mode is an illegal instruction
        // rather than a read of somebody else's state.
        if !self.privilege.can_access(Csr::required_privilege(address)) {
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
        // Device interrupt signals contribute to the read result, but must
        // never get latched into the software-injected pending bits by RMW.
        let write_old = if matches!(address, csr_addr::MIP | csr_addr::SIP) {
            self.csr.read(address)
        } else {
            old
        };
        let write = match inst.funct3 & 0b011 {
            0b001 => Some(source),                               // csrrw(i)
            0b010 if inst.rs1 != 0 => Some(write_old | source),  // csrrs(i)
            0b011 if inst.rs1 != 0 => Some(write_old & !source), // csrrc(i)
            0b010 | 0b011 => None, // rs1 = x0: read only, never a write
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
            csr_addr::MIP => self.mip | self.csr.read(csr_addr::MIP),
            csr_addr::SIP => {
                (self.mip | self.csr.read(csr_addr::MIP)) & self.csr.read(csr_addr::MIDELEG)
            }
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
            _ => {
                self.csr.write(address, value);
            }
        }
        // Recorded after the write so the value is what the register actually
        // holds, i.e. after WARL legalization.
        if !matches!(
            address,
            csr_addr::MCYCLE | csr_addr::MCYCLEH | csr_addr::MINSTRET | csr_addr::MINSTRETH
        ) {
            self.changes.push(Change::Csr {
                address,
                value: self.csr.read(address),
            });
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
