//! Architectural traps, counters, and reserved-encoding checks.

use chips::cpu::{Cpu, StepOutcome, Trap};
use chips::Memory;

fn cpu_with_instruction(base: u32, instruction: u32) -> (Cpu, Memory) {
    let mut mem = Memory::new();
    mem.store_u32(base, instruction);
    let mut cpu = Cpu::new();
    cpu.set_pc(base);
    (cpu, mem)
}

#[test]
fn instruction_fetch_requires_four_byte_alignment() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
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
fn loads_and_stores_enforce_natural_alignment() {
    let base = 0x3000;

    let (mut load_cpu, mut load_mem) = cpu_with_instruction(base, 0x0010_0093); // addi x1, x0, 1
    load_mem.store_u32(base + 4, 0x0000_a103); // lw x2, 0(x1)
    assert_eq!(load_cpu.step(&mut load_mem), Ok(StepOutcome::Continue));
    assert_eq!(
        load_cpu.step(&mut load_mem),
        Err(Trap::LoadAddressMisaligned(1))
    );
    assert_eq!(load_cpu.reg(2), 0);

    let (mut store_cpu, mut store_mem) = cpu_with_instruction(base, 0x0010_0093); // addi x1, x0, 1
    store_mem.store_u32(base + 4, 0x0020_9023); // sh x2, 0(x1)
    store_mem.write_u8(1, 0xaa);
    store_mem.write_u8(2, 0xbb);
    assert_eq!(store_cpu.step(&mut store_mem), Ok(StepOutcome::Continue));
    assert_eq!(
        store_cpu.step(&mut store_mem),
        Err(Trap::StoreAddressMisaligned(1))
    );
    assert_eq!(store_mem.read_u8(1), 0xaa, "a trapping store has no effect");
    assert_eq!(store_mem.read_u8(2), 0xbb, "a trapping store has no effect");
}

#[test]
fn cycle_and_instret_counters_track_execution() {
    let base = 0x4000;
    let mut mem = Memory::new();
    mem.store_u32(base, 0xC000_22F3); // csrrs x5, cycle, x0
    mem.store_u32(base + 4, 0xC020_2373); // csrrs x6, instret, x0
    mem.store_u32(base + 8, 0x0010_0073); // ebreak
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
            Err(Trap::IllegalInstruction(base)),
            "encoding 0x{instruction:08x} must trap"
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
