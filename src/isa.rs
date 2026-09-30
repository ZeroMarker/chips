//! RISC-V ISA constants, register definitions, and RV32IMZicsr instruction
//! decoding.
//!
//! Encoding is little-endian 32-bit. Utilities here are shared by the CPU
//! execute loop and the instruction-level tests.

/// Logical register width in bits (RV32).
pub const XLEN: u32 = 32;

/// Number of integer registers (`x0`–`x31`).
pub const NUM_REGS: usize = 32;

/// A privilege level.
///
/// The discriminants are the architectural encodings, and the ordering is
/// meaningful: `User < Supervisor < Machine`, so "is this level at least as
/// privileged as that one" is a `<=` and a CSR's required privilege is a lower
/// bound rather than a set to walk.
///
/// Machine is 3 rather than 2 because that is what the architecture encodes and
/// 2 is reserved. The derived ordering is still correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Privilege {
    /// Unprivileged. `ecall` from here is cause 8.
    User = 0,
    /// Supervisor. `ecall` from here is cause 9.
    Supervisor = 1,
    /// Machine. The reset level, and the only one always available.
    Machine = 3,
}

impl Privilege {
    /// Every level, least privileged first.
    pub const ALL: [Privilege; 3] = [Privilege::User, Privilege::Supervisor, Privilege::Machine];

    /// The level for an `xPP` field encoding, or `None` for the reserved value 2.
    pub fn from_encoding(value: u32) -> Option<Privilege> {
        match value {
            0 => Some(Privilege::User),
            1 => Some(Privilege::Supervisor),
            3 => Some(Privilege::Machine),
            _ => None,
        }
    }

    /// The architectural encoding of this level.
    pub fn encoding(self) -> u32 {
        self as u32
    }

    /// A one-letter name, for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Privilege::User => "U",
            Privilege::Supervisor => "S",
            Privilege::Machine => "M",
        }
    }

    /// May code running at `self` access a register requiring `required`?
    ///
    /// Delegation is deliberately not consulted: this model has no `medeleg`, so
    /// nothing can be delegated and the rule is simply "at least as privileged".
    pub fn can_access(self, required: Privilege) -> bool {
        self >= required
    }
}

/// ABI names for the 32 integer registers, indexed by register number.
pub const REG_NAMES: [&str; NUM_REGS] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

/// Return the ABI name for register `i` (masked to the register window).
pub fn reg_name(i: u32) -> &'static str {
    REG_NAMES[(i as usize) & (NUM_REGS - 1)]
}

/// 7-bit major opcodes.
pub mod opcode {
    pub const LOAD: u32 = 0x03;
    pub const LOAD_FP: u32 = 0x07;
    pub const MISC_MEM: u32 = 0x0F;
    pub const OP_IMM: u32 = 0x13;
    pub const AUIPC: u32 = 0x17;
    pub const STORE: u32 = 0x23;
    pub const STORE_FP: u32 = 0x27;
    pub const AMO: u32 = 0x2F;
    pub const OP: u32 = 0x33;
    pub const LUI: u32 = 0x37;
    pub const OP_FP: u32 = 0x53;
    pub const OP_V: u32 = 0x57;
    pub const BRANCH: u32 = 0x63;
    pub const JALR: u32 = 0x67;
    pub const JAL: u32 = 0x6F;
    pub const SYSTEM: u32 = 0x73;
}

/// `funct12` (`inst[31:20]`) values identifying the operand-less privileged
/// SYSTEM instructions. Every other `funct3 = 0` SYSTEM encoding is illegal in
/// this machine-mode-only RV32IMZicsr model.
pub mod system {
    pub const ECALL: u32 = 0x000;
    pub const EBREAK: u32 = 0x001;
    pub const SRET: u32 = 0x102;
    pub const WFI: u32 = 0x105;
    pub const MRET: u32 = 0x302;
}

/// `funct3` values for load/store, branch and OP/OP-IMM groups.
pub mod funct3 {
    // Loads.
    pub const LB: u32 = 0b000;
    pub const LH: u32 = 0b001;
    pub const LW: u32 = 0b010;
    pub const LBU: u32 = 0b100;
    pub const LHU: u32 = 0b101;
    // Stores.
    pub const SB: u32 = 0b000;
    pub const SH: u32 = 0b001;
    pub const SW: u32 = 0b010;
    // Branches.
    pub const BEQ: u32 = 0b000;
    pub const BNE: u32 = 0b001;
    pub const BLT: u32 = 0b100;
    pub const BGE: u32 = 0b101;
    pub const BLTU: u32 = 0b110;
    pub const BGEU: u32 = 0b111;
}

/// `funct7` value that selects the ALU/compare "second" op (e.g. `sub`,
/// `sra`) within the `OP`/`OP-IMM` groups.
pub const ALT_FUNCT7: u32 = 0x20;

/// `funct7` value that selects the M extension within the `OP` group.
pub const M_FUNCT7: u32 = 0x01;

/// A decoded 32-bit instruction.
#[derive(Debug, Clone, Copy)]
pub struct Decoded {
    pub raw: u32,
    pub opcode: u32,
    pub rd: u32,
    pub rs1: u32,
    pub rs2: u32,
    pub funct3: u32,
    pub funct7: u32,
    /// `inst[31:20]`: the CSR address for Zicsr instructions, the operand-less
    /// SYSTEM selector (`ecall`, `mret`, …), or the shift amount for OP-IMM.
    pub funct12: u32,
    /// Sign-extended immediate, decoded per the instruction's format.
    pub imm: i32,
}

/// Sign-extend the low `bits` bits of `val` to 32 bits.
#[inline]
fn sign_extend(val: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((val << shift) as i32) >> shift
}

/// I-type immediate (`inst[31:20]`, signed 12-bit).
#[inline]
fn imm_i(inst: u32) -> i32 {
    sign_extend(inst >> 20, 12)
}

/// S-type immediate (`inst[31:25:11:7]`, signed 12-bit).
#[inline]
fn imm_s(inst: u32) -> i32 {
    sign_extend(((inst >> 25) << 5) | ((inst >> 7) & 0x1f), 12)
}

/// B-type immediate (`inst[31:7:30:25:11:8]`, signed 13-bit).
#[inline]
fn imm_b(inst: u32) -> i32 {
    let imm = ((inst >> 31) << 12)
        | (((inst >> 7) & 0x1) << 11)
        | (((inst >> 25) & 0x3f) << 5)
        | (((inst >> 8) & 0xf) << 1);
    sign_extend(imm, 13)
}

/// U-type immediate (`inst[31:12]`, left-justified into the high half-word).
#[inline]
fn imm_u(inst: u32) -> i32 {
    (inst & 0xFFFF_F000) as i32
}

/// J-type immediate (`inst[31:19:20:12]`, signed 21-bit).
#[inline]
fn imm_j(inst: u32) -> i32 {
    let imm = ((inst >> 31) << 20)
        | (((inst >> 12) & 0xff) << 12)
        | (((inst >> 20) & 0x1) << 11)
        | (((inst >> 21) & 0x3ff) << 1);
    sign_extend(imm, 21)
}

// Select the correct immediate decoder for a given opcode.
#[inline]
fn immediate(inst: u32) -> i32 {
    match inst & 0x7f {
        opcode::STORE => imm_s(inst),
        opcode::BRANCH => imm_b(inst),
        opcode::LUI | opcode::AUIPC => imm_u(inst),
        opcode::JAL => imm_j(inst),
        // LOAD, OP_IMM, JALR, SYSTEM (ecall/ebreak/CSR) use the I format.
        _ => imm_i(inst),
    }
}

/// Decode a 32-bit little-endian instruction word.
pub fn decode(raw: u32) -> Decoded {
    Decoded {
        raw,
        opcode: raw & 0x7f,
        rd: (raw >> 7) & 0x1f,
        funct3: (raw >> 12) & 0x7,
        rs1: (raw >> 15) & 0x1f,
        rs2: (raw >> 20) & 0x1f,
        funct7: (raw >> 25) & 0x7f,
        funct12: raw >> 20,
        imm: immediate(raw),
    }
}
