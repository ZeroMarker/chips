//! Native breakpoint triggers: capabilities, gates, priority and precise effects.
mod common;

use chips::cpu::{Cpu, StepOutcome, Trap};
use chips::csr::{addr, Csr, MSTATUS_MIE, MSTATUS_MPRV, MSTATUS_SIE};
use chips::isa::Privilege;
use chips::mem::{Access, Device, Memory, Permissions, Region};
use chips::trace::{Change, Record, StepResult, Trace};
use chips::triggers::Triggers;
use common::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CODE: u32 = 0x100;
const DATA: u32 = 0x500;
const HANDLER: u32 = 0x800;
const SUPERVISOR: u32 = 0x900;
const TYPE: u32 = 2 << 28;
const M: u32 = 1 << 6;
const S: u32 = 1 << 4;
const U: u32 = 1 << 3;

fn setup(mode: Privilege, values: &[(u32, u32)], words: &[u32]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = machine(CODE, words);
    permit_all_memory(&mut cpu);
    let mut prefix = Vec::new();
    for &(address, value) in values {
        prefix.extend(li32(1, value));
        prefix.push(csrrw(address, 1, 0));
    }
    load_words(&mut mem, 0x4000, &prefix);
    cpu.set_pc(0x4000);
    for _ in prefix {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    cpu.set_pc(CODE);
    cpu.set_privilege(mode);
    (cpu, mem)
}

fn instruction(access: Access) -> u32 {
    match access {
        Access::Fetch => addi(5, 0, 42),
        Access::Load => load(DATA as i32, 0, 2, 5),
        Access::Store => store(DATA as i32, 0, 0, 2),
    }
}

#[test]
fn two_trigger_banks_enumerate_and_preserve_independent_configuration() {
    let mut c = Csr::new();
    for index in 0..2 {
        c.write(addr::TSELECT, index);
        assert_eq!(c.read(addr::TSELECT), index);
        assert_eq!(c.read(addr::TDATA1), TYPE);
        c.write(addr::TDATA2, DATA + index * 4);
        c.write(addr::TDATA1, TYPE | M | (index + 1));
    }
    c.write(addr::TSELECT, 2);
    assert_ne!(c.read(addr::TSELECT), 2, "enumeration must stop");
    c.write(addr::TSELECT, u32::MAX);
    assert!(c.read(addr::TSELECT) < 2);
    for index in 0..2 {
        c.write(addr::TSELECT, index);
        assert_eq!(c.read(addr::TDATA1), TYPE | M | (index + 1));
        assert_eq!(c.read(addr::TDATA2), DATA + index * 4);
    }
}

#[test]
fn unsupported_fields_are_legalized_and_zero_disables_without_erasing_address() {
    let mut c = Csr::new();
    c.write(addr::TDATA2, u32::MAX);
    c.write(addr::TDATA1, TYPE | 0x0fff_ffff);
    assert_eq!(
        c.read(addr::TDATA1),
        TYPE | M | S | U | 7,
        "dmode/action/select/timing/chain/match/size/hit/maskmax are unsupported"
    );
    c.write(addr::TDATA3, u32::MAX);
    assert_eq!(c.read(addr::TDATA3), 0);
    assert_eq!(c.read(addr::TINFO), 0x0100_0004);
    c.write(addr::TINFO, 0);
    assert_eq!(c.read(addr::TINFO), 0x0100_0004);
    for value in [0, u32::MAX, (6 << 28) | M | 7] {
        c.write(addr::TDATA1, value);
        assert_eq!(c.read(addr::TDATA1), TYPE);
        assert_eq!(c.read(addr::TDATA2), u32::MAX);
        assert_eq!(
            c.triggers()
                .matched(Privilege::Machine, u32::MAX, 1, Access::Load),
            None
        );
    }
    assert!(
        !Csr::exists(0x7a5),
        "optional tcontrol is absent: use MIE/SIE gating"
    );
}

#[test]
fn access_and_privilege_bits_are_independent_and_all_accessed_bytes_match() {
    for (trigger_mode, bit) in [
        (Privilege::Machine, M),
        (Privilege::Supervisor, S),
        (Privilege::User, U),
    ] {
        for (trigger_access, operation) in
            [(Access::Fetch, 4), (Access::Load, 1), (Access::Store, 2)]
        {
            let mut triggers = Triggers::default();
            triggers.write(1, TYPE | bit | operation);
            triggers.write(2, DATA + 3);
            for mode in Privilege::ALL {
                for access in [Access::Fetch, Access::Load, Access::Store] {
                    assert_eq!(
                        triggers.matched(mode, DATA, 4, access),
                        if mode == trigger_mode && access == trigger_access {
                            Some(DATA + 3)
                        } else {
                            None
                        }
                    );
                }
            }
            assert_eq!(
                triggers.matched(trigger_mode, DATA, 3, trigger_access),
                None
            );
            assert_eq!(
                triggers.matched(trigger_mode, DATA + 4, 4, trigger_access),
                None
            );
        }
    }
    let mut triggers = Triggers::default();
    triggers.write(1, TYPE | M | 1);
    triggers.write(2, 0);
    assert_eq!(
        triggers.matched(Privilege::Machine, u32::MAX - 1, 4, Access::Load),
        None,
        "a cross-address-space access cannot wrap and trigger at zero"
    );
    assert_eq!(
        triggers.matched(Privilege::Machine, 0, 0, Access::Load),
        None
    );
}

#[derive(Clone, Default)]
struct Records(Rc<RefCell<Vec<Record>>>);
impl Trace for Records {
    fn record(&mut self, record: &Record) {
        self.0.borrow_mut().push(record.clone());
    }
}

#[test]
fn cpu_triggers_are_precise_and_do_not_retire_or_modify_data() {
    for (access, operation) in [(Access::Fetch, 4), (Access::Load, 1), (Access::Store, 2)] {
        let target = if access == Access::Fetch { CODE } else { DATA };
        let word = instruction(access);
        let (mut cpu, mut mem) = setup(
            Privilege::Machine,
            &[
                (addr::MTVEC, HANDLER),
                (addr::TDATA2, target),
                (addr::TDATA1, TYPE | M | operation),
                (addr::MSTATUS, MSTATUS_MIE),
            ],
            &[word],
        );
        mem.poke_u32(DATA, 0x1234_5678);
        let records = Records::default();
        cpu.set_trace(Some(Box::new(records.clone())));
        let retired = cpu.instret();
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
        assert_eq!(cpu.pc(), HANDLER);
        assert_eq!(cpu.csr().read(addr::MEPC), CODE);
        assert_eq!(cpu.csr().read(addr::MCAUSE), 3);
        assert_eq!(cpu.csr().read(addr::MTVAL), target);
        assert_eq!(cpu.instret(), retired);
        assert_eq!(cpu.reg(5), 0);
        assert_eq!(mem.peek_u32(DATA), 0x1234_5678);
        let records = records.0.borrow();
        assert_eq!(
            records[0].outcome,
            StepResult::Trap {
                cause: 3,
                tval: target
            }
        );
        assert_eq!(
            records[0].instruction,
            if access == Access::Fetch { 0 } else { word }
        );
        assert!(!records[0]
            .changes
            .iter()
            .any(|c| matches!(c, Change::Register { .. } | Change::Memory { .. })));
    }
}

#[test]
fn native_gates_and_breakpoint_delegation_cover_all_modes() {
    for mode in Privilege::ALL {
        for delegated in [false, true] {
            for global in [0, MSTATUS_MIE, MSTATUS_SIE, MSTATUS_MIE | MSTATUS_SIE] {
                let (mut cpu, mut mem) = setup(
                    mode,
                    &[
                        (addr::MTVEC, HANDLER),
                        (addr::STVEC, SUPERVISOR),
                        (addr::MEDELEG, if delegated { 1 << 3 } else { 0 }),
                        (addr::TDATA2, CODE),
                        (addr::TDATA1, TYPE | M | S | U | 4),
                        (addr::MSTATUS, global),
                    ],
                    &[addi(5, 0, 42)],
                );
                let fire = match mode {
                    Privilege::Machine => global & MSTATUS_MIE != 0,
                    Privilege::Supervisor => !delegated || global & MSTATUS_SIE != 0,
                    Privilege::User => true,
                };
                assert_eq!(
                    cpu.step(&mut mem),
                    Ok(if fire {
                        StepOutcome::TrapTaken
                    } else {
                        StepOutcome::Continue
                    }),
                    "mode={mode:?}, delegated={delegated}, status={global}"
                );
                if fire {
                    let to_s = delegated && mode != Privilege::Machine;
                    assert_eq!(cpu.pc(), if to_s { SUPERVISOR } else { HANDLER });
                    assert_eq!(
                        cpu.csr().read(if to_s { addr::SEPC } else { addr::MEPC }),
                        CODE
                    );
                    assert_eq!(
                        cpu.csr()
                            .read(if to_s { addr::SCAUSE } else { addr::MCAUSE }),
                        3
                    );
                    assert_eq!(
                        cpu.csr().read(if to_s { addr::STVAL } else { addr::MTVAL }),
                        CODE
                    );
                } else {
                    assert_eq!(cpu.reg(5), 42);
                }
            }
        }
    }
}

#[test]
fn trap_entry_masks_native_triggers_and_return_rearms_them() {
    for supervisor in [false, true] {
        let (mut cpu, mut mem) = setup(
            if supervisor {
                Privilege::Supervisor
            } else {
                Privilege::Machine
            },
            &[
                (addr::MTVEC, HANDLER),
                (addr::STVEC, SUPERVISOR),
                (addr::MEDELEG, if supervisor { 1 << 3 } else { 0 }),
                (addr::TDATA2, DATA),
                (addr::TDATA1, TYPE | M | S | 1),
                (addr::MSTATUS, MSTATUS_MIE | MSTATUS_SIE),
            ],
            &[instruction(Access::Load)],
        );
        load_words(
            &mut mem,
            if supervisor { SUPERVISOR } else { HANDLER },
            &[
                instruction(Access::Load),
                if supervisor { sret() } else { mret() },
            ],
        );
        mem.poke_u32(DATA, 42);
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
        assert_eq!(
            cpu.step(&mut mem),
            Ok(StepOutcome::Continue),
            "same data read inside handler must not retrigger"
        );
        assert_eq!(cpu.reg(5), 42);
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
        assert_eq!(cpu.pc(), CODE);
        assert_eq!(
            cpu.step(&mut mem),
            Ok(StepOutcome::TrapTaken),
            "xRET restores the native gate"
        );
    }
}

#[test]
fn address_trigger_precedes_pmp_fault_but_illegal_encoding_precedes_data_trigger() {
    let (mut cpu, mut mem) = setup(
        Privilege::Machine,
        &[
            (addr::PMPADDR0, CODE >> 2),
            (addr::PMPCFG0, 0x90), // locked NA4 with no X
            (addr::TDATA2, CODE),
            (addr::TDATA1, TYPE | M | 4),
            (addr::MSTATUS, MSTATUS_MIE),
        ],
        &[addi(5, 0, 42)],
    );
    assert_eq!(
        cpu.step(&mut mem),
        Err(Trap::Trigger(CODE)),
        "execute address breakpoint outranks access fault"
    );
    for access in [Access::Load, Access::Store] {
        let operation = if access == Access::Load { 1 } else { 2 };
        let (mut cpu, mut mem) = setup(
            Privilege::Machine,
            &[
                (addr::PMPADDR0, DATA >> 2),
                (addr::PMPCFG0, 0x90),
                (addr::TDATA2, DATA),
                (addr::TDATA1, TYPE | M | operation),
                (addr::MSTATUS, MSTATUS_MIE),
            ],
            &[instruction(access)],
        );
        assert_eq!(cpu.step(&mut mem), Err(Trap::Trigger(DATA)));
        let invalid = if access == Access::Load {
            load(DATA as i32, 0, 3, 5)
        } else {
            store(DATA as i32, 0, 0, 3)
        };
        mem.poke_u32(CODE, invalid);
        assert_eq!(cpu.step(&mut mem), Err(Trap::IllegalInstruction(invalid)));
    }
}

#[derive(Debug, Default)]
struct DeviceCalls(Cell<u32>);
impl Device for DeviceCalls {
    fn read(&self, _: u32, _: u32) -> u64 {
        self.0.set(self.0.get() + 1);
        42
    }
    fn write(&self, _: u32, _: u32, _: u64) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn execute_and_data_triggers_prevent_mmio_side_effects() {
    for (access, operation) in [(Access::Fetch, 4), (Access::Load, 1), (Access::Store, 2)] {
        let target = if access == Access::Fetch { CODE } else { DATA };
        let (mut cpu, _) = setup(
            Privilege::Machine,
            &[
                (addr::TDATA2, target),
                (addr::TDATA1, TYPE | M | operation),
                (addr::MSTATUS, MSTATUS_MIE),
            ],
            &[instruction(access)],
        );
        let calls = Rc::new(DeviceCalls::default());
        let mut mem = Memory::from_regions(if access == Access::Fetch {
            vec![Region::shared_device(
                CODE,
                4,
                Permissions::READ_WRITE,
                calls.clone(),
            )]
        } else {
            vec![
                Region::ram(0, DATA.into(), Permissions::READ_WRITE),
                Region::shared_device(DATA, 4, Permissions::READ_WRITE, calls.clone()),
            ]
        });
        if access != Access::Fetch {
            load_words(&mut mem, CODE, &[instruction(access)]);
        }
        assert_eq!(cpu.step(&mut mem), Err(Trap::Trigger(target)));
        assert_eq!(calls.0.get(), 0);
    }
}

#[test]
fn both_triggers_remain_active_regardless_of_selected_bank() {
    let (mut cpu, mut mem) = setup(
        Privilege::Machine,
        &[
            (addr::TDATA2, CODE),
            (addr::TDATA1, TYPE | M | 4),
            (addr::TSELECT, 1),
            (addr::TDATA2, DATA),
            (addr::TDATA1, TYPE | M | 1),
            (addr::MSTATUS, MSTATUS_MIE),
        ],
        &[addi(5, 0, 42), instruction(Access::Load)],
    );
    assert_eq!(cpu.step(&mut mem), Err(Trap::Trigger(CODE)));
    cpu.set_pc(CODE + 4);
    assert_eq!(cpu.step(&mut mem), Err(Trap::Trigger(DATA)));
}

#[test]
fn trigger_csrs_stay_machine_only_and_missing_handler_is_not_a_halt() {
    for mode in [Privilege::Supervisor, Privilege::User] {
        for address in addr::TSELECT..=addr::TINFO {
            let word = csrrs(address, 0, 5);
            let (mut cpu, mut mem) = setup(mode, &[], &[word]);
            assert_eq!(cpu.step(&mut mem), Err(Trap::IllegalInstruction(word)));
        }
    }
    let (mut cpu, mut mem) = setup(
        Privilege::Machine,
        &[
            (addr::TDATA2, CODE),
            (addr::TDATA1, TYPE | M | 4),
            (addr::MSTATUS, MSTATUS_MIE),
        ],
        &[ebreak()],
    );
    assert_eq!(cpu.step(&mut mem), Err(Trap::Trigger(CODE)));
    // Disabling the native gate lets the EBREAK sentinel keep its old behavior.
    let (mut cpu, mut mem) = setup(Privilege::Machine, &[], &[ebreak()]);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Ebreak));
}

#[test]
fn trigger_mode_uses_current_privilege_while_mprv_only_changes_data_protection() {
    for trigger_mode in [M, S] {
        let (mut cpu, mut mem) = setup(
            Privilege::Machine,
            &[
                (addr::TDATA2, DATA),
                (addr::TDATA1, TYPE | trigger_mode | 1),
                (addr::MSTATUS, MSTATUS_MIE | MSTATUS_MPRV | (1 << 11)),
            ],
            &[instruction(Access::Load)],
        );
        mem.poke_u32(DATA, 42);
        if trigger_mode == M {
            assert_eq!(cpu.step(&mut mem), Err(Trap::Trigger(DATA)));
        } else {
            assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
            assert_eq!(cpu.reg(5), 42);
        }
    }
}

#[test]
fn interior_byte_match_reports_matched_address_and_bank_writes_are_traced() {
    let (mut cpu, mut mem) = setup(
        Privilege::Machine,
        &[
            (addr::MTVEC, HANDLER),
            (addr::TDATA2, DATA + 2),
            (addr::TDATA1, TYPE | M | 1),
            (addr::MSTATUS, MSTATUS_MIE),
        ],
        &[instruction(Access::Load)],
    );
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
    assert_eq!(cpu.csr().read(addr::MEPC), CODE);
    assert_eq!(cpu.csr().read(addr::MTVAL), DATA + 2);
    let (mut cpu, mut mem) = setup(Privilege::Machine, &[], &[csrrw(addr::TDATA1, 0, 0)]);
    let records = Records::default();
    cpu.set_trace(Some(Box::new(records.clone())));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert!(records.0.borrow()[0].changes.contains(&Change::Csr {
        address: addr::TDATA1,
        value: TYPE
    }));
}
