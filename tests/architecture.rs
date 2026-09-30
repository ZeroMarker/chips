//! Architectural traps, counters, and reserved-encoding checks.

mod common;

use chips::cpu::{Cpu, StepOutcome, StopReason, Trap};
use chips::Memory;
use common::*;

fn cpu_with_instruction(base: u32, instruction: u32) -> (Cpu, Memory) {
    let mut mem = Memory::permissive();
    mem.poke_u32(base, instruction);
    let mut cpu = Cpu::new();
    cpu.set_pc(base);
    (cpu, mem)
}

#[test]
fn instruction_fetch_requires_four_byte_alignment() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::permissive();
    cpu.set_pc(0x1002);

    assert_eq!(
        cpu.step(&mut mem),
        Err(Trap::InstructionAddressMisaligned(0x1002))
    );
    assert_eq!(cpu.pc(), 0x1002);
}

#[test]
fn taken_control_flow_requires_four_byte_alignment() {
    let base = 0x2000;
    let cases = [
        (0x0020_00ef, 1), // jal x1, +2
        (0x0000_0163, 0), // beq x0, x0, +2
    ];

    for (instruction, destination) in cases {
        let (mut cpu, mut mem) = cpu_with_instruction(base, instruction);
        assert_eq!(
            cpu.step(&mut mem),
            Err(Trap::InstructionAddressMisaligned(base + 2))
        );
        assert_eq!(cpu.pc(), base);
        assert_eq!(cpu.reg(destination), 0, "a trapping jump must not link");
    }

    let (mut cpu, mut mem) = cpu_with_instruction(base, 0x0000_1163); // bne x0, x0, +2
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(
        cpu.pc(),
        base + 4,
        "an untaken branch does not check its target"
    );
}

#[test]
fn misaligned_data_accesses_complete_rather_than_trap() {
    // The base ISA leaves misaligned data accesses implementation-defined
    // ("may be supported"). This model completes them, matching Spike, QEMU, and
    // real hardware, because that is what a differential test against them will
    // expect and what `riscv-tests`' `ma_data` requires.
    //
    // Instruction fetch is a different matter: IALIGN = 32 makes a four-byte
    // target architecturally mandatory, which the other tests here cover.
    let code = 0x3000;
    let data = 0x4000;

    // A misaligned `lw` splices the four bytes across the boundary.
    let [hi, lo] = li32(1, data);
    let (mut cpu, mut mem) = machine(code, &[hi, lo, load(1, 1, 0b010, 2)]); // lw x2, 1(x1)
    mem.poke_u32(data + 1, 0xAABB_CCDD);
    for _ in 0..3 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(
        cpu.reg(2),
        0xAABB_CCDD,
        "a misaligned lw reads its four bytes little-endian"
    );

    // A misaligned `sh` writes only the two bytes it covers.
    let [hi, lo] = li32(1, data);
    let (mut cpu, mut mem) = machine(
        code,
        &[
            hi,
            lo,
            li(2, 0x5A),
            store(1, 2, 1, 0b001), // sh x2, 1(x1)
            ebreak(),
        ],
    );
    mem.poke_u32(data + 8, 0xFFFF_FFFF);
    assert_eq!(cpu.run(&mut mem, 10), Ok(StopReason::Ebreak));
    assert_eq!(mem.peek_u8(data + 1), 0x5A);
    assert_eq!(mem.peek_u8(data + 2), 0x00, "the high half of the halfword");
    assert_eq!(
        mem.peek_u32(data + 8),
        0xFFFF_FFFF,
        "a misaligned store must not touch bytes past its width"
    );
}

#[test]
fn cycle_and_instret_counters_track_execution() {
    let base = 0x4000;
    let mut mem = Memory::permissive();
    mem.poke_u32(base, 0xC000_22F3); // csrrs x5, cycle, x0
    mem.poke_u32(base + 4, 0xC020_2373); // csrrs x6, instret, x0
    mem.poke_u32(base + 8, 0x0010_0073); // ebreak
    let mut cpu = Cpu::new();
    cpu.set_pc(base);

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.reg(5), 1, "the first step observes cycle 1");
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.reg(6), 1, "one prior instruction has retired");
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Ebreak));
    assert_eq!(cpu.cycle(), 3);
    assert_eq!(cpu.instret(), 2, "ebreak does not retire");
}

#[test]
fn writing_minstret_suppresses_that_instructions_own_increment() {
    // The rule `riscv-tests`' instret_overflow checks: an instruction that writes
    // minstret is not counted against the counter it just set, so the next
    // reader sees the value written rather than one more than that. The same
    // applies to the high half.
    let base = 0x7000;

    // `csrwi minstret, 0` then read it back: must read 0, not 1.
    let (mut cpu, mut mem) = machine(
        base,
        &[
            csr(0b101, 0xB02, 0, 0), // csrrwi x0, minstret, 0
            csrrs(0xC02, 0, 5),      // csrrs x5, instret, x0
            ebreak(),
        ],
    );
    for _ in 0..2 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(
        cpu.reg(5),
        0,
        "the read saw the value written, not one more"
    );
    assert_eq!(
        cpu.instret(),
        1,
        "only the writing instruction was suppressed; the reader retired"
    );

    // A write to the high half suppresses too, so the counter can be driven to
    // its maximum and wrap cleanly.
    let [hi, lo] = li32(6, 0xFFFF_FFFF);
    let (mut cpu, mut mem) = machine(
        base,
        &[
            hi,
            lo,                 // x6 = 0xffffffff
            csrrw(0xB02, 6, 0), // minstret = 0xffffffff (suppressed)
            csrrw(0xB82, 6, 0), // minstreth = 0xffffffff (suppressed)
            addi(0, 0, 0),      // nop: this one does retire
            csrrs(0xC02, 0, 7), // read instret
            ebreak(),
        ],
    );
    for _ in 0..6 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(
        cpu.reg(7),
        0,
        "0xffffffff_ffffffff plus one nop wraps to zero"
    );
}

#[test]
fn reserved_encodings_are_illegal() {
    let base = 0x5000;
    let cases = [
        0x0200_1013, // slli with a reserved funct7
        0x0200_5013, // srli with a reserved funct7
        0x0000_1067, // jalr with a reserved funct3
        0x1000_000f, // fence with a reserved fm value
        0x0000_200f, // misc-mem with a reserved funct3
        0x0000_00f3, // ecall with a nonzero rd
    ];

    for instruction in cases {
        let (mut cpu, mut mem) = cpu_with_instruction(base, instruction);
        assert_eq!(
            cpu.step(&mut mem),
            Err(Trap::IllegalInstruction(instruction)),
            "encoding 0x{instruction:08x} must trap with its own encoding as mtval"
        );
    }
}

#[test]
fn canonical_fence_encodings_are_accepted() {
    let base = 0x6000;
    for instruction in [0x0ff0_000f, 0x8330_000f, 0x0000_100f] {
        let (mut cpu, mut mem) = cpu_with_instruction(base, instruction);
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
        assert_eq!(cpu.pc(), base + 4);
    }
}
