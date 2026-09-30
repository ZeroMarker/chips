//! Machine-mode trap entry, `mret`, and CSR field semantics.
//!
//! Test code lives at 0x100 and the handler at 0x180, so every address fits in a
//! single `addi` immediate.

mod common;

use chips::cpu::{StepOutcome, StopReason, Trap};
use chips::csr::{self, addr as csr_addr};
use common::*;

const CODE: u32 = 0x100;
const HANDLER: u32 = 0x180;

/// The two instructions that install [`HANDLER`] as the trap vector.
fn install_handler() -> [u32; 2] {
    [li(5, HANDLER as i32), csrrw(csr_addr::MTVEC, 5, 0)]
}

#[test]
fn ecall_without_handler_halts_the_run_loop() {
    let prog = [li(5, 1), ecall(), li(6, 2), ebreak()];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    assert_eq!(cpu.run(&mut mem, 100), Ok(StopReason::Ecall));
    assert_eq!(cpu.pc(), CODE + 4, "the ecall does not advance the PC");
    assert_eq!(cpu.instret(), 1, "a halting ecall does not retire");
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 0, "no trap was taken");
}

#[test]
fn ecall_enters_the_handler_with_cause_11() {
    let prog = [
        install_handler()[0],
        install_handler()[1],
        ecall(),
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert!(cpu.handler_installed(), "mtvec is non-zero");

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.pc(), HANDLER, "the PC moves to the mtvec base");
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 11, "ecall from M-mode");
    assert_eq!(cpu.csr().read(csr_addr::MEPC), CODE + 8);
    assert_eq!(cpu.csr().read(csr_addr::MTVAL), 0, "ecall reports no value");
    assert_eq!(cpu.instret(), 2, "a trapping ecall does not retire");
}

#[test]
fn breakpoint_reports_cause_3_and_the_faulting_pc() {
    let prog = [
        install_handler()[0],
        install_handler()[1],
        ebreak(),
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    cpu.step(&mut mem).unwrap();
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 3, "breakpoint");
    assert_eq!(cpu.csr().read(csr_addr::MEPC), CODE + 8);
    assert_eq!(cpu.csr().read(csr_addr::MTVAL), CODE + 8, "the ebreak PC");
}

#[test]
fn a_misaligned_load_does_not_trap() {
    // Misaligned data accesses are completed rather than trapped, so with a
    // handler installed the load must run to completion and never reach it.
    // `mcause` 4 is therefore unreachable in this model; the variant survives
    // only because it is part of the architecture.
    let prog = [
        install_handler()[0],
        install_handler()[1],
        li(6, 3),
        load(0, 6, 0b010, 7), // lw x7, 0(x6) with an unaligned address
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    for _ in 0..3 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(
        cpu.step(&mut mem),
        Ok(StepOutcome::Continue),
        "a misaligned load completes rather than trapping"
    );
    assert_eq!(cpu.pc(), CODE + 16, "it advanced past the load");
    assert_eq!(
        cpu.csr().read(csr_addr::MEPC),
        0,
        "the handler was not entered"
    );
    // The `ebreak` *does* trap, because a handler is installed and so it is
    // delivered rather than halting the run. That is the contrast being pinned
    // down: the misaligned load above never reached the handler, this one does.
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 3, "breakpoint");
}

#[test]
fn misaligned_branch_target_reports_cause_0() {
    let prog = [
        install_handler()[0],
        install_handler()[1],
        branch(2, 0, 0, 0b000), // beq x0, x0, +2 — a target that cannot be fetched
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    cpu.step(&mut mem).unwrap();
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(
        cpu.csr().read(csr_addr::MCAUSE),
        0,
        "instruction address misaligned"
    );
    assert_eq!(
        cpu.csr().read(csr_addr::MTVAL),
        CODE + 10,
        "the untaken target"
    );
    assert_eq!(
        cpu.csr().read(csr_addr::MEPC),
        CODE + 8,
        "the branch itself"
    );
}

#[test]
fn illegal_instruction_reports_its_encoding_as_mtval() {
    let bad = 0x0200_1013; // slli with a reserved funct7
    let prog = [install_handler()[0], install_handler()[1], bad, ebreak()];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    cpu.step(&mut mem).unwrap();
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 2, "illegal instruction");
    assert_eq!(
        cpu.csr().read(csr_addr::MTVAL),
        bad,
        "the faulting encoding"
    );
    assert_eq!(cpu.csr().read(csr_addr::MEPC), CODE + 8);
}

#[test]
fn mret_restores_the_interrupt_enable_stack_and_returns_to_mepc() {
    let mut prog = vec![
        li(5, 1 << 3),                  // +0x00: mstatus.MIE
        csrrs(csr_addr::MSTATUS, 5, 0), // +0x04: enable machine interrupts
        li(5, HANDLER as i32),          // +0x08
        csrrw(csr_addr::MTVEC, 5, 0),   // +0x0c: install the handler
        ecall(),                        // +0x10: traps
        li(7, 0x55),                    // +0x14: resumed by mret
        csrrw(csr_addr::MTVEC, 0, 0),   // +0x18: uninstall, so ebreak halts
        ebreak(),                       // +0x1c
    ];
    while prog.len() < (HANDLER - CODE) as usize / 4 {
        prog.push(addi(0, 0, 0)); // padding up to the handler
    }
    prog.extend_from_slice(&[
        csrrs(csr_addr::MCAUSE, 0, 10),  // x10 = mcause
        csrrs(csr_addr::MEPC, 0, 6),     // x6 = mepc
        addi(6, 6, 4),                   // step past the ecall
        csrrw(csr_addr::MEPC, 6, 0),     // mepc = x6
        csrrs(csr_addr::MSTATUS, 0, 11), // x11 = mstatus inside the handler
        mret(),
    ]);

    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(cpu.reg(10), 11, "mcause = environment call from M-mode");
    let in_handler = cpu.reg(11);
    assert_eq!(in_handler & 0x1800, 0x1800, "MPP records machine mode");
    assert_eq!(in_handler & 0x80, 0x80, "MPIE saved the enabled MIE");
    assert_eq!(in_handler & 0x8, 0, "MIE is cleared on trap entry");
    assert_eq!(
        cpu.reg(7),
        0x55,
        "mret returned to the instruction after the ecall"
    );

    let mstatus = cpu.csr().read(csr_addr::MSTATUS);
    assert_eq!(mstatus & 0x8, 0x8, "mret restored MIE from MPIE");
    assert_eq!(mstatus & 0x80, 0x80, "mret leaves MPIE set");
}

#[test]
fn mtvec_and_mepc_legalize_warl_writes() {
    let mut prog = Vec::new();
    prog.extend_from_slice(&li32(5, 0x1006)); // reserved mode 2
    prog.push(csrrw(csr_addr::MTVEC, 5, 0));
    prog.push(csrrs(csr_addr::MTVEC, 0, 6));
    prog.extend_from_slice(&li32(5, 0x1001)); // vectored mode 1 is supported
    prog.push(csrrw(csr_addr::MTVEC, 5, 0));
    prog.push(csrrs(csr_addr::MTVEC, 0, 7));
    prog.extend_from_slice(&li32(5, 0x1003)); // mepc has no low bits with IALIGN = 32
    prog.push(csrrw(csr_addr::MEPC, 5, 0));
    prog.push(csrrs(csr_addr::MEPC, 0, 8));
    prog.push(csrrw(csr_addr::MTVEC, 0, 0)); // uninstall so ebreak halts
    prog.push(ebreak());

    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(
        cpu.reg(6),
        0x1004,
        "reserved mtvec modes legalize to mode 0"
    );
    assert_eq!(cpu.reg(7), 0x1001, "vectored mode is preserved");
    assert_eq!(cpu.reg(8), 0x1000, "mepc clears its low two bits");
}

#[test]
fn unimplemented_csr_access_is_illegal() {
    let bad = csrrs(0x7c0, 0, 6); // a custom CSR address this model lacks
    let (mut cpu, mut mem) = machine(CODE, &[bad, ebreak()]);

    assert_eq!(cpu.step(&mut mem), Err(Trap::IllegalInstruction(bad)));
}

#[test]
fn unprivileged_counter_aliases_reject_writes() {
    for bad in [
        csrrw(csr_addr::CYCLE, 0, 0),
        csrrw(csr_addr::TIME, 0, 0),
        csrrw(csr_addr::INSTRET, 0, 0),
    ] {
        let (mut cpu, mut mem) = machine(CODE, &[bad]);
        assert_eq!(cpu.step(&mut mem), Err(Trap::IllegalInstruction(bad)));
    }
}

#[test]
fn machine_counters_are_writable_and_aliased() {
    let prog = [
        li(5, 0x100),
        csrrw(csr_addr::MCYCLE, 5, 0),
        csrrs(csr_addr::CYCLE, 0, 6), // read the read-only alias
        li(5, 0x200),
        csrrw(csr_addr::MINSTRET, 5, 0),
        csrrs(csr_addr::INSTRET, 0, 7),
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    cpu.step(&mut mem).unwrap(); // li x5, 0x100
    cpu.step(&mut mem).unwrap(); // csrrw mcycle
    assert_eq!(cpu.cycle(), 0x100, "mcycle writes the cycle counter");
    cpu.step(&mut mem).unwrap(); // csrrs cycle
    assert_eq!(cpu.reg(6), 0x101, "the read sees the step it executes in");

    cpu.step(&mut mem).unwrap(); // li x5, 0x200
    cpu.step(&mut mem).unwrap(); // csrrw minstret
    assert_eq!(
        cpu.instret(),
        0x200,
        "an instruction that writes minstret does not count itself"
    );
    cpu.step(&mut mem).unwrap(); // csrrs instret
    assert_eq!(
        cpu.reg(7),
        0x200,
        "the read sees the value that was written"
    );
    assert_eq!(cpu.instret(), 0x201, "this one did retire");
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Ebreak));
}

#[test]
fn time_csr_is_a_read_only_time_base() {
    let prog = [
        csrrs(csr_addr::TIME, 0, 6),
        csrrs(csr_addr::TIME, 0, 7),
        ebreak(),
    ];
    let (mut cpu, mut mem) = machine(CODE, &prog);

    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.reg(6), 1, "time advances once per step");
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.reg(7), 2);
    assert_eq!(cpu.mtime(), 2);

    cpu.set_mtime(1 << 33);
    let (mut high, mut high_mem) = machine(CODE, &[csrrs(csr_addr::TIMEH, 0, 6), ebreak()]);
    high.set_mtime(1 << 33);
    high.step(&mut high_mem).unwrap();
    assert_eq!(
        high.reg(6),
        (((1u64 << 33) + 1) >> 32) as u32,
        "timeh reads the high word"
    );
}

#[test]
fn misa_and_mhartid_are_read_only_constants() {
    let prog = [
        csrrs(csr_addr::MISA, 0, 6),
        csrrs(csr_addr::MHARTID, 0, 7),
        li(5, 0x7ff),
        csrrw(csr_addr::MISA, 5, 0), // ignored rather than trapped (WARL)
        csrrs(csr_addr::MISA, 0, 8),
        ebreak(),
    ];
    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(cpu.reg(6), csr::MISA_VALUE);
    assert_eq!(
        cpu.reg(6),
        (1 << 30) | (1 << 8) | (1 << 12),
        "MXL = 32-bit, I and M"
    );
    assert_eq!(cpu.reg(7), 0, "mhartid is 0 for a single hart");
    assert_eq!(cpu.reg(8), csr::MISA_VALUE, "writes to misa are ignored");
}

#[test]
fn mie_masks_unsupported_bits_and_mip_is_inert() {
    let prog = [
        li(5, -1),
        csrrw(csr_addr::MIE, 5, 0),
        csrrs(csr_addr::MIE, 0, 6),
        csrrw(csr_addr::MIP, 5, 0),
        csrrs(csr_addr::MIP, 0, 7),
        ebreak(),
    ];
    let cpu = run_until_halt(CODE, &prog);

    assert_eq!(cpu.reg(6), 0x888, "only MEIE, MTIE and MSIE exist");
    assert_eq!(cpu.reg(7), 0, "no interrupt source can raise mip");
}

#[test]
fn wfi_retires_in_every_mode() {
    let (mut cpu, mut mem) = machine(CODE, &[wfi(), ebreak()]);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.pc(), CODE + 4, "wfi advances the PC");
    assert_eq!(cpu.instret(), 1, "wfi is a legal, retiring instruction");

    // `sret` and `sfence.vma` used to be illegal encodings here, because S-mode
    // did not exist. They are legal from M now; the privilege gating is what
    // tests/privilege.rs covers.
}

#[test]
fn unimplemented_extensions_are_diagnosed() {
    let cases: [(u32, &str); 4] = [
        (0x0000_202f, "A"),   // amoadd.w x0, x0, (x0)
        (0x0000_0053, "F/D"), // fadd.s f0, f0, f0
        (0x0000_0007, "F/D"), // fld f0, 0(x0)
        (0x0000_4501, "C"),   // c.li a0, 0
    ];

    for (encoding, expected) in cases {
        let (mut cpu, mut mem) = machine(CODE, &[encoding]);
        match cpu.step(&mut mem) {
            Err(Trap::Unsupported(name)) => {
                assert_eq!(name, expected, "encoding 0x{encoding:08x}")
            }
            other => panic!("encoding 0x{encoding:08x} produced {other:?}"),
        }
    }
}
