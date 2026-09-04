//! CPU state and the RV32IMZicsr execute loop.
//!
//! `Cpu` owns the program counter, the integer register file, and a CSR
//! container. `step` fetches, decodes, and executes one instruction against
//! a `Memory`. Extensions outside the implemented RV32IM base raise
//! [`Trap`] rather than fabricating a result.

use crate::csr::Csr;
use crate::isa::{self, Decoded};
use crate::mem::Memory;

/// Outcome of executing a single instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Instruction retired normally; PC already advanced.
    Continue,
    /// `ecall` was executed — the program requested an environment call.
    Ecall,
    /// `ebreak` was executed — the program halted (debug breakpoint).
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

/// A trap raised while executing an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    /// The instruction at the given program counter is not a valid RV32IMZicsr
    /// instruction.
    IllegalInstruction(u32),
    /// A recognized-but-unimplemented extension (atomics, FP, etc.).
    Unsupported(&'static str),
}

/// A RISC-V CPU with RV32IM integer execution.
pub struct Cpu {
    pc: u32,
    x: [u32; 32],
    csr: Csr,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Create a fresh CPU: PC = 0, all registers zero, empty CSR file.
    pub fn new() -> Self {
        Cpu {
            pc: 0,
            x: [0; 32],
            csr: Csr::new(),
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
    /// budget is exhausted.
    pub fn run(&mut self, mem: &mut Memory, budget: u64) -> Result<StopReason, Trap> {
        for _ in 0..budget {
            match self.step(mem)? {
                StepOutcome::Continue => {}
                StepOutcome::Ecall => return Ok(StopReason::Ecall),
                StepOutcome::Ebreak => return Ok(StopReason::Ebreak),
            }
        }
        Ok(StopReason::Limit)
    }

    /// Fetch, decode, and execute a single instruction.
    pub fn step(&mut self, mem: &mut Memory) -> Result<StepOutcome, Trap> {
        let pc = self.pc;
        let raw = mem.load_u32(pc);
        let inst = isa::decode(pc, raw);
        self.execute(inst, mem)
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
            imm,
            pc: _,
        } = inst;

        match opcode {
            isa::opcode::OP_IMM => {
                let a = self.x(rs1);
                let val = match funct3 {
                    0b000 => a.wrapping_add(imm as u32),                     // addi
                    0b001 => a.checked_shl((raw >> 20) & 0x1f).unwrap_or(0), // slli
                    0b010 => ((a as i32) < imm) as u32,                      // slti
                    0b011 => (a < imm as u32) as u32,                        // sltiu
                    0b100 => a ^ imm as u32,                                 // xori
                    0b101 => {
                        let shamt = (raw >> 20) & 0x1f;
                        if funct7 == isa::ALT_FUNCT7 {
                            ((a as i32) >> shamt) as u32 // srai
                        } else {
                            a >> shamt // srli
                        }
                    }
                    0b110 => a | imm as u32, // ori
                    0b111 => a & imm as u32, // andi
                    _ => return Err(Trap::IllegalInstruction(pc)),
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
                        _ => return Err(Trap::IllegalInstruction(pc)),
                    }
                };
                self.write_rd(rd, val);
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::LOAD => {
                let addr = self.x(rs1).wrapping_add(imm as u32);
                let val = match funct3 {
                    isa::funct3::LB => (mem.read_bytes(addr, 1) as u8 as i8) as i32 as u32,
                    isa::funct3::LH => (mem.read_bytes(addr, 2) as u16 as i16) as i32 as u32,
                    isa::funct3::LW => mem.read_bytes(addr, 4) as u32,
                    isa::funct3::LBU => mem.read_bytes(addr, 1) as u32,
                    isa::funct3::LHU => mem.read_bytes(addr, 2) as u32,
                    _ => return Err(Trap::IllegalInstruction(pc)),
                };
                self.write_rd(rd, val);
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::STORE => {
                let addr = self.x(rs1).wrapping_add(imm as u32);
                let val = self.x(rs2);
                match funct3 {
                    isa::funct3::SB => mem.write_bytes(addr, 1, val as u64),
                    isa::funct3::SH => mem.write_bytes(addr, 2, val as u64),
                    isa::funct3::SW => mem.write_bytes(addr, 4, val as u64),
                    _ => return Err(Trap::IllegalInstruction(pc)),
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
                    _ => return Err(Trap::IllegalInstruction(pc)),
                };
                self.pc = if taken {
                    pc.wrapping_add(imm as u32)
                } else {
                    pc.wrapping_add(4)
                };
                Ok(StepOutcome::Continue)
            }

            isa::opcode::JAL => {
                self.write_rd(rd, pc.wrapping_add(4));
                self.pc = pc.wrapping_add(imm as u32);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::JALR => {
                let target = self.x(rs1).wrapping_add(imm as u32) & !1;
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
                self.pc = pc.wrapping_add(4);
                Ok(StepOutcome::Continue)
            }

            isa::opcode::SYSTEM => {
                if funct3 == 0 {
                    match imm as u32 {
                        0x000 => Ok(StepOutcome::Ecall),
                        0x001 => Ok(StepOutcome::Ebreak),
                        // mret/sret/wfi and other system instructions.
                        _ => Err(Trap::Unsupported("system")),
                    }
                } else {
                    self.execute_csr(raw, rd, rs1, funct3, pc)
                }
            }

            _ => Err(Trap::IllegalInstruction(pc)),
        }
    }

    /// Execute one of the six Zicsr read/modify/write instructions.
    fn execute_csr(
        &mut self,
        raw: u32,
        rd: u32,
        rs1: u32,
        funct3: u32,
        pc: u32,
    ) -> Result<StepOutcome, Trap> {
        let address = raw >> 20;
        let old = self.csr.read(address);
        let source = if funct3 & 0b100 == 0 {
            self.x(rs1)
        } else {
            rs1 // The rs1 field encodes the five-bit immediate (zimm).
        };

        let write = match funct3 {
            0b001 | 0b101 => Some(source),                    // csrrw(i)
            0b010 | 0b110 if rs1 != 0 => Some(old | source),  // csrrs(i)
            0b011 | 0b111 if rs1 != 0 => Some(old & !source), // csrrc(i)
            0b010 | 0b011 | 0b110 | 0b111 => None,
            _ => return Err(Trap::IllegalInstruction(pc)),
        };

        // csr[11:10] = 0b11 denotes a read-only CSR. Pure reads through
        // CSRRS/CSRRC with a zero source remain legal.
        if let Some(value) = write {
            if (address >> 10) & 0b11 == 0b11 {
                return Err(Trap::IllegalInstruction(pc));
            }
            self.csr.write(address, value);
        }

        self.write_rd(rd, old);
        self.pc = pc.wrapping_add(4);
        Ok(StepOutcome::Continue)
    }
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
