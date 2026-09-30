//! Hand-assembler helpers shared by the integration tests.
//!
//! Hand-writing instruction words is error prone — a mistyped field silently
//! tests a different instruction. These encoders take the architectural fields
//! and assemble the word, so tests state what they mean.
#![allow(dead_code)]

use chips::cpu::Cpu;
use chips::mem::Memory;

pub const OP: u32 = 0x33;
pub const OP_IMM: u32 = 0x13;
pub const LOAD: u32 = 0x03;
pub const STORE: u32 = 0x23;
pub const BRANCH: u32 = 0x63;
pub const JAL: u32 = 0x6f;
pub const JALR: u32 = 0x67;
pub const LUI: u32 = 0x37;
pub const AUIPC: u32 = 0x17;
pub const MISC_MEM: u32 = 0x0f;
pub const SYSTEM: u32 = 0x73;

/// R-type: `OP` with `funct7`/`funct3` selecting the operation.
pub const fn r(funct7: u32, rs2: u32, rs1: u32, funct3: u32, rd: u32) -> u32 {
    (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | OP
}

/// I-type with the `OP_IMM` opcode (`addi`, `slti`, `xori`, …).
pub const fn i(imm: i32, rs1: u32, funct3: u32, rd: u32) -> u32 {
    (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | OP_IMM
}

/// `addi rd, rs1, imm` — also the standard way to materialize a small constant.
pub const fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i(imm, rs1, 0b000, rd)
}

/// `addi rd, x0, imm` — materializes a *12-bit* immediate only.
///
/// The assertion is deliberate: `li(rd, 0x4000_0000)` would silently assemble
/// an immediate of 0 and test a different program than intended.
pub const fn li(rd: u32, imm: i32) -> u32 {
    assert!(
        imm >= -2048 && imm <= 2047,
        "li materializes only a 12-bit immediate; use li32 for wider values"
    );
    addi(rd, 0, imm)
}

/// Materialize any 32-bit constant into `rd` with `lui` + `addi`.
pub fn li32(rd: u32, value: u32) -> [u32; 2] {
    let hi = (value.wrapping_add(0x800)) >> 12;
    let lo = (value as i32).wrapping_sub((hi << 12) as i32);
    [lui(hi, rd), addi(rd, rd, lo)]
}

/// OP-IMM shift: `funct7` = 0 for logical, 0x20 for arithmetic right shift.
pub const fn shift(funct7: u32, shamt: u32, rs1: u32, funct3: u32, rd: u32) -> u32 {
    (funct7 << 25) | ((shamt & 0x1f) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | OP_IMM
}

/// `lui rd, upper` where `upper` is the raw 20-bit field (bits 31:12 of the
/// result).
pub const fn lui(upper: u32, rd: u32) -> u32 {
    ((upper & 0xf_ffff) << 12) | (rd << 7) | LUI
}

/// `auipc rd, upper`, PC-relative like `lui` but with the current PC added.
pub const fn auipc(upper: u32, rd: u32) -> u32 {
    ((upper & 0xf_ffff) << 12) | (rd << 7) | AUIPC
}

/// I-type load (`lb`/`lh`/`lw`/`lbu`/`lhu`).
pub const fn load(imm: i32, rs1: u32, funct3: u32, rd: u32) -> u32 {
    (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | LOAD
}

/// S-type store (`sb`/`sh`/`sw`).
pub const fn store(imm: i32, rs2: u32, rs1: u32, funct3: u32) -> u32 {
    let imm = imm as u32;
    (((imm >> 5) & 0x7f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | ((imm & 0x1f) << 7)
        | STORE
}

/// B-type conditional branch. `imm` is a byte offset from the branch.
pub const fn branch(imm: i32, rs2: u32, rs1: u32, funct3: u32) -> u32 {
    let imm = imm as u32;
    (((imm >> 12) & 1) << 31)
        | (((imm >> 5) & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (((imm >> 1) & 0xf) << 8)
        | (((imm >> 11) & 1) << 7)
        | BRANCH
}

/// `jal rd, imm` — J-type jump with a byte offset.
pub const fn jal(imm: i32, rd: u32) -> u32 {
    let imm = imm as u32;
    (((imm >> 20) & 1) << 31)
        | (((imm >> 1) & 0x3ff) << 21)
        | (((imm >> 11) & 1) << 20)
        | (((imm >> 12) & 0xff) << 12)
        | (rd << 7)
        | JAL
}

/// `jalr rd, rs1, imm` — `funct3` is fixed at 0.
pub const fn jalr(imm: i32, rs1: u32, rd: u32) -> u32 {
    (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | JALR
}

/// Zicsr: `funct3` selects the operation (1 rw, 2 rs, 3 rc, +4 immediate).
pub const fn csr(funct3: u32, address: u32, rs1: u32, rd: u32) -> u32 {
    (address << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | SYSTEM
}

/// `csrrw rd, csr, rs1`.
pub const fn csrrw(address: u32, rs1: u32, rd: u32) -> u32 {
    csr(0b001, address, rs1, rd)
}

/// `csrrs rd, csr, rs1`.
pub const fn csrrs(address: u32, rs1: u32, rd: u32) -> u32 {
    csr(0b010, address, rs1, rd)
}

/// `csrrc rd, csr, rs1`.
pub const fn csrrc(address: u32, rs1: u32, rd: u32) -> u32 {
    csr(0b011, address, rs1, rd)
}

/// `csrrwi rd, csr, uimm` — the five-bit immediate travels in the `rs1` field.
pub const fn csrrwi(address: u32, uimm: u32, rd: u32) -> u32 {
    csr(0b101, address, uimm & 0x1f, rd)
}

pub const fn ecall() -> u32 {
    0x0000_0073
}

pub const fn ebreak() -> u32 {
    0x0010_0073
}

pub const fn mret() -> u32 {
    0x3020_0073
}

pub const fn sret() -> u32 {
    0x1020_0073
}

pub const fn wfi() -> u32 {
    0x1050_0073
}

pub const fn fence_i() -> u32 {
    0x0000_100f
}

/// Write `words` as little-endian 32-bit instructions starting at `base`.
pub fn load_words(mem: &mut Memory, base: u32, words: &[u32]) {
    for (i, w) in words.iter().enumerate() {
        mem.poke_u32(base + (i as u32) * 4, *w);
    }
}

/// A CPU with `words` loaded at `base` and its PC set there.
pub fn machine(base: u32, words: &[u32]) -> (Cpu, Memory) {
    let mut mem = Memory::permissive();
    load_words(&mut mem, base, words);
    let mut cpu = Cpu::new();
    cpu.set_pc(base);
    (cpu, mem)
}

/// Run a program to its halting `ecall`/`ebreak`, panicking on a trap.
pub fn run_until_halt(base: u32, words: &[u32]) -> Cpu {
    let (mut cpu, mut mem) = machine(base, words);
    match cpu.run(&mut mem, 10_000) {
        Ok(chips::cpu::StopReason::Ecall) | Ok(chips::cpu::StopReason::Ebreak) => cpu,
        other => panic!("program did not halt: {other:?}"),
    }
}
