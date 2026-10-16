//! Delegation, privilege-sensitive interrupt gates, CSR aliases, and nested traps.
mod common;

use chips::cpu::{Cpu, StepOutcome, Trap};
use chips::csr::{
    addr, Csr, MSTATUS_MIE, MSTATUS_MPIE, MSTATUS_MPP, MSTATUS_SIE, MSTATUS_SPIE, MSTATUS_SPP,
    MSTATUS_TSR,
};
use chips::isa::Privilege;
use chips::mem::{Device, Memory, Permissions, Region};
use chips::trace::{Record, StepResult, Trace};
use common::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CODE: u32 = 0x100;
const MACHINE: u32 = 0x200;
const SUPERVISOR: u32 = 0x300;
const SSIP: u32 = 1 << 1;
const STIP: u32 = 1 << 5;
const SEIP: u32 = 1 << 9;

fn setup(mode: Privilege, values: &[(u32, u32)], body: &[u32]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = machine(CODE, body);
    let mut words = Vec::new();
    for &(address, value) in values {
        words.extend(li32(1, value));
        words.push(csrrw(address, 1, 0));
    }
    load_words(&mut mem, 0x4000, &words);
    cpu.set_pc(0x4000);
    for _ in &words {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    cpu.set_pc(CODE);
    cpu.set_privilege(mode);
    (cpu, mem)
}

#[test]
fn exceptions_delegate_only_from_lower_privileges() {
    for mode in Privilege::ALL {
        for (instruction, cause, tval) in [
            (ecall(), 8 + mode.encoding(), 0),
            (0xffff_ffff, 2, 0xffff_ffff),
        ] {
            for delegated in [false, true] {
                let (mut cpu, mut mem) = setup(
                    mode,
                    &[
                        (addr::MTVEC, MACHINE | 1),
                        (addr::STVEC, SUPERVISOR | 1),
                        (addr::MEPC, 0x444),
                        (addr::MCAUSE, 99),
                        (addr::MTVAL, 0x555),
                        (addr::MEDELEG, if delegated { 1 << cause } else { 0 }),
                        (addr::MSTATUS, MSTATUS_SIE | MSTATUS_MIE | MSTATUS_MPP),
                    ],
                    &[instruction],
                );
                let old_status = cpu.csr().read(addr::MSTATUS);
                let retired = cpu.instret();
                let to_s = delegated && mode != Privilege::Machine;
                assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
                assert_eq!(cpu.pc(), if to_s { SUPERVISOR } else { MACHINE });
                assert_eq!(
                    cpu.privilege(),
                    if to_s {
                        Privilege::Supervisor
                    } else {
                        Privilege::Machine
                    }
                );
                assert_eq!(cpu.instret(), retired, "traps do not retire");
                if to_s {
                    assert_eq!(cpu.csr().read(addr::SEPC), CODE);
                    assert_eq!(cpu.csr().read(addr::SCAUSE), cause);
                    assert_eq!(cpu.csr().read(addr::STVAL), tval);
                    assert_eq!(cpu.csr().read(addr::MEPC), 0x444);
                    assert_eq!(cpu.csr().read(addr::MCAUSE), 99);
                    assert_eq!(cpu.csr().read(addr::MTVAL), 0x555);
                    let status = cpu.csr().read(addr::MSTATUS);
                    assert_eq!(status & MSTATUS_SIE, 0);
                    assert_ne!(status & MSTATUS_SPIE, 0);
                    assert_eq!(status & MSTATUS_SPP != 0, mode == Privilege::Supervisor);
                    assert_eq!(
                        status & (MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP),
                        old_status & (MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP)
                    );
                } else {
                    assert_eq!(cpu.csr().read(addr::MEPC), CODE);
                    assert_eq!(cpu.csr().read(addr::MCAUSE), cause);
                    assert_eq!(cpu.csr().read(addr::MTVAL), tval);
                    assert_eq!(cpu.csr().read(addr::SCAUSE), 0);
                }
            }
        }
    }
}

#[test]
fn a_missing_delegated_handler_does_not_fall_back_to_mtvec() {
    for (instruction, result) in [
        (ecall(), Ok(StepOutcome::Ecall)),
        (0xffff_ffff, Err(Trap::IllegalInstruction(0xffff_ffff))),
    ] {
        let (mut cpu, mut mem) = setup(
            Privilege::User,
            &[(addr::MTVEC, MACHINE), (addr::MEDELEG, (1 << 8) | (1 << 2))],
            &[instruction],
        );
        assert_eq!(cpu.step(&mut mem), result);
        assert_eq!(cpu.pc(), CODE);
        assert_eq!(cpu.csr().read(addr::MCAUSE), 0);
    }
    // A supervisor handler is sufficient even without a machine handler.
    let (mut cpu, mut mem) = setup(
        Privilege::User,
        &[(addr::STVEC, SUPERVISOR), (addr::MEDELEG, 1 << 8)],
        &[ecall()],
    );
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.pc(), SUPERVISOR);
}

#[test]
fn interrupt_gates_depend_on_target_and_current_privilege() {
    for cause in [1, 5, 9] {
        for mode in Privilege::ALL {
            for delegated in [false, true] {
                for enabled in [false, true] {
                    for global in [0, MSTATUS_SIE, MSTATUS_MIE, MSTATUS_SIE | MSTATUS_MIE] {
                        let bit = 1 << cause;
                        let (mut cpu, mut mem) = setup(
                            mode,
                            &[
                                (addr::MTVEC, MACHINE | 1),
                                (addr::STVEC, SUPERVISOR | 1),
                                (addr::MIDELEG, if delegated { bit } else { 0 }),
                                (addr::MIE, if enabled { bit } else { 0 }),
                                (addr::MIP, bit),
                                (addr::MSTATUS, global),
                            ],
                            &[addi(5, 0, 42)],
                        );
                        let deliver = enabled
                            && if delegated {
                                mode == Privilege::User
                                    || (mode == Privilege::Supervisor && global & MSTATUS_SIE != 0)
                            } else {
                                mode != Privilege::Machine || global & MSTATUS_MIE != 0
                            };
                        assert_eq!(cpu.step(&mut mem), Ok(if deliver { StepOutcome::TrapTaken } else { StepOutcome::Continue }),
                            "cause={cause} mode={mode:?} delegated={delegated} enabled={enabled} global={global}");
                        if deliver {
                            assert_eq!(cpu.reg(5), 0, "interrupted instruction did not run");
                            assert_eq!(
                                cpu.pc(),
                                (if delegated { SUPERVISOR } else { MACHINE }) + 4 * cause
                            );
                            assert_eq!(
                                cpu.csr().read(if delegated {
                                    addr::SCAUSE
                                } else {
                                    addr::MCAUSE
                                }),
                                0x8000_0000 | cause
                            );
                            assert_eq!(
                                cpu.csr()
                                    .read(if delegated { addr::SEPC } else { addr::MEPC }),
                                CODE
                            );
                            assert_eq!(
                                cpu.csr()
                                    .read(if delegated { addr::STVAL } else { addr::MTVAL }),
                                0
                            );
                        } else {
                            assert_eq!(cpu.reg(5), 42);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn machine_targets_preempt_supervisor_targets_and_ssi_preempts_sti() {
    // Machine sources are device-driven. Check their actual priority and the
    // rule that higher-privilege interrupts ignore MIE while running in S/U.
    for mode in Privilege::ALL {
        for global in [0, MSTATUS_MIE] {
            for (mask, cause) in [(0x888, 11), (0x88, 3), (0x80, 7)] {
                let (mut cpu, _) = setup(
                    mode,
                    &[
                        (addr::MTVEC, MACHINE),
                        (addr::MIE, 0x888),
                        (addr::MSTATUS, global),
                    ],
                    &[addi(0, 0, 0)],
                );
                let signal = Rc::new(Signal(Cell::new(mask)));
                let mut mem = Memory::from_regions(vec![
                    Region::ram(0, 0x1000, Permissions::READ_WRITE),
                    Region::shared_device(0x1000, 4, Permissions::READ_WRITE, signal),
                ]);
                mem.poke_u32(CODE, addi(0, 0, 0));
                if mode != Privilege::Machine || global != 0 {
                    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
                    assert_eq!(cpu.csr().read(addr::MCAUSE), 0x8000_0000 | cause);
                } else {
                    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
                }
            }
        }
    }
    for (delegation, expected_mode, expected_cause) in [
        (SSIP | STIP, Privilege::Machine, 9),
        (SSIP | STIP | SEIP, Privilege::Supervisor, 9),
    ] {
        let (mut cpu, mut mem) = setup(
            Privilege::User,
            &[
                (addr::MTVEC, MACHINE),
                (addr::STVEC, SUPERVISOR),
                (addr::MIDELEG, delegation),
                (addr::MIE, SSIP | STIP | SEIP),
                (addr::MIP, SSIP | STIP | SEIP),
            ],
            &[addi(0, 0, 0)],
        );
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
        assert_eq!(cpu.privilege(), expected_mode);
        assert_eq!(
            cpu.csr().read(if expected_mode == Privilege::Machine {
                addr::MCAUSE
            } else {
                addr::SCAUSE
            }),
            0x8000_0000 | expected_cause
        );
    }
    let (mut cpu, mut mem) = setup(
        Privilege::User,
        &[
            (addr::STVEC, SUPERVISOR),
            (addr::MIDELEG, SSIP | STIP),
            (addr::MIE, SSIP | STIP),
            (addr::MIP, SSIP | STIP),
        ],
        &[addi(0, 0, 0)],
    );
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.csr().read(addr::SCAUSE), 0x8000_0001);
}

#[test]
fn delegation_masks_and_alias_writes_preserve_hidden_state() {
    let mut c = Csr::new();
    c.write(addr::MEDELEG, u32::MAX);
    c.write(addr::MIDELEG, u32::MAX);
    assert_eq!(
        c.read(addr::MEDELEG),
        0x3ff,
        "M ECALL and unsupported causes cannot delegate"
    );
    assert_eq!(
        c.read(addr::MIDELEG),
        0x222,
        "machine sources cannot delegate"
    );
    c.write(addr::MIE, 0xaaa);
    c.write(addr::MIP, 0x222);
    c.write(addr::MIDELEG, SSIP);
    assert_eq!(c.read(addr::SIE), SSIP);
    assert_eq!(c.read(addr::SIP), SSIP);
    c.write(addr::SIE, 0);
    c.write(addr::SIP, 0);
    assert_eq!(c.read(addr::MIE), 0xaa8);
    assert_eq!(c.read(addr::MIP), STIP | SEIP);
    c.write(addr::MIDELEG, 0);
    c.write(addr::SIE, u32::MAX);
    c.write(addr::SIP, u32::MAX);
    assert_eq!(c.read(addr::SIE), 0);
    assert_eq!(c.read(addr::SIP), 0);
    assert_eq!(c.read(addr::MIE), 0xaa8);
    assert_eq!(c.read(addr::MIP), STIP | SEIP);
    c.write(addr::MIDELEG, 0x222);
    c.write(addr::SIP, u32::MAX);
    assert_eq!(c.read(addr::MIP), 0x222);
    c.write(addr::SIP, 0);
    assert_eq!(c.read(addr::MIP), STIP | SEIP, "S can clear only SSIP");
}

#[test]
fn supervisor_can_clear_ssip_then_sret_resumes_user() {
    let (mut cpu, mut mem) = setup(
        Privilege::User,
        &[
            (addr::STVEC, SUPERVISOR),
            (addr::MIDELEG, SSIP),
            (addr::MIE, SSIP),
            (addr::MIP, SSIP),
            (addr::MSTATUS, MSTATUS_SIE),
        ],
        &[addi(5, 0, 42)],
    );
    load_words(&mut mem, SUPERVISOR, &[csrrwi(addr::SIP, 0, 0), sret()]);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.privilege(), Privilege::User);
    assert_eq!(cpu.pc(), CODE);
    assert_eq!(cpu.csr().read(addr::MSTATUS) & MSTATUS_SPP, 0);
    assert_ne!(cpu.csr().read(addr::MSTATUS) & MSTATUS_SIE, 0);
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.reg(5), 42);
}

#[test]
fn nested_supervisor_and_machine_traps_preserve_both_return_paths() {
    let (mut cpu, mut mem) = setup(
        Privilege::User,
        &[
            (addr::MTVEC, MACHINE),
            (addr::STVEC, SUPERVISOR),
            (addr::MEDELEG, 1 << 8),
            (addr::MSTATUS, MSTATUS_SIE),
        ],
        &[ecall()],
    );
    load_words(&mut mem, SUPERVISOR, &[ecall(), sret()]);
    load_words(
        &mut mem,
        MACHINE,
        &[
            csrrs(addr::MEPC, 0, 5),
            addi(5, 5, 4),
            csrrw(addr::MEPC, 5, 0),
            mret(),
        ],
    );
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(addr::MEPC), SUPERVISOR);
    assert_eq!(cpu.csr().read(addr::MCAUSE), 9);
    assert_eq!(cpu.csr().read(addr::SEPC), CODE);
    for _ in 0..4 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.privilege(), Privilege::Supervisor);
    assert_eq!(cpu.pc(), SUPERVISOR + 4);
    assert_eq!(cpu.csr().read(addr::MSTATUS) & MSTATUS_MPP, 0);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.privilege(), Privilege::User);
    assert_eq!(cpu.pc(), CODE);
}

#[test]
fn tsr_intercepts_sret_only_in_supervisor_mode() {
    for mode in Privilege::ALL {
        for tsr in [0, MSTATUS_TSR] {
            let (mut cpu, mut mem) = setup(
                mode,
                &[
                    (addr::MTVEC, MACHINE),
                    (addr::SEPC, 0x500),
                    (addr::MSTATUS, tsr | MSTATUS_SPP | MSTATUS_SPIE),
                ],
                &[sret()],
            );
            let illegal = mode == Privilege::User || (mode == Privilege::Supervisor && tsr != 0);
            assert_eq!(
                cpu.step(&mut mem),
                Ok(if illegal {
                    StepOutcome::TrapTaken
                } else {
                    StepOutcome::Continue
                })
            );
            if illegal {
                assert_eq!(cpu.csr().read(addr::MCAUSE), 2);
                assert_eq!(cpu.csr().read(addr::MTVAL), sret());
            } else {
                assert_eq!(cpu.pc(), 0x500);
                assert_eq!(cpu.privilege(), Privilege::Supervisor);
                assert_eq!(cpu.csr().read(addr::MSTATUS) & MSTATUS_SPP, 0);
                assert_ne!(cpu.csr().read(addr::MSTATUS) & MSTATUS_SIE, 0);
            }
        }
    }
}

#[test]
fn bare_capabilities_and_sfence_encodings_are_explicit() {
    let mut c = Csr::new();
    c.write(addr::MSTATUS, u32::MAX);
    let unsupported = (1 << 18) | (1 << 19) | (1 << 20); // SUM, MXR, TVM
    assert_eq!(c.read(addr::MSTATUS) & unsupported, 0);
    assert_eq!(c.read(addr::SSTATUS) & unsupported, 0);
    c.write(addr::SATP, 0x8000_1234); // Sv32, not an RV64 mode field
    assert_eq!(c.read(addr::SATP), 0);
    for mode in Privilege::ALL {
        for instruction in [0x1200_0073, 0x13f5_0073, 0x1300_0073, 0x1200_00f3] {
            let (mut cpu, mut mem) = setup(mode, &[(addr::MTVEC, MACHINE)], &[instruction]);
            assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
            assert_eq!(cpu.csr().read(addr::MCAUSE), 2);
            assert_eq!(cpu.csr().read(addr::MTVAL), instruction);
        }
        let (mut cpu, mut mem) = setup(mode, &[], &[csrrs(addr::SATP, 0, 5)]);
        if mode == Privilege::User {
            assert!(matches!(
                cpu.step(&mut mem),
                Err(Trap::IllegalInstruction(_))
            ));
        } else {
            assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
            assert_eq!(cpu.reg(5), 0);
        }
    }
}

#[derive(Debug)]
struct Signal(Cell<u32>);
impl Device for Signal {
    fn read(&self, _: u32, _: u32) -> u64 {
        0
    }
    fn write(&self, _: u32, _: u32, _: u64) {}
    fn interrupt_pending(&self) -> u32 {
        self.0.get()
    }
}

#[test]
fn csr_rmw_does_not_latch_external_pending_signals() {
    for instruction in [
        csrrs(addr::MIP, 1, 5),
        csrrc(addr::MIP, 1, 5),
        csrrs(addr::SIP, 1, 5),
    ] {
        let (mut cpu, mut mem) = setup(
            Privilege::Machine,
            &[(addr::MIDELEG, SEIP)],
            &[li(1, 2), instruction, csrrs(addr::MIP, 0, 6)],
        );
        let signal = Rc::new(Signal(Cell::new(SEIP)));
        mem = {
            let mut mapped = Memory::from_regions(vec![
                Region::ram(0, 0x1000, Permissions::READ_WRITE),
                Region::shared_device(0x1000, 4, Permissions::READ_WRITE, Rc::clone(&signal)),
            ]);
            mapped
                .load_image(0, &(0..0x1000).map(|i| mem.peek_u8(i)).collect::<Vec<_>>())
                .unwrap();
            mapped
        };
        cpu.step(&mut mem).unwrap();
        cpu.step(&mut mem).unwrap();
        assert_eq!(cpu.reg(5) & SEIP, SEIP, "CSR read sees device signal");
        signal.0.set(0);
        cpu.step(&mut mem).unwrap();
        assert_eq!(
            cpu.reg(6) & SEIP,
            0,
            "device signal never becomes software pending state"
        );
    }
}

#[derive(Clone)]
struct Records(Rc<RefCell<Vec<Record>>>);
impl Trace for Records {
    fn record(&mut self, record: &Record) {
        self.0.borrow_mut().push(record.clone());
    }
}

#[test]
fn supervisor_trace_uses_scause_and_records_supervisor_trap_csrs() {
    let (mut cpu, mut mem) = setup(
        Privilege::User,
        &[
            (addr::STVEC, SUPERVISOR),
            (addr::MEDELEG, 1 << 2),
            (addr::MCAUSE, 99),
            (addr::MTVAL, 77),
        ],
        &[0xffff_ffff],
    );
    let records = Records(Rc::default());
    cpu.set_trace(Some(Box::new(records.clone())));
    cpu.step(&mut mem).unwrap();
    let records = records.0.borrow();
    assert_eq!(
        records[0].outcome,
        StepResult::Trap {
            cause: 2,
            tval: 0xffff_ffff
        }
    );
    assert!(records[0]
        .to_line()
        .contains("c141=00000100 c142=00000002 c143=ffffffff"));
    assert!(!records[0].to_line().contains("c341="));
}
