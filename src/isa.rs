//! RISC-V ISA constants, register definitions, and RV32IM instruction decoding.
//!
//! Encoding is little-endian 32-bit. Utilities here are shared by the CPU
//! execute loop and the instruction-level tests.

/// Logical register width in bits (RV32).
pub const XLEN: u32 = 32;

/// Number of integer registers (`x0`–`x31`).
pub const NUM_REGS: usize = 32;

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
    pub const OP_IMM: u32 = 0x13;
    pub const AUIPC: u32 = 0x17;
    pub const STORE: u32 = 0x23;
    pub const OP: u32 = 0x33;
    pub const LUI: u32 = 0x37;
    pub const BRANCH: u32 = 0x63;
    pub const JALR: u32 = 0x67;
    pub const JAL: u32 = 0x6F;
    pub const MISC_MEM: u32 = 0x0F;
    pub const SYSTEM: u32 = 0x73;
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
    pub pc: u32,
    pub raw: u32,
    pub opcode: u32,
    pub rd: u32,
    pub rs1: u32,
    pub rs2: u32,
    pub funct3: u32,
    pub funct7: u32,
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
pub fn decode(pc: u32, raw: u32) -> Decoded {
    Decoded {
        pc,
        raw,
        opcode: raw & 0x7f,
        rd: (raw >> 7) & 0x1f,
        funct3: (raw >> 12) & 0x7,
        rs1: (raw >> 15) & 0x1f,
        rs2: (raw >> 20) & 0x1f,
        funct7: (raw >> 25) & 0x7f,
        imm: immediate(raw),
    }
}
