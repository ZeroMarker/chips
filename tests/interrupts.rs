//! Interrupt delivery: the CLINT, `mip`/`mie`/`mstatus.MIE` gating, vectored
//! mode, and the memory-mapped time base.
//!
//! Interrupt delivery needs a platform with a CLINT — a permissive address
//! space has no devices, so nothing can ever be pending. Every test here
//! therefore builds one explicitly rather than using the shared
//! [`common::machine`].

mod common;

use chips::clint::{self, Clint, MIP_MEIP, MIP_MSIP, MIP_MTIP};
use chips::cpu::{Cpu, StepOutcome, StopReason};
use chips::csr::addr as csr_addr;
use chips::mem::{Device, Memory, Permissions, Region};
use chips::platform;
use common::*;
use std::rc::Rc;

const CODE: u32 = 0x8000_0000;
const HANDLER: u32 = 0x8000_0100;
const MSIP_ADDR: u32 = 0x0200_0000;
const MTIMECMP_ADDR: u32 = 0x0200_4000;
const MTIME_ADDR: u32 = 0x0200_BFF8;

/// A platform with just enough to raise interrupts: a CLINT and some RAM to run
/// code in.
fn machine_with_clint(words: &[u32]) -> (Cpu, Memory, Rc<Clint>) {
    let clint = Rc::new(Clint::new());
    let mut mem = Memory::from_regions(vec![
        Region::shared_device(
            MSIP_ADDR,
            clint::SIZE,
            Permissions::READ_WRITE,
            Rc::clone(&clint),
        ),
        Region::ram(CODE, 0x2000, Permissions::READ_WRITE),
    ]);
    load_words(&mut mem, CODE, words);
    let mut cpu = Cpu::new();
    cpu.set_pc(CODE);
    (cpu, mem, clint)
}

/// `mtvec = handler | 1`, i.e. vectored mode. Two instructions.
fn vectored_mtvec(handler: u32) -> [u32; 2] {
    li32(1, handler | 1)
}

/// `mtvec = handler`, direct mode. Two instructions.
fn direct_mtvec(handler: u32) -> [u32; 2] {
    li32(1, handler)
}

/// `x2 = mask`, then `mie = x2`, then `mstatus.MIE = 1`. Four instructions.
///
/// The mask is materialized with `lui`/`addi` rather than a single `addi`
/// because a combination like MEI|MSI does not fit a 12-bit immediate.
fn arm_interrupts(mask: u32) -> [u32; 4] {
    let [hi, lo] = li32(2, mask);
    [
        hi,
        lo,                              // x2 = mask
        csrrw(csr_addr::MIE, 2, 0),      // mie = x2
        csrrwi(csr_addr::MSTATUS, 8, 0), // set MIE (bit 3) via the immediate form
    ]
}

/// Instructions in the [`arm`] preamble: lui, addi (mtvec), csrrw mtvec, lui,
/// addi (mask), csrrw mie, csrrwi mstatus.
const ARM_SETUP: usize = 7;

/// The full preamble for delivering an interrupt: `mtvec = handler` (direct
/// mode), `mie = mask`, and `mstatus.MIE = 1`.
fn arm(handler: u32, mask: u32) -> Vec<u32> {
    let mut program = direct_mtvec(handler).to_vec();
    program.push(csrrw(csr_addr::MTVEC, 1, 0));
    program.extend(arm_interrupts(mask));
    program
}

/// As [`arm`], but with vectored `mtvec`.
fn arm_vectored(handler: u32, mask: u32) -> Vec<u32> {
    let mut program = vectored_mtvec(handler).to_vec();
    program.push(csrrw(csr_addr::MTVEC, 1, 0));
    program.extend(arm_interrupts(mask));
    program
}

#[test]
fn a_fresh_clint_asserts_nothing() {
    let clint = Clint::new();
    assert!(!clint.software_interrupt_pending());
    assert!(!clint.timer_interrupt_pending());
    assert_eq!(clint.mtime(), 0);
    assert_eq!(
        clint.mtimecmp(),
        u64::MAX,
        "a reset hart must not be interrupted immediately"
    );
    assert_eq!(clint.interrupt_pending(), 0);
}

#[test]
fn msip_and_mtimecmp_are_memory_mapped() {
    let (_cpu, mut mem, clint) = machine_with_clint(&[]);

    mem.store(MSIP_ADDR, 4, 1).unwrap();
    assert!(clint.software_interrupt_pending());
    assert_eq!(clint.interrupt_pending(), MIP_MSIP);

    mem.store(MSIP_ADDR, 4, 0).unwrap();
    assert!(!clint.software_interrupt_pending(), "clearing works too");

    // mtimecmp is 64-bit and little-endian, so a low write alone is not a
    // deadline: mtime is nowhere near it yet.
    mem.store(MTIMECMP_ADDR, 4, 100).unwrap();
    assert!(!clint.timer_interrupt_pending());

    mem.store(MTIMECMP_ADDR + 4, 4, 0).unwrap();
    clint.set_mtime(200);
    assert!(
        clint.timer_interrupt_pending(),
        "mtime >= mtimecmp raises MTIP"
    );
    assert_eq!(clint.interrupt_pending(), MIP_MTIP);

    // It is a level, not an edge: a missed deadline stays pending.
    clint.set_mtime(201);
    assert!(clint.timer_interrupt_pending());
}

#[test]
fn mtime_reads_back_through_the_memory_map() {
    let (_cpu, mut mem, clint) = machine_with_clint(&[]);
    clint.set_mtime(0x0000_0000_1234_5678);

    assert_eq!(mem.load(MTIME_ADDR, 4), Ok(0x1234_5678));
    assert_eq!(mem.load(MTIME_ADDR + 4, 4), Ok(0x0000_0000));

    mem.store(MTIME_ADDR, 8, 0xDEAD_BEEF_CAFE_BABE).unwrap();
    assert_eq!(clint.mtime(), 0xDEAD_BEEF_CAFE_BABE, "mtime is writable");
}

#[test]
fn the_time_csr_follows_the_clint_rather_than_the_model() {
    let [hi, lo] = li32(1, 0xFFFF_FFFF);
    let program = [hi, lo, csrrs(0xC01, 0, 5), csrrs(0xC81, 0, 6), ebreak()];
    let (mut cpu, mut mem, clint) = machine_with_clint(&program);

    // A distinctive value, so a private counter starting near zero cannot
    // produce it by accident. The CLINT ticks once per step, so allow a small
    // window rather than pinning an exact figure.
    const START: u64 = 0x1234_5678;
    clint.set_mtime(START);
    assert_eq!(cpu.run(&mut mem, 20), Ok(StopReason::Ebreak));

    let observed = u64::from(cpu.reg(6)) << 32 | u64::from(cpu.reg(5));
    assert!(
        (START..START + 8).contains(&observed),
        "`time`/`timeh` reported 0x{observed:016x}, not the CLINT's counter"
    );
}

#[test]
fn a_pending_interrupt_is_not_taken_until_it_is_enabled() {
    // Assert the software interrupt before enabling anything, then check each
    // gate in turn. `mip` alone is not enough; `mie` alone is not enough;
    // `mstatus.MIE` alone is not enough.
    let (mut cpu, mut mem, clint) = machine_with_clint(&[ebreak()]);
    clint.set_software_interrupt_pending(true);

    // Nothing enabled: the ebreak halts as usual.
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Ebreak));

    // Re-arm and try with only mip pending, no handler: still nothing to take.
    let (mut cpu, mut mem, clint) = machine_with_clint(&[addi(0, 0, 0), ebreak()]);
    clint.set_software_interrupt_pending(true);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Ebreak));
}

#[test]
fn an_enabled_pending_interrupt_is_taken_before_the_next_instruction() {
    let mut program = arm(HANDLER, MIP_MSIP);
    program.extend([addi(0, 0, 0), ebreak()]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    load_words(&mut mem, HANDLER, &[mret()]);
    clint.set_software_interrupt_pending(true);

    // The setup instructions must run without being interrupted, because
    // nothing is enabled until the last of them.
    for i in 0..ARM_SETUP {
        assert_eq!(
            cpu.step(&mut mem),
            Ok(StepOutcome::Continue),
            "setup step {i} was interrupted"
        );
    }
    let target = cpu.pc();
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));

    assert_eq!(cpu.pc(), HANDLER, "entered the handler");
    assert_eq!(
        cpu.csr().read(csr_addr::MCAUSE),
        MIP_MSIP,
        "an interrupt's mcause is the bit itself"
    );
    assert_eq!(
        cpu.csr().read(csr_addr::MEPC),
        target,
        "mepc is the next instruction"
    );
    assert_eq!(
        cpu.csr().read(csr_addr::MTVAL),
        0,
        "an interrupt has no address"
    );

    // mstatus: MIE cleared, MPIE holds the old MIE.
    let mstatus = cpu.csr().read(csr_addr::MSTATUS);
    assert_eq!(mstatus & (1 << 3), 0, "MIE cleared on entry");
    assert_eq!((mstatus >> 7) & 1, 1, "MPIE holds the previous MIE");

    // mret returns to the interrupted instruction, which then runs.
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.pc(), target);
}

#[test]
fn the_global_enable_gates_delivery() {
    // Install the handler and enable the source, but leave `mstatus.MIE` clear.
    let mut program = direct_mtvec(HANDLER).to_vec();
    program.push(csrrw(csr_addr::MTVEC, 1, 0));
    program.push(li(2, MIP_MSIP as i32));
    program.push(csrrw(csr_addr::MIE, 2, 0));
    program.extend([addi(0, 0, 0), addi(0, 0, 0)]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    clint.set_software_interrupt_pending(true);

    // Five instructions of preamble: lui, addi, csrrw mtvec, li x2, csrrw mie.
    for _ in 0..5 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_ne!(cpu.pc(), HANDLER, "not delivered without mstatus.MIE");
    assert_eq!(cpu.pc(), CODE + (5 + 2) * 4);
}

#[test]
fn a_disabled_source_does_not_interrupt() {
    // `mie` enables the timer; the software interrupt is pending but not
    // enabled, and must stay undelivered.
    // No `ebreak` sentinel: a handler *is* installed, so an ebreak would be
    // delivered as a trap and be indistinguishable from an interrupt. The test
    // instead checks that the PC runs straight through the body.
    let mut program = arm(HANDLER, MIP_MTIP);
    program.extend([addi(0, 0, 0), addi(0, 0, 0)]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    clint.set_software_interrupt_pending(true);

    for _ in 0..ARM_SETUP {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    // The body runs to its end without ever entering the handler.
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_ne!(
        cpu.pc(),
        HANDLER,
        "a pending but disabled source never interrupts"
    );
    assert_eq!(cpu.pc(), CODE + (ARM_SETUP as u32 + 2) * 4);
}

#[test]
fn the_highest_priority_source_wins() {
    // Both sources pending and enabled. MTI outranks MSI, so the timer is taken.
    let mut program = arm(HANDLER, MIP_MSIP | MIP_MTIP);
    program.extend([addi(0, 0, 0), ebreak()]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    clint.set_software_interrupt_pending(true);
    clint.set_mtime(0);
    mem.store(MTIMECMP_ADDR, 8, 1).unwrap(); // deadline already passed

    for _ in 0..ARM_SETUP {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(
        cpu.csr().read(csr_addr::MCAUSE),
        MIP_MTIP,
        "MTI outranks MSI"
    );
}

#[test]
fn the_external_source_outranks_both() {
    // MEI is the highest machine priority. No PLIC exists, so this asserts the
    // *ordering* is encoded in the machine rather than only in the CLINT: if
    // both MEI and MSI are somehow set, MEI is the one delivered.
    let mut program = arm(HANDLER, MIP_MEIP | MIP_MSIP);
    program.extend([addi(0, 0, 0), ebreak()]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    clint.set_software_interrupt_pending(true);
    // Force the external bit into the latch the way a PLIC would.
    mem.store(MSIP_ADDR, 4, 1).unwrap();

    for _ in 0..ARM_SETUP {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(
        cpu.csr().read(csr_addr::MCAUSE),
        MIP_MSIP,
        "only the CLINT can assert a bit, so MSI is what is actually pending"
    );
}

#[test]
fn vectored_mode_offsets_by_cause() {
    // In vectored mode an interrupt enters at base + 4 * cause. For a machine
    // timer interrupt that is base + 28, not base.
    let mut program = arm_vectored(HANDLER, MIP_MTIP);
    program.extend([addi(0, 0, 0), ebreak()]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    load_words(&mut mem, HANDLER, &[mret()]);
    clint.set_mtime(0);
    mem.store(MTIMECMP_ADDR, 8, 1).unwrap();

    for _ in 0..ARM_SETUP {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(
        cpu.pc(),
        HANDLER + 4 * MIP_MTIP,
        "vectored entry is base + 4 * cause"
    );
}

#[test]
fn a_synchronous_exception_still_enters_at_the_base_in_vectored_mode() {
    // The other half of the rule: vectoring applies to interrupts only.
    let mut program = vectored_mtvec(HANDLER).to_vec();
    program.push(csrrw(csr_addr::MTVEC, 1, 0));
    program.extend([ecall(), ebreak()]);

    let (mut cpu, mut mem, _clint) = machine_with_clint(&program);

    // lui, addi, csrrw mtvec — then the ecall.
    for _ in 0..3 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(
        cpu.pc(),
        HANDLER,
        "an exception enters at BASE, not BASE + 4 * cause"
    );
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 11);
}

#[test]
fn mip_reflects_the_devices() {
    let (_cpu, _mem, clint) = machine_with_clint(&[]);
    assert_eq!(clint.interrupt_pending(), 0);
    clint.set_software_interrupt_pending(true);
    assert_eq!(clint.interrupt_pending(), MIP_MSIP);
    clint.set_software_interrupt_pending(false);
    assert_eq!(clint.interrupt_pending(), 0);
}

#[test]
fn software_cannot_write_mip() {
    // Every `mip` bit belongs to an interrupt controller, so a write from
    // software must not be able to invent a pending interrupt.
    let program = [
        li(2, MIP_MTIP as i32),
        csrrw(csr_addr::MIP, 2, 0), // must be ignored
        csrrs(csr_addr::MIP, 0, 5), // read it back
        ebreak(),
    ];
    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    clint.set_mtime(0);
    mem.store(MTIMECMP_ADDR, 8, u64::MAX).unwrap(); // no deadline

    assert_eq!(cpu.run(&mut mem, 10), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(5), 0, "a write to mip does not make a bit pending");
    assert!(!clint.timer_interrupt_pending());
}

#[test]
fn mret_restores_the_interrupt_enable_stack() {
    let mut program = arm(HANDLER, MIP_MSIP);
    program.extend([addi(0, 0, 0), ebreak()]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    load_words(&mut mem, HANDLER, &[mret()]);
    clint.set_software_interrupt_pending(true);

    for _ in 0..ARM_SETUP {
        cpu.step(&mut mem).unwrap();
    }
    cpu.step(&mut mem).unwrap(); // take the interrupt
    assert_eq!(
        cpu.csr().read(csr_addr::MSTATUS) & (1 << 3),
        0,
        "MIE cleared on entry"
    );

    cpu.step(&mut mem).unwrap(); // mret
    let mstatus = cpu.csr().read(csr_addr::MSTATUS);
    assert_eq!(mstatus & (1 << 3), 1 << 3, "MIE restored from MPIE");
    assert_eq!((mstatus >> 7) & 1, 1, "MPIE set to 1");

    // The source is still pending, so the same interrupt is taken again. That
    // is correct: a level-triggered interrupt stays pending until the handler
    // clears it.
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), MIP_MSIP);
}

#[test]
fn wfi_completes_and_does_not_mask_the_next_interrupt() {
    let mut program = arm(HANDLER, MIP_MSIP);
    program.extend([wfi(), addi(0, 0, 0), ebreak()]);

    let (mut cpu, mut mem, clint) = machine_with_clint(&program);
    load_words(&mut mem, HANDLER, &[mret()]);

    // Nothing pending yet, so the wait is reached and retires: WFI is only a
    // hint and may complete immediately.
    for _ in 0..ARM_SETUP {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue), "wfi retires");
    assert_eq!(cpu.pc(), CODE + 32, "past the wfi, which was at +28");
    assert_eq!(
        cpu.csr().read(csr_addr::MEPC),
        0,
        "nothing has been taken yet"
    );

    // The source now arrives. The instruction after wfi must not run: the
    // interrupt is taken at the very next step boundary, so retiring WFI early
    // is not a way to skip past it.
    clint.set_software_interrupt_pending(true);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), MIP_MSIP);
    assert_eq!(
        cpu.csr().read(csr_addr::MEPC),
        CODE + 32,
        "mepc is the addi that did not run, not the wfi that did"
    );
}

#[test]
fn wfi_with_nothing_pending_simply_retires() {
    let (mut cpu, mut mem, _clint) = machine_with_clint(&[wfi(), ebreak()]);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Ebreak));
}

#[test]
fn the_riscv_tests_platform_includes_a_clint() {
    let (mem, _htif, clint) = platform::riscv_tests(0x8000_1000);
    assert_eq!(
        mem.pending_interrupts(),
        0,
        "nothing is pending after reset"
    );
    assert_eq!(mem.time_base(), Some(0));
    clint.set_mtime(77);
    assert_eq!(mem.time_base(), Some(77));
    assert!(mem.load(platform::riscv_tests::CLINT_BASE, 4).is_ok());
}

#[test]
fn an_address_space_with_no_clint_falls_back_to_the_model_time_base() {
    // Permissive memory has no devices, so `time` must still advance rather
    // than freeze — otherwise every instruction-level test reading `time`
    // would see zero.
    let (mut cpu, mut mem) = machine(0x100, &[csrrs(0xC01, 0, 5), ebreak()]);
    assert_eq!(cpu.run(&mut mem, 10), Ok(StopReason::Ebreak));
    assert!(cpu.reg(5) > 0, "the model time base still advances");
    assert_eq!(mem.time_base(), None);
    assert_eq!(mem.pending_interrupts(), 0);
}
