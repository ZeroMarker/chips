//! Integration tests that hand-assemble RV32IM instruction words and check the
//! functional model executes them correctly.

use chips::cpu::{Cpu, StopReason};
use chips::mem::Memory;

/// Load a run of little-endian instruction words at `base`.
fn load_words(mem: &mut Memory, base: u32, words: &[u32]) {
    for (i, w) in words.iter().enumerate() {
        mem.store_u32(base + (i as u32) * 4, *w);
    }
}

#[test]
fn addi_add_sw_lw_ebreak() {
    // 0x1000: addi x5, x0, 42
    // 0x1004: addi x6, x5, 8
    // 0x1008: add  x7, x5, x6
    // 0x100c: sw   x7, 0(x0)
    // 0x1010: lw   x8, 0(x0)
    // 0x1014: ebreak
    let prog = [
        0x02A0_0293, // addi x5, x0, 42
        0x0082_8313, // addi x6, x5, 8
        0x0062_83B3, // add  x7, x5, x6
        0x0070_2023, // sw   x7, 0(x0)
        0x0000_2403, // lw   x8, 0(x0)
        0x0010_0073, // ebreak
    ];
    let base = 0x1000u32;
    let mut mem = Memory::new();
    load_words(&mut mem, base, &prog);

    let mut cpu = Cpu::new();
    cpu.set_pc(base);

    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(5), 42, "x5 should be 42");
    assert_eq!(cpu.reg(6), 50, "x6 should be 50");
    assert_eq!(cpu.reg(7), 92, "x7 should be 92");
    assert_eq!(cpu.reg(8), 92, "x8 should be 92 (loaded back from memory)");
}

#[test]
fn branch_not_taken_and_jal() {
    // 0x2000: addi x5, x0, 1
    // 0x2004: addi x6, x0, 2
    // 0x2008: beq  x5, x6, +8   (not taken)
    // 0x200c: addi x7, x0, 10
    // 0x2010: jal  x1, +8       (call 0x2018)
    // 0x2014: addi x8, x0, 99   (skipped)
    // 0x2018: addi x9, x0, 7
    // 0x201c: ebreak
    let prog = [
        0x0010_0293, // addi x5, x0, 1
        0x0020_0313, // addi x6, x0, 2
        0x0062_8463, // beq  x5, x6, +8
        0x00A0_0393, // addi x7, x0, 10
        0x0080_00EF, // jal  x1, +8
        0x0630_0413, // addi x8, x0, 99
        0x0070_0493, // addi x9, x0, 7
        0x0010_0073, // ebreak
    ];
    let base = 0x2000u32;
    let mut mem = Memory::new();
    load_words(&mut mem, base, &prog);

    let mut cpu = Cpu::new();
    cpu.set_pc(base);

    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(5), 1, "x5");
    assert_eq!(cpu.reg(6), 2, "x6");
    assert_eq!(cpu.reg(7), 10, "x7 (branch not taken)");
    assert_eq!(cpu.reg(8), 0, "x8 (jal skipped this instruction)");
    assert_eq!(cpu.reg(9), 7, "x9 (jal target reached)");
    assert_eq!(
        cpu.reg(1),
        0x2014,
        "x1 = ra should be the jal return address"
    );
}

#[test]
fn base_immediates_overflow() {
    // addi x5, x0, -20  (0xFEC, sign-extends to 0xFFFFFFEC)
    let mut mem = Memory::new();
    load_words(
        &mut mem,
        0x4000,
        &[
            0xFEC0_0293, // addi x5, x0, -20
            0x0010_0073, // ebreak
        ],
    );
    let mut cpu = Cpu::new();
    cpu.set_pc(0x4000);
    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(5), (-20i32) as u32, "x5 = -20");
}

#[test]
fn m_mul_div_rem() {
    // addi x5, x0, 6     -> 6
    // addi x6, x0, 7     -> 7
    // mul  x7, x5, x6    -> 42
    // addi x8, x0, -20
    // addi x9, x0, 3
    // div  x10, x8, x9   -> -6
    // rem  x11, x8, x9   -> -2
    // divu x12, x0, x0   -> div by zero -> u32::MAX
    // ebreak
    let prog = [
        0x0060_0293, // addi x5, x0, 6
        0x0070_0313, // addi x6, x0, 7
        // mul x7, x5, x6: funct7=1, rs2=6, rs1=5, funct3=0, rd=7
        0x0262_83B3,
        0xFEC0_0413, // addi x8, x0, -20
        0x0030_0493, // addi x9, x0, 3
        // div x10, x8, x9: funct7=1, rs2=9, rs1=8, funct3=4, rd=10
        0x0294_4533,
        // rem x11, x8, x9: funct7=1, rs2=9, rs1=8, funct3=6, rd=11
        0x0294_65B3,
        // divu x12, x0, x0: funct7=1, rs2=0, rs1=0, funct3=5, rd=12
        0x0200_5633,
        0x0010_0073, // ebreak
    ];
    let base = 0x5000u32;
    let mut mem = Memory::new();
    load_words(&mut mem, base, &prog);

    let mut cpu = Cpu::new();
    cpu.set_pc(base);

    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(5), 6, "x5");
    assert_eq!(cpu.reg(6), 7, "x6");
    assert_eq!(cpu.reg(7), 42, "x7 = 6 * 7");
    assert_eq!(cpu.reg(10), (-6i32) as u32, "x10 = -20 / 3 = -6");
    assert_eq!(cpu.reg(11), (-2i32) as u32, "x11 = -20 % 3 = -2");
    assert_eq!(cpu.reg(12), u32::MAX, "x12 = 0 / 0 -> all ones");
}
