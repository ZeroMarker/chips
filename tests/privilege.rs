//! Privilege levels: the M/S/U transitions, `ecall` causes, the return
//! instructions, and CSR access control.
//!
//! The machine resets into M mode and can only leave it by trapping and then
//! executing `mret`, so most of these tests install a handler, drop privilege,
//! and check what the less privileged code is and is not allowed to do.

mod common;

use chips::cpu::{Cpu, StepOutcome};
use chips::csr::addr as csr_addr;
use chips::isa::Privilege;
use common::*;

const CODE: u32 = 0x100;
/// Where the body under test begins. [`enter`] installs `mtvec` as part of the
/// drop, so the body is the first thing at `CODE`.
const BODY: u32 = CODE;
const HANDLER: u32 = 0x180;

/// `sfence.vma x0, x0` — the funct12 is 0x120 and it takes two register operands,
/// so it is not covered by the `no_operands` check the others use.
const fn sfence_vma() -> u32 {
    (0x120 << 20) | SYSTEM
}

/// A CPU running `words` at `CODE`, with a one-instruction `mret` handler at
/// [`HANDLER`].
fn machine_with_handler(words: &[u32]) -> (Cpu, chips::Memory) {
    let (cpu, mut mem) = machine(CODE, words);
    mem.poke_u32(HANDLER, mret());
    (cpu, mem)
}

/// Drop to `mode` the way firmware would: install `mtvec`, set the `xPP` field
/// and `mepc`, then `mret`.
///
/// The whole sequence runs out of a scratch area, so the body at [`BODY`] is the
/// first thing the hart reaches at the new privilege — and `mtvec` is genuinely
/// installed by the time the body runs, which matters for the tests that expect
/// a trap to be delivered.
fn enter(cpu: &mut Cpu, mode: Privilege, target: u32) {
    let [h1, h2] = li32(1, HANDLER);
    let [m1, m2] = li32(2, mode.encoding() << 11);
    let [t1, t2] = li32(3, target);
    let words = [
        h1,
        h2, // x1 = HANDLER
        m1,
        m2, // x2 = mode << 11
        t1,
        t2,                             // x3 = target
        csrrw(csr_addr::MTVEC, 1, 0),   // mtvec = HANDLER
        csrrw(csr_addr::MSTATUS, 2, 0), // mstatus = x2
        csrrw(csr_addr::MEPC, 3, 0),    // mepc = target
        mret(),
    ];
    let mut mem = chips::Memory::permissive();
    load_words(&mut mem, 0x4000, &words);
    cpu.set_pc(0x4000);
    for _ in 0..words.len() {
        cpu.step(&mut mem).expect("the drop must not trap");
    }
    assert_eq!(cpu.pc(), target);
    assert_eq!(cpu.privilege(), mode);
}

/// Step one instruction that must be an illegal instruction.
///
/// These tests run with `mtvec` installed, so the exception is *delivered* to
/// the handler rather than returned to the caller. The distinction shows up in
/// the trap CSRs, which is also the more useful assertion: it checks the cause
/// and that the offending encoding is reported as `mtval`.
fn assert_illegal(cpu: &mut Cpu, mem: &mut chips::Memory, inst: u32) {
    assert_eq!(cpu.step(mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(
        cpu.csr().read(csr_addr::MCAUSE),
        2,
        "illegal instruction, encoding 0x{inst:08x}"
    );
    assert_eq!(
        cpu.csr().read(csr_addr::MTVAL),
        inst,
        "mtval is the offending encoding"
    );
}

#[test]
fn the_hart_resets_into_machine_mode() {
    let (cpu, _mem) = machine(CODE, &[ebreak()]);
    assert_eq!(cpu.privilege(), Privilege::Machine);
}

#[test]
fn mret_can_return_to_each_lower_mode() {
    for mode in [Privilege::Supervisor, Privilege::User] {
        let (mut cpu, mem) = machine_with_handler(&[ebreak()]);
        enter(&mut cpu, mode, BODY);
        assert_eq!(cpu.privilege(), mode, "dropped to {}", mode.name());
        let _ = mem;
    }
}

#[test]
fn mret_from_machine_mode_returns_to_machine_mode() {
    let (mut cpu, _mem) = machine_with_handler(&[ebreak()]);
    enter(&mut cpu, Privilege::Machine, BODY);
    assert_eq!(cpu.privilege(), Privilege::Machine);
}

#[test]
fn a_reserved_mpp_encoding_legalizes_to_user_mode() {
    // MPP = 2 is reserved. The architecture says to read back the least
    // privileged supported mode, not to trap.
    let (mut cpu, _mem) = machine_with_handler(&[ebreak()]);
    let mut mem = chips::Memory::permissive();
    // MPP = 0b10 is the reserved encoding. 2 << 11 is 0x1000, which needs both
    // halves of an lui/addi pair.
    let [h1, h2] = li32(1, HANDLER);
    let [m1, m2] = li32(2, 2 << 11);
    let [t1, t2] = li32(3, BODY);
    let words = [
        h1,
        h2,
        m1,
        m2,
        t1,
        t2,
        csrrw(csr_addr::MTVEC, 1, 0),
        csrrw(csr_addr::MSTATUS, 2, 0),
        csrrw(csr_addr::MEPC, 3, 0),
        mret(),
    ];
    load_words(&mut mem, 0x4000, &words);
    cpu.set_pc(0x4000);
    for _ in 0..words.len() {
        cpu.step(&mut mem).unwrap();
    }
    assert_eq!(cpu.pc(), BODY);
    assert_eq!(cpu.privilege(), Privilege::User);
}

#[test]
fn ecall_reports_the_mode_it_came_from() {
    // Cause 11 from M, 9 from S, 8 from U. This is the observable difference
    // between the three modes, so it is worth pinning down exactly.
    for (mode, cause) in [
        (Privilege::Machine, 11),
        (Privilege::Supervisor, 9),
        (Privilege::User, 8),
    ] {
        let (mut cpu, mut mem) = machine_with_handler(&[ecall()]);
        enter(&mut cpu, mode, BODY);
        assert_eq!(cpu.privilege(), mode);

        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
        assert_eq!(
            cpu.csr().read(csr_addr::MCAUSE),
            cause,
            "ecall from {} is cause {cause}",
            mode.name()
        );
        assert_eq!(
            cpu.privilege(),
            Privilege::Machine,
            "a trap raises privilege"
        );
    }
}

#[test]
fn a_trap_from_a_lower_mode_records_it_in_mpp() {
    let (mut cpu, mut mem) = machine_with_handler(&[ecall()]);
    enter(&mut cpu, Privilege::User, BODY);

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    let mpp = (cpu.csr().read(csr_addr::MSTATUS) >> 11) & 0b11;
    assert_eq!(
        Privilege::from_encoding(mpp),
        Some(Privilege::User),
        "MPP remembers where the trap came from so mret can go back"
    );

    // mret from the handler returns to the ecall, still in user mode.
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.pc(), BODY);
    assert_eq!(cpu.privilege(), Privilege::User);
}

#[test]
fn mret_from_a_lower_mode_is_illegal() {
    let (mut cpu, mut mem) = machine_with_handler(&[mret()]);
    enter(&mut cpu, Privilege::User, BODY);

    assert_illegal(&mut cpu, &mut mem, mret());
}

#[test]
fn sret_returns_to_the_mode_spp_names() {
    // SPP = 0 is user, SPP = 1 is supervisor. sret is the way an S-mode handler
    // resumes user code, and it cannot express "return to M".
    for (spp, expected) in [(0u32, Privilege::User), (1, Privilege::Supervisor)] {
        let (mut cpu, mut mem) = machine_with_handler(&[sret()]);
        enter(&mut cpu, Privilege::Supervisor, BODY);
        cpu.set_pc(CODE);

        // Set SPP and sepc, then sret. SPP goes through `sstatus`, because
        // S-mode may not write `mstatus` — using the alias is also what a real
        // kernel would do.
        let [hi, lo] = li32(2, spp << 8);
        let [t1, t2] = li32(3, CODE);
        let words = [
            hi,
            lo,
            t1,
            t2,
            csrrw(csr_addr::SSTATUS, 2, 0), // SPP = spp
            csrrw(csr_addr::SEPC, 3, 0),
            sret(),
        ];
        load_words(&mut mem, 0x5000, &words);
        cpu.set_pc(0x5000);
        for _ in 0..words.len() {
            cpu.step(&mut mem).unwrap();
        }
        assert_eq!(cpu.privilege(), expected, "SPP = {spp}");
    }
}

#[test]
fn sret_from_machine_mode_lands_in_user_mode() {
    // SPP cannot hold M, so a hart that has never set it returns to U.
    let (mut cpu, mut mem) = machine_with_handler(&[sret()]);
    enter(&mut cpu, Privilege::Machine, BODY);
    cpu.set_pc(BODY);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.privilege(), Privilege::User);
}

#[test]
fn sret_from_user_mode_is_illegal() {
    let (mut cpu, mut mem) = machine_with_handler(&[sret()]);
    enter(&mut cpu, Privilege::User, BODY);
    assert_illegal(&mut cpu, &mut mem, sret());
}

#[test]
fn sret_sets_spp_to_supervisor_so_it_cannot_climb() {
    // After sret, SPP is S. A second sret from the mode it landed in therefore
    // returns to S again, not to something higher.
    let (mut cpu, mut mem) = machine_with_handler(&[sret()]);
    enter(&mut cpu, Privilege::Supervisor, BODY);
    load_words(&mut mem, BODY, &[sret()]);
    cpu.set_pc(BODY);

    let mut mem2 = mem;
    cpu.step(&mut mem2).unwrap();
    assert_eq!(cpu.privilege(), Privilege::User, "SPP was 0");
    let spp = (cpu.csr().read(csr_addr::MSTATUS) >> 8) & 1;
    assert_eq!(spp, 1, "sret leaves SPP = S");
}

#[test]
fn sfence_vma_is_legal_at_s_and_above_only() {
    let (mut cpu, mut mem) = machine_with_handler(&[sfence_vma()]);

    // Legal from machine mode: it retires.
    enter(&mut cpu, Privilege::Machine, BODY);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.pc(), BODY + 4);

    // Legal from supervisor mode.
    let (mut cpu, mut mem) = machine_with_handler(&[sfence_vma()]);
    enter(&mut cpu, Privilege::Supervisor, BODY);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));

    // Illegal from user mode.
    let (mut cpu, mut mem) = machine_with_handler(&[sfence_vma()]);
    enter(&mut cpu, Privilege::User, BODY);
    assert_illegal(&mut cpu, &mut mem, sfence_vma());
}

#[test]
fn wfi_is_legal_in_every_mode() {
    for mode in Privilege::ALL {
        let (mut cpu, mut mem) = machine_with_handler(&[wfi()]);
        enter(&mut cpu, mode, BODY);
        assert_eq!(
            cpu.step(&mut mem),
            Ok(StepOutcome::Continue),
            "wfi from {}",
            mode.name()
        );
    }
}

#[test]
fn a_machine_register_is_invisible_from_a_lower_mode() {
    // Not merely unwritable: reading it is an illegal instruction, so user code
    // cannot observe machine state at all.
    for mode in [Privilege::Supervisor, Privilege::User] {
        let (mut cpu, mut mem) = machine_with_handler(&[csrrs(csr_addr::MSTATUS, 0, 5)]);
        enter(&mut cpu, mode, BODY);
        assert_illegal(&mut cpu, &mut mem, csrrs(csr_addr::MSTATUS, 0, 5));
        assert_eq!(cpu.reg(5), 0, "nothing was written");
    }
}

#[test]
fn a_supervisor_register_is_invisible_from_user_mode() {
    let inst = csrrs(csr_addr::SSTATUS, 0, 5);
    let (mut cpu, mut mem) = machine_with_handler(&[inst]);
    enter(&mut cpu, Privilege::User, BODY);
    assert_illegal(&mut cpu, &mut mem, inst);
}

#[test]
fn unprivileged_counters_are_readable_from_every_mode() {
    // The one deliberate exception: the shadow counters exist to be read by
    // unprivileged code, and the machine aliases stay out of reach.
    for mode in Privilege::ALL {
        let (mut cpu, mut mem) = machine_with_handler(&[csrrs(csr_addr::CYCLE, 0, 5)]);
        enter(&mut cpu, mode, BODY);
        assert_eq!(
            cpu.step(&mut mem),
            Ok(StepOutcome::Continue),
            "cycle from {}",
            mode.name()
        );
        assert!(cpu.reg(5) > 0);
    }
}

#[test]
fn the_machine_counter_aliases_stay_machine_only() {
    // Reading `cycle` is fine everywhere; `mcycle` is not, or a user program
    // could simply use the writable form instead.
    let inst = csrrs(csr_addr::MCYCLE, 0, 5);
    for mode in [Privilege::Supervisor, Privilege::User] {
        let (mut cpu, mut mem) = machine_with_handler(&[inst]);
        enter(&mut cpu, mode, BODY);
        assert_illegal(&mut cpu, &mut mem, inst);
    }
}

#[test]
fn sstatus_is_a_narrow_view_of_mstatus() {
    // SIE and SPIE are shared storage; MIE is not visible through sstatus, and
    // writing sstatus must not disturb it.
    let (mut cpu, _mem) = machine_with_handler(&[]);

    let words = [
        csrrwi(csr_addr::MSTATUS, 0b1010, 0), // set SIE (bit 1) and MIE (bit 3)
        csrrs(csr_addr::SSTATUS, 0, 5),       // read the supervisor view
        csrrs(csr_addr::MSTATUS, 0, 6),       // read the machine view
    ];
    let mut mem2 = chips::Memory::permissive();
    load_words(&mut mem2, 0x6000, &words);
    cpu.set_pc(0x6000);
    for _ in 0..words.len() {
        cpu.step(&mut mem2).unwrap();
    }

    let sstatus = cpu.reg(5);
    let mstatus = cpu.reg(6);
    assert_eq!(sstatus & (1 << 1), 1 << 1, "SIE is visible through sstatus");
    assert_eq!(sstatus & (1 << 3), 0, "MIE is not");
    assert_eq!(mstatus & (1 << 3), 1 << 3, "but it is there in mstatus");
}

#[test]
fn writing_sstatus_leaves_the_machine_fields_alone() {
    let (mut cpu, mut mem) = machine_with_handler(&[]);
    let words = [
        csrrwi(csr_addr::MSTATUS, 0b1010, 0), // SIE and MIE
        li(5, 0),                             // x5 = 0
        csrrw(csr_addr::SSTATUS, 5, 0),       // clear SIE and SPIE
        csrrs(csr_addr::MSTATUS, 0, 6),       // read back
    ];
    load_words(&mut mem, 0x6000, &words);
    cpu.set_pc(0x6000);
    for _ in 0..words.len() {
        cpu.step(&mut mem).unwrap();
    }
    assert_eq!(cpu.reg(6) & (1 << 1), 0, "SIE cleared");
    assert_eq!(
        cpu.reg(6) & (1 << 3),
        1 << 3,
        "MIE untouched by a supervisor write"
    );
}

#[test]
fn sie_and_mie_share_storage() {
    let (mut cpu, mut mem) = machine_with_handler(&[]);
    let words = [
        li(5, 0x80),                // x5 = MTIE
        csrrw(csr_addr::SIE, 5, 0), // set MTIE through sie
        csrrs(csr_addr::MIE, 0, 6), // read it through mie
        csrrs(csr_addr::MIE, 0, 7), // and again
    ];
    load_words(&mut mem, 0x6000, &words);
    cpu.set_pc(0x6000);
    for _ in 0..words.len() {
        cpu.step(&mut mem).unwrap();
    }
    assert_eq!(cpu.reg(6) & 0x80, 0x80, "MTIE visible through mie");
    // A pure read of mie with rs1 = x0 does not write, so the second read is
    // still there.
    assert_eq!(cpu.reg(7) & 0x80, 0x80);
}

#[test]
fn satp_legalizes_to_bare_because_there_is_no_translation() {
    // Writing Sv39 would otherwise leave a mode set that does nothing, and
    // software that checks satp before trusting a pointer would be misled.
    let (mut cpu, mut mem) = machine_with_handler(&[]);
    let words = [
        li32(5, 0x80D)[0],
        li32(5, 0x80D)[1],           // x5 = mode 8 (Sv39)
        csrrw(csr_addr::SATP, 5, 0), // attempt to enable translation
        csrrs(csr_addr::SATP, 0, 6), // read it back
    ];
    load_words(&mut mem, 0x6000, &words);
    cpu.set_pc(0x6000);
    for _ in 0..words.len() {
        cpu.step(&mut mem).unwrap();
    }
    assert_eq!(cpu.reg(6) & 0b1111, 0, "reads back as Bare");
}

#[test]
fn ecall_with_no_handler_still_halts_the_run() {
    // The mechanism bare-metal images use to stop must survive: with no mtvec,
    // an ecall ends the loop whatever mode it came from.
    for mode in [Privilege::Machine, Privilege::Supervisor, Privilege::User] {
        let (mut cpu, mut mem) = machine(CODE, &[ecall()]);
        cpu.set_privilege(mode);
        assert_eq!(
            cpu.step(&mut mem),
            Ok(StepOutcome::Ecall),
            "ecall from {}",
            mode.name()
        );
    }
}
