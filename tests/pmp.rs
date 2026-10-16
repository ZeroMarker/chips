//! PMP behavior beyond the official G=0 register probe: isolation, boundaries,
//! locking, MPRV, trap state, and side effects.
mod common;

use chips::cpu::{Cpu, StepOutcome, Trap};
use chips::csr::{addr, Csr, MSTATUS_MPP, MSTATUS_MPRV, MSTATUS_SPP};
use chips::isa::Privilege;
use chips::mem::{Access, Device, Memory, Permissions, Region};
use chips::pmp::Pmp;
use chips::trace::{Change, Record, Trace};
use common::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CODE: u32 = 0x100;
const DATA: u32 = 0x400;
const HANDLER: u32 = 0x800;
const R: u32 = 1;
const W: u32 = 2;
const X: u32 = 4;
const TOR: u32 = 8;
const NA4: u32 = 16;
const NAPOT: u32 = 24;
const L: u32 = 128;

fn setup(values: &[(u32, u32)], words: &[u32]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = machine(CODE, words);
    let mut prefix = Vec::new();
    for &(csr_addr, value) in values {
        prefix.extend(li32(1, value));
        prefix.push(csrrw(csr_addr, 1, 0));
    }
    load_words(&mut mem, 0x2000, &prefix);
    cpu.set_pc(0x2000);
    for _ in prefix {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    cpu.set_pc(CODE);
    (cpu, mem)
}

fn permit_fallback() -> [(u32, u32); 2] {
    [
        (addr::PMPADDR0 + 15, u32::MAX),
        (addr::PMPCFG0 + 3, (NAPOT | R | W | X) << 24),
    ]
}

#[test]
fn reset_denies_supervisor_and_user_but_allows_machine() {
    let pmp = Pmp::default();
    for mode in Privilege::ALL {
        for access in [Access::Fetch, Access::Load, Access::Store] {
            assert_eq!(
                pmp.permits(mode, CODE, 4, access),
                mode == Privilege::Machine
            );
        }
        let (mut cpu, mut mem) = machine(CODE, &[addi(5, 0, 42)]);
        cpu.set_privilege(mode);
        if mode == Privilege::Machine {
            assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
            assert_eq!(cpu.reg(5), 42);
        } else {
            assert_eq!(cpu.step(&mut mem), Err(Trap::InstructionAccessFault(CODE)));
            assert_eq!(cpu.instret(), 0);
            assert_eq!(cpu.reg(5), 0);
        }
    }
}

#[test]
fn csr_bank_has_sixteen_entries_with_four_byte_grain_and_warl_fields() {
    let mut c = Csr::new();
    for i in 0..16 {
        assert!(Csr::exists(addr::PMPADDR0 + i));
        c.write(addr::PMPADDR0 + i, u32::MAX - i);
        assert_eq!(c.read(addr::PMPADDR0 + i), 0x3fff_ffff - i);
    }
    for i in 0..4 {
        assert!(Csr::exists(addr::PMPCFG0 + i));
        c.write(addr::PMPCFG0 + i, 0x601f_1a11);
        // Reserved bits are zero; W without R legalizes by clearing W.
        assert_eq!(c.read(addr::PMPCFG0 + i), 0x001f_1811);
    }
    assert!(!Csr::exists(addr::PMPCFG0 + 4));
    assert!(!Csr::exists(addr::PMPADDR0 + 16));
    c.write(addr::PMPCFG0, 0);
    c.write(addr::PMPADDR0, u32::MAX);
    assert_eq!(
        c.read(addr::PMPADDR0) & 1,
        1,
        "G=0: the lowest address bit is writable even in OFF"
    );
}

#[test]
fn permissions_cover_all_privileges_locks_and_access_types() {
    for mode in Privilege::ALL {
        for lock in [0, L] {
            for requested in 0..8 {
                let mut pmp = Pmp::default();
                pmp.write_address(0, DATA >> 2);
                pmp.write_config(0, NA4 | lock | requested);
                let normalized = if requested & 3 == 2 {
                    requested & !W
                } else {
                    requested
                };
                for (access, permission) in
                    [(Access::Fetch, X), (Access::Load, R), (Access::Store, W)]
                {
                    let expected =
                        (mode == Privilege::Machine && lock == 0) || normalized & permission != 0;
                    assert_eq!(
                        pmp.permits(mode, DATA, 4, access),
                        expected,
                        "mode={mode:?} lock={lock} rwx={requested} access={access:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn off_tor_na4_and_napot_match_exact_boundaries_without_wrapping() {
    let mut pmp = Pmp::default();
    pmp.write_address(0, DATA >> 2);
    pmp.write_config(0, R | W | X); // OFF even though RWX is set
    assert!(!pmp.permits(Privilege::User, DATA, 1, Access::Load));
    pmp.write_config(0, TOR | R);
    assert!(pmp.permits(Privilege::User, 0, 1, Access::Load));
    assert!(pmp.permits(Privilege::User, DATA - 4, 4, Access::Load));
    assert!(!pmp.permits(Privilege::User, DATA, 1, Access::Load));
    pmp.write_config(0, NA4 | R);
    assert!(pmp.permits(Privilege::User, DATA, 4, Access::Load));
    assert!(!pmp.permits(Privilege::User, DATA - 1, 1, Access::Load));
    assert!(!pmp.permits(Privilege::User, DATA + 4, 1, Access::Load));
    for size in [8u64, 16, 32, 256, 4096, 1 << 32] {
        let base = if size == 1 << 32 { 0 } else { 0x10000 };
        pmp.write_config(0, NAPOT | R);
        pmp.write_address(0, ((base >> 2) | ((size >> 3) - 1)) as u32);
        assert!(pmp.permits(Privilege::User, base as u32, 4, Access::Load));
        assert!(pmp.permits(Privilege::User, (base + size - 4) as u32, 4, Access::Load));
        if base != 0 {
            assert!(!pmp.permits(Privilege::User, (base - 1) as u32, 1, Access::Load));
        }
        if base + size < 1 << 32 {
            assert!(!pmp.permits(Privilege::User, (base + size) as u32, 1, Access::Load));
        }
    }
    assert!(!pmp.permits(Privilege::Machine, u32::MAX - 1, 4, Access::Load));
    assert!(!pmp.permits(Privilege::User, u32::MAX - 1, 4, Access::Load));
    pmp.write_address(0, u32::MAX);
    assert!(
        pmp.permits(Privilege::User, u32::MAX - 3, 4, Access::Load),
        "maximum NAPOT encoding cannot overflow"
    );
}

#[test]
fn tor_uses_preceding_address_even_when_preceding_entry_is_off() {
    let mut pmp = Pmp::default();
    pmp.write_address(0, DATA >> 2);
    pmp.write_address(1, (DATA + 16) >> 2);
    pmp.write_config(0, (TOR | R) << 8);
    assert!(!pmp.permits(Privilege::User, DATA - 1, 1, Access::Load));
    assert!(pmp.permits(Privilege::User, DATA, 16, Access::Load));
    assert!(!pmp.permits(Privilege::User, DATA + 16, 1, Access::Load));
    for top in [DATA, DATA - 4] {
        pmp.write_address(1, top >> 2);
        assert!(
            !pmp.permits(Privilege::User, DATA, 1, Access::Load),
            "empty and reversed TOR ranges match nothing"
        );
    }
}

#[test]
fn lowest_partial_match_denies_even_when_later_rule_covers_every_byte() {
    for mode in Privilege::ALL {
        let mut pmp = Pmp::default();
        pmp.write_address(0, (DATA + 4) >> 2);
        pmp.write_address(1, u32::MAX);
        pmp.write_config(0, NA4 | R | ((NAPOT | R | W | X) << 8));
        assert!(
            !pmp.permits(mode, DATA + 2, 4, Access::Load),
            "overlap starts before the first matching entry"
        );
        assert!(
            !pmp.permits(mode, DATA + 6, 4, Access::Load),
            "overlap ends beyond the first matching entry"
        );
        assert!(pmp.permits(mode, DATA, 4, Access::Store));
        assert_eq!(
            pmp.permits(mode, DATA + 4, 4, Access::Store),
            mode == Privilege::Machine,
            "an unlocked M access bypasses RWX but not full-containment checks"
        );
    }
}

#[test]
fn locked_config_bytes_and_tor_predecessors_remain_locked_until_reset() {
    let mut pmp = Pmp::default();
    pmp.write_address(0, DATA >> 2);
    pmp.write_address(3, 0x123);
    pmp.write_address(4, 0x456);
    pmp.write_config(0, L | NA4 | R);
    pmp.write_config(1, L | TOR | R);
    pmp.write_config(0, (NAPOT | R) << 8);
    assert_eq!(
        pmp.read_config(0),
        L | NA4 | R | ((NAPOT | R) << 8),
        "neighbors remain writable"
    );
    pmp.write_address(0, 0);
    pmp.write_address(3, 0); // entry 4's locked TOR protects predecessor across CSR groups
    pmp.write_address(4, 0);
    pmp.write_config(1, u32::MAX);
    assert_eq!(pmp.read_address(0), DATA >> 2);
    assert_eq!(pmp.read_address(3), 0x123);
    assert_eq!(pmp.read_address(4), 0x456);
    assert_eq!(pmp.read_config(1) & 0xff, L | TOR | R);
    assert!(!pmp.permits(Privilege::Machine, DATA, 4, Access::Store));
    pmp.write_config(3, L << 24); // OFF may be locked too
    pmp.write_address(15, u32::MAX);
    assert_eq!(pmp.read_address(15), 0);
    assert_eq!(
        Pmp::default().read_config(0),
        0,
        "new hart resets lock state"
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
fn denied_cpu_accesses_report_precise_traps_and_have_no_side_effects() {
    for mode in Privilege::ALL {
        for access in [Access::Fetch, Access::Load, Access::Store] {
            let address = if access == Access::Fetch { CODE } else { DATA };
            let instruction = match access {
                Access::Fetch => addi(5, 0, 42),
                Access::Load => load(DATA as i32, 0, 2, 5),
                Access::Store => store(DATA as i32, 5, 0, 2),
            };
            let mut values = permit_fallback().to_vec();
            values.extend([(addr::PMPADDR0, address >> 2), (addr::PMPCFG0, NA4 | L)]);
            let (mut cpu, mut mem) = setup(&values, &[instruction]);
            mem.poke_u32(DATA, 0x1234_5678);
            cpu.set_privilege(mode);
            let records = Records::default();
            cpu.set_trace(Some(Box::new(records.clone())));
            let retired = cpu.instret();
            let expected = match access {
                Access::Fetch => Trap::InstructionAccessFault(CODE),
                Access::Load => Trap::LoadAccessFault(DATA),
                Access::Store => Trap::StoreAccessFault(DATA),
            };
            assert_eq!(cpu.step(&mut mem), Err(expected));
            assert_eq!(cpu.pc(), CODE);
            assert_eq!(cpu.instret(), retired);
            assert_eq!(cpu.reg(5), 0);
            assert_eq!(mem.peek_u32(DATA), 0x1234_5678);
            assert!(!records.0.borrow()[0]
                .changes
                .iter()
                .any(|c| matches!(c, Change::Memory { .. } | Change::Register { .. })));
            assert_eq!(
                records.0.borrow()[0].instruction,
                if access == Access::Fetch {
                    0
                } else {
                    instruction
                }
            );
        }
    }
    // A nonaligned word straddling two allowed entries must fail atomically.
    let (mut cpu, mut mem) = setup(
        &[
            (addr::PMPADDR0, DATA >> 2),
            (addr::PMPADDR0 + 1, (DATA + 4) >> 2),
            (addr::PMPCFG0, (L | NA4 | R | W) | ((L | NA4 | R | W) << 8)),
        ],
        &[store((DATA + 2) as i32, 1, 0, 2)],
    );
    mem.poke(DATA, &[0x55; 8]);
    assert_eq!(cpu.step(&mut mem), Err(Trap::StoreAccessFault(DATA + 2)));
    assert_eq!(mem.peek(DATA, 8), 0x5555_5555_5555_5555);
}

#[test]
fn pmp_faults_use_existing_machine_and_delegated_supervisor_trap_paths() {
    for delegated in [false, true] {
        let mut values = permit_fallback().to_vec();
        values.extend([
            (addr::MTVEC, HANDLER),
            (addr::STVEC, HANDLER + 0x100),
            (addr::MEDELEG, if delegated { 1 << 5 } else { 0 }),
            (addr::PMPADDR0, DATA >> 2),
            (addr::PMPCFG0, NA4),
        ]);
        let (mut cpu, mut mem) = setup(&values, &[load(DATA as i32, 0, 2, 5)]);
        cpu.set_privilege(Privilege::User);
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));
        assert_eq!(cpu.pc(), HANDLER + if delegated { 0x100 } else { 0 });
        assert_eq!(
            cpu.csr()
                .read(if delegated { addr::SEPC } else { addr::MEPC }),
            CODE
        );
        assert_eq!(
            cpu.csr().read(if delegated {
                addr::SCAUSE
            } else {
                addr::MCAUSE
            }),
            5
        );
        assert_eq!(
            cpu.csr()
                .read(if delegated { addr::STVAL } else { addr::MTVAL }),
            DATA
        );
    }
}

#[derive(Debug, Default)]
struct CountingDevice {
    reads: Cell<u32>,
    writes: Cell<u32>,
}
impl Device for CountingDevice {
    fn read(&self, _: u32, _: u32) -> u64 {
        self.reads.set(self.reads.get() + 1);
        7
    }
    fn write(&self, _: u32, _: u32, _: u64) {
        self.writes.set(self.writes.get() + 1);
    }
}

#[test]
fn denied_mmio_is_not_called_and_pmp_allow_does_not_bypass_memory_permissions() {
    for instruction in [load(DATA as i32, 0, 2, 5), store(DATA as i32, 0, 0, 2)] {
        let (mut cpu, _) = setup(
            &[(addr::PMPADDR0, DATA >> 2), (addr::PMPCFG0, L | NA4)],
            &[instruction],
        );
        let device = Rc::new(CountingDevice::default());
        let mut mem = Memory::from_regions(vec![
            Region::ram(0, DATA.into(), Permissions::READ_WRITE),
            Region::shared_device(DATA, 4, Permissions::READ_WRITE, device.clone()),
        ]);
        load_words(&mut mem, CODE, &[instruction]);
        assert!(cpu.step(&mut mem).is_err());
        assert_eq!(device.reads.get(), 0);
        assert_eq!(device.writes.get(), 0);
    }
    let (mut cpu, _) = setup(&permit_fallback(), &[store(DATA as i32, 0, 0, 2)]);
    cpu.set_privilege(Privilege::User);
    let mut mem = Memory::from_regions(vec![
        Region::ram(0, DATA.into(), Permissions::READ_WRITE),
        Region::ram(DATA, 4, Permissions::READ_ONLY),
    ]);
    load_words(&mut mem, CODE, &[store(DATA as i32, 0, 0, 2)]);
    assert_eq!(cpu.step(&mut mem), Err(Trap::StoreAccessFault(DATA)));
}

#[test]
fn mprv_changes_data_privilege_but_never_instruction_fetch_privilege() {
    for target in Privilege::ALL {
        for access in [Access::Load, Access::Store] {
            let instruction = if access == Access::Load {
                load(DATA as i32, 0, 2, 5)
            } else {
                store(DATA as i32, 0, 0, 2)
            };
            let (mut cpu, mut mem) = setup(
                &[(addr::MSTATUS, MSTATUS_MPRV | (target.encoding() << 11))],
                &[instruction],
            );
            mem.poke_u32(DATA, 42);
            if target == Privilege::Machine {
                assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
            } else {
                assert_eq!(
                    cpu.step(&mut mem),
                    Err(if access == Access::Load {
                        Trap::LoadAccessFault(DATA)
                    } else {
                        Trap::StoreAccessFault(DATA)
                    })
                );
            }
        }
    }
    // An explicit read-only rule allows a machine handler to read U data via MPRV.
    let (mut cpu, mut mem) = setup(
        &[
            (addr::PMPADDR0, DATA >> 2),
            (addr::PMPCFG0, NA4 | R),
            (addr::MSTATUS, MSTATUS_MPRV),
        ],
        &[load(DATA as i32, 0, 2, 5)],
    );
    mem.poke_u32(DATA, 42);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.reg(5), 42);
}

#[test]
fn return_to_lower_privilege_clears_mprv_and_pmp_csrs_remain_machine_only() {
    for target in Privilege::ALL {
        let (mut cpu, mut mem) = setup(
            &[
                (addr::MEPC, CODE + 4),
                (addr::MSTATUS, MSTATUS_MPRV | (target.encoding() << 11)),
            ],
            &[mret()],
        );
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
        assert_eq!(cpu.privilege(), target);
        assert_eq!(
            cpu.csr().read(addr::MSTATUS) & MSTATUS_MPRV != 0,
            target == Privilege::Machine
        );
        assert_eq!(cpu.csr().read(addr::MSTATUS) & MSTATUS_MPP, 0);
    }
    let (mut cpu, mut mem) = setup(
        &[(addr::MSTATUS, MSTATUS_MPRV | MSTATUS_SPP | MSTATUS_MPP)],
        &[sret()],
    );
    cpu.step(&mut mem).unwrap();
    assert_eq!(cpu.privilege(), Privilege::Supervisor);
    assert_eq!(cpu.csr().read(addr::MSTATUS) & MSTATUS_MPRV, 0);
    for mode in [Privilege::Supervisor, Privilege::User] {
        for csr_addr in [
            addr::PMPCFG0,
            addr::PMPCFG0 + 3,
            addr::PMPADDR0,
            addr::PMPADDR0 + 15,
        ] {
            let instruction = csrrs(csr_addr, 0, 5);
            let (mut cpu, mut mem) = setup(&permit_fallback(), &[instruction]);
            cpu.set_privilege(mode);
            assert_eq!(
                cpu.step(&mut mem),
                Err(Trap::IllegalInstruction(instruction))
            );
        }
    }
}
