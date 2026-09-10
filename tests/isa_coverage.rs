//! Table-driven coverage of the RV32IM instruction groups and their
//! architectural edge cases.
//!
//! Test code lives at 0x100 and data at 0x40, so every address used here fits in
//! a single `addi` immediate; that keeps the programs readable.

mod common;

use chips::cpu::{StepOutcome, StopReason};
use common::*;

const CODE: u32 = 0x100;
const DATA: u32 = 0x40;

/// `x7 = op(x5, x6)` with both operands materialized as full 32-bit constants.
fn binary_op(funct7: u32, funct3: u32, a: u32, b: u32) -> Vec<u32> {
    let mut prog = Vec::new();
    prog.extend_from_slice(&li32(5, a));
    prog.extend_from_slice(&li32(6, b));
    prog.push(r(funct7, 6, 5, funct3, 7));
    prog.push(ebreak());
    prog
}

#[test]
fn op_imm_shift_encodings() {
    let prog = [
        li(5, -16),                  // x5 = 0xfffffff0
        shift(0, 4, 5, 0b001, 6),    // slli x6, x5, 4
        shift(0, 4, 5, 0b101, 7),    // srli x7, x5, 4
        shift(0x20, 4, 5, 0b101, 8), // srai x8, x5, 4
        shift(0, 0, 6, 0b001, 9),    // slli x9, x6, 0 (zero shift amount)
        ebreak(),
    ];
    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(cpu.reg(6), 0xffff_ff00, "slli shifts left");
    assert_eq!(cpu.reg(7), 0x0fff_ffff, "srli shifts in zeros");
    assert_eq!(cpu.reg(8), 0xffff_ffff, "srai replicates the sign");
    assert_eq!(cpu.reg(9), 0xffff_ff00, "slli by zero is a no-op");
}

#[test]
fn lui_and_auipc_load_upper_immediates() {
    let prog = [
        lui(0x12345, 5),   // x5 = 0x12345000
        auipc(0x00001, 6), // x6 = (CODE + 4) + 0x1000
        ebreak(),
    ];
    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(cpu.reg(5), 0x1234_5000);
    assert_eq!(cpu.reg(6), (CODE + 4).wrapping_add(0x1000));
}

#[test]
fn jalr_links_and_clears_the_low_bit() {
    // The target register holds an odd address; JALR clears bit 0 before
    // jumping, and the link register holds the instruction after the jump.
    let mut prog = vec![
        li(5, (CODE + 0x20 + 1) as i32),
        jalr(0, 5, 1), // jalr x1, 0(x5)
        li(6, 0x99),   // skipped by the jump
    ];
    while prog.len() < 8 {
        prog.push(addi(0, 0, 0)); // padding up to CODE + 0x20
    }
    prog.push(li(7, 0x07));
    prog.push(ebreak());

    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(cpu.reg(1), CODE + 8, "jalr links the following instruction");
    assert_eq!(cpu.reg(6), 0, "the jump skips the next instruction");
    assert_eq!(
        cpu.reg(7),
        7,
        "execution resumes at the target with bit 0 clear"
    );
}

#[test]
fn load_variants_sign_and_zero_extend() {
    let prog = [
        li(5, DATA as i32),
        load(0, 5, 0b000, 6),  // lb  x6, 0(x5)
        load(1, 5, 0b000, 7),  // lb  x7, 1(x5)
        load(0, 5, 0b100, 8),  // lbu x8, 0(x5)
        load(1, 5, 0b100, 9),  // lbu x9, 1(x5)
        load(0, 5, 0b001, 10), // lh  x10, 0(x5)
        load(0, 5, 0b101, 11), // lhu x11, 0(x5)
        load(0, 5, 0b010, 12), // lw  x12, 0(x5)
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);
    // Halfword 0xff80: bit 15 and bit 7 both set.
    mem.write_u8(DATA, 0x80);
    mem.write_u8(DATA + 1, 0xff);

    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(6), (-128i32) as u32, "lb sign-extends");
    assert_eq!(cpu.reg(7), u32::MAX, "lb sign-extends 0xff to -1");
    assert_eq!(cpu.reg(8), 0x80, "lbu zero-extends");
    assert_eq!(cpu.reg(9), 0xff, "lbu zero-extends");
    assert_eq!(cpu.reg(10), (-128i32) as u32, "lh sign-extends");
    assert_eq!(cpu.reg(11), 0xff80, "lhu zero-extends");
    assert_eq!(
        cpu.reg(12),
        0x0000_ff80,
        "lw reads the whole halfword region"
    );
}

#[test]
fn store_variants_write_little_endian() {
    let prog = [
        lui(0x12345, 5), // x5 = 0x12345000
        li(6, DATA as i32),
        store(0, 5, 6, 0b010), // sw x5, 0(x6)
        store(4, 5, 6, 0b001), // sh x5, 4(x6)
        store(6, 5, 6, 0b000), // sb x5, 6(x6)
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ebreak));
    assert_eq!(mem.read_bytes(DATA, 4), 0x1234_5000, "sw writes a word");
    assert_eq!(mem.read_u8(DATA), 0x00, "the word is little-endian");
    assert_eq!(mem.read_u8(DATA + 3), 0x12);
    assert_eq!(
        mem.read_bytes(DATA + 4, 2),
        0x5000,
        "sh writes the low halfword"
    );
    assert_eq!(mem.read_u8(DATA + 6), 0x00, "sb writes the low byte");
    assert_eq!(
        mem.read_u8(DATA + 7),
        0x00,
        "bytes above the width are untouched"
    );
}

#[test]
fn all_branch_conditions() {
    // +0x00: li x5, a   +0x04: li x6, b   +0x08: branch +8 -> +0x10
    // +0x0c: jal x0, +8 (the not-taken path)   +0x10: li x7, 1   +0x14: ebreak
    let cases: [(u32, i32, i32, u32); 12] = [
        (0b000, 1, 1, 1),               // beq taken
        (0b000, 1, 2, 0),               // beq not taken
        (0b001, 1, 2, 1),               // bne taken
        (0b001, 1, 1, 0),               // bne not taken
        (0b100, -1, 1, 1),              // blt compares signed
        (0b100, 1, -1, 0),              // blt signed, not taken
        (0b101, 1, -1, 1),              // bge signed
        (0b101, -1, 1, 0),              // bge signed, not taken
        (0b110, 1, u32::MAX as i32, 1), // bltu compares unsigned
        (0b110, u32::MAX as i32, 1, 0), // bltu unsigned, not taken
        (0b111, u32::MAX as i32, 1, 1), // bgeu unsigned
        (0b111, 1, u32::MAX as i32, 0), // bgeu unsigned, not taken
    ];

    for (funct3, a, b, expected) in cases {
        let prog = [
            li(5, a),
            li(6, b),
            branch(8, 6, 5, funct3),
            jal(8, 0), // not taken: jump over the `li x7, 1`
            li(7, 1),
            ebreak(),
        ];
        let cpu = run_until_halt(CODE, &prog);
        assert_eq!(
            cpu.reg(7),
            expected,
            "funct3={funct3:03b} with a={a}, b={b}"
        );
    }
}

#[test]
fn multiply_variants() {
    let cases: [(u32, u32, u32, u32); 6] = [
        (0b000, u32::MAX, u32::MAX, 1),            // mul: (-1) * (-1) = 1
        (0b001, u32::MAX, 2, u32::MAX),            // mulh: -2 >> 32 = -1
        (0b001, 0x4000_0000, 4, 1),                // mulh: 2^30 * 4 >> 32
        (0b010, u32::MAX, 2, u32::MAX),            // mulhsu: signed × unsigned
        (0b011, u32::MAX, 2, 1),                   // mulhu: (2^32-1) * 2 >> 32
        (0b011, u32::MAX, u32::MAX, u32::MAX - 1), // mulhu: (2^32-1)^2 >> 32
    ];

    for (funct3, a, b, expected) in cases {
        let cpu = run_until_halt(CODE, &binary_op(0x01, funct3, a, b));
        assert_eq!(
            cpu.reg(7),
            expected,
            "funct3={funct3:03b} with a=0x{a:08x}, b=0x{b:08x}"
        );
    }
}

#[test]
fn divide_and_remainder_edge_cases() {
    let cases: [(u32, u32, u32, u32); 10] = [
        (0b100, (-20i32) as u32, 3, (-6i32) as u32), // div truncates toward zero
        (0b110, (-20i32) as u32, 3, (-2i32) as u32), // rem takes the dividend's sign
        (0b100, 5, (-3i32) as u32, (-1i32) as u32),  // div, negative divisor
        (0b110, 5, (-3i32) as u32, 2),               // rem, negative divisor
        (0b100, 5, 0, u32::MAX),                     // div by zero -> -1
        (0b101, 5, 0, u32::MAX),                     // divu by zero -> all ones
        (0b110, 5, 0, 5),                            // rem by zero -> dividend
        (0b111, 5, 0, 5),                            // remu by zero -> dividend
        (0b100, i32::MIN as u32, u32::MAX, i32::MIN as u32), // overflow -> dividend
        (0b110, i32::MIN as u32, u32::MAX, 0),       // remainder of that overflow
    ];

    for (funct3, a, b, expected) in cases {
        let cpu = run_until_halt(CODE, &binary_op(0x01, funct3, a, b));
        assert_eq!(
            cpu.reg(7),
            expected,
            "funct3={funct3:03b} with a=0x{a:08x}, b=0x{b:08x}"
        );
    }
}

#[test]
fn fence_i_retires() {
    let (mut cpu, mut mem) = machine(CODE, &[fence_i(), ebreak()]);

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.pc(), CODE + 4, "fence.i advances the PC");
    assert_eq!(cpu.instret(), 1, "fence.i retires");
}
