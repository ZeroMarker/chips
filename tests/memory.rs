//! The decoded address map: region containment, permissions, and the three
//! access faults.
//!
//! Everything else in the test suite uses `Memory::permissive()`, where nothing
//! can fault. These tests are the counterweight: they install a real map and
//! check that the CPU reports the architecturally correct `mcause` for an
//! access that the platform does not allow.

mod common;

use chips::cpu::{Cpu, StepOutcome, Trap};
use chips::csr::addr as csr_addr;
use chips::mem::{Access, Memory, Permissions, Region};
use common::*;

const CODE: u32 = 0x8000_0000;
const HANDLER: u32 = 0x8000_0200;
const RAM_END: u32 = 0x8000_1000;
/// Unmapped on purpose: no region covers this.
const NOWHERE: u32 = 0x9000_0000;

/// RAM covering [CODE, RAM_END) plus a read-only device page above it.
fn platform() -> Memory {
    Memory::from_regions(vec![
        Region::ram(CODE, u64::from(RAM_END - CODE), Permissions::READ_WRITE),
        Region::sparse(RAM_END, 0x100, Permissions::READ_ONLY),
    ])
}

/// A CPU running `words` at `base` on the platform above.
fn machine(base: u32, words: &[u32]) -> (Cpu, Memory) {
    let mut mem = platform();
    load_words(&mut mem, base, words);
    let mut cpu = Cpu::new();
    cpu.set_pc(base);
    (cpu, mem)
}

/// Materialize `value` into register `rd`, returning the two instruction words.
fn materialize(rd: u32, value: u32) -> [u32; 2] {
    li32(rd, value)
}

#[test]
fn fetching_an_unmapped_address_is_an_instruction_access_fault() {
    let mut mem = platform();
    let mut cpu = Cpu::new();
    cpu.set_pc(NOWHERE);

    assert_eq!(
        cpu.step(&mut mem),
        Err(Trap::InstructionAccessFault(NOWHERE))
    );
    assert_eq!(Trap::InstructionAccessFault(0).mcause(), 1);
    assert_eq!(Trap::InstructionAccessFault(NOWHERE).mtval(), NOWHERE);
}

#[test]
fn fetching_mapped_memory_works() {
    // Guards the fault test above: the same fetch must succeed when the address
    // is inside a region, or "unmapped" would be testing nothing.
    let (mut cpu, mut mem) = machine(CODE, &[addi(1, 0, 42), ebreak()]);
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.reg(1), 42);
}

#[test]
fn an_unmapped_load_reports_cause_5() {
    let addr = materialize(1, NOWHERE);
    let (mut cpu, mut mem) = machine(
        CODE,
        &[addr[0], addr[1], load(0, 1, 0b010, 2), ebreak()], // lw x2, 0(x1)
    );

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Err(Trap::LoadAccessFault(NOWHERE)));
    assert_eq!(Trap::LoadAccessFault(0).mcause(), 5);
    assert_eq!(cpu.reg(2), 0, "a faulting load must not write rd");
}

#[test]
fn an_unmapped_store_reports_cause_7_and_writes_nothing() {
    let addr = materialize(1, NOWHERE);
    let (mut cpu, mut mem) = machine(
        CODE,
        &[
            addr[0],
            addr[1],
            store(0, 2, 1, 0b010), // sw x2, 0(x1)
            ebreak(),
        ],
    );

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Err(Trap::StoreAccessFault(NOWHERE)));
    assert_eq!(Trap::StoreAccessFault(0).mcause(), 7);
    // The address stays unmapped, so there is nothing to read back; what
    // matters is that the CPU reported a fault instead of inventing storage.
}

#[test]
fn a_read_only_region_refuses_writes_but_allows_reads() {
    let base = materialize(1, RAM_END);
    let (mut cpu, mut mem) = machine(
        CODE,
        &[
            base[0],
            base[1],
            load(0, 1, 0b010, 2),  // lw x2, 0(x1) — permitted
            store(0, 2, 1, 0b010), // sw x2, 0(x1) — refused
            ebreak(),
        ],
    );

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Err(Trap::StoreAccessFault(RAM_END)));
}

#[test]
fn an_access_running_past_the_end_of_a_region_faults() {
    // A region whose last byte is not word-aligned, so an *aligned* `lw` still
    // straddles the end. The shared platform's RAM is a multiple of 4 bytes and
    // would report a misalignment instead, testing something else.
    let mut mem = Memory::from_regions(vec![Region::ram(CODE, 0x101, Permissions::READ_WRITE)]);
    load_words(&mut mem, CODE, &[load(0, 0, 0b010, 2), ebreak()]); // lw x2, 0(x0)
    let mut cpu = Cpu::new();
    cpu.set_pc(CODE);

    // CODE + 0x100 is 4-byte aligned but the region ends at CODE + 0x101.
    assert_eq!(cpu.reg(0), 0);
    assert_eq!(
        mem.load(CODE + 0x100, 4),
        Err(chips::AccessFault {
            addr: CODE + 0x100,
            access: Access::Load,
        })
    );
    // The last fully contained word is fine.
    assert_eq!(mem.load(CODE + 0xFC, 4), Ok(0));
}

#[test]
fn an_unmapped_misaligned_load_reports_the_access_fault() {
    // Misaligned data accesses are completed, not trapped, so the alignment of
    // the address is no longer what decides the outcome. An address that is both
    // misaligned and unmapped reports the access fault: there is no second,
    // earlier check left to fire.
    let addr = materialize(1, NOWHERE + 1);
    let (mut cpu, mut mem) = machine(CODE, &[addr[0], addr[1], load(0, 1, 0b010, 2), ebreak()]);

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(cpu.step(&mut mem), Err(Trap::LoadAccessFault(NOWHERE + 1)));
}

#[test]
fn a_misaligned_load_inside_mapped_memory_still_succeeds() {
    // The counterpart: misalignment is no longer a fault at all, so the same
    // encoding against a mapped address completes and reads across the boundary.
    let data = CODE + 0x800;
    let addr = materialize(1, data + 1);
    let (mut cpu, mut mem) = machine(CODE, &[addr[0], addr[1], load(0, 1, 0b010, 2), ebreak()]);
    mem.poke_u32(data + 1, 0xAABB_CCDD);

    for _ in 0..3 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(cpu.reg(2), 0xAABB_CCDD);
}

#[test]
fn access_faults_route_through_the_machine_trap_handler() {
    // The same fault as `an_unmapped_load_reports_cause_5`, but with a handler
    // installed, so it must be delivered through mtvec rather than returned.
    let setup = materialize(1, HANDLER);
    let target = materialize(2, NOWHERE);
    let (mut cpu, mut mem) = machine(
        CODE,
        &[
            setup[0],
            setup[1],
            csrrw(csr_addr::MTVEC, 1, 0), // mtvec = HANDLER
            target[0],
            target[1],
            load(0, 2, 0b010, 3), // lw x3, 0(x2) -> LoadAccessFault
            ebreak(),
        ],
    );
    load_words(&mut mem, HANDLER, &[mret()]);

    for _ in 0..5 {
        cpu.step(&mut mem).expect("setup must not trap");
    }
    let faulting_pc = cpu.pc();
    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::TrapTaken));

    assert_eq!(cpu.csr().read(csr_addr::MCAUSE), 5);
    assert_eq!(cpu.csr().read(csr_addr::MTVAL), NOWHERE);
    assert_eq!(cpu.csr().read(csr_addr::MEPC), faulting_pc);
    assert_eq!(cpu.pc(), HANDLER, "the handler runs, the run does not stop");
    assert_eq!(cpu.reg(3), 0);

    assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    assert_eq!(
        cpu.pc(),
        faulting_pc,
        "mret returns to the faulting instruction"
    );
}

#[test]
fn permissive_memory_cannot_fault_at_either_end_of_the_address_space() {
    // The whole point of the permissive map: instruction-level tests must not
    // have to think about layout. Check the extremes, where a naive
    // implementation overflows.
    let mut mem = Memory::permissive();

    assert_eq!(mem.load(0, 4), Ok(0));
    assert_eq!(mem.load(0xFFFF_FFFC, 4), Ok(0));
    assert_eq!(mem.fetch_u32(0xFFFF_FFFC), Ok(0));
    assert!(mem.store(0xFFFF_FFFC, 4, 0xdead_beef).is_ok());
    assert_eq!(mem.peek_u32(0xFFFF_FFFC), 0xdead_beef);
}

#[test]
fn an_image_that_does_not_fit_the_map_is_rejected() {
    let mut mem = Memory::from_regions(vec![Region::ram(CODE, 16, Permissions::READ_WRITE)]);

    assert!(
        mem.load_image(CODE, &[0u8; 16]).is_ok(),
        "an exact fit is fine"
    );
    assert!(
        mem.load_image(CODE, &[0u8; 17]).is_err(),
        "one byte over is not"
    );
    assert!(
        mem.load_image(CODE + 8, &[0u8; 16]).is_err(),
        "an image straddling the end must not be truncated"
    );
}

#[test]
fn flat_and_sparse_backing_agree() {
    // The two backings are an implementation detail; a program must not be able
    // to tell which one it is running on.
    let code = materialize(5, CODE);
    let value = materialize(2, 0x1234_5678);
    let program = [
        code[0],
        code[1], // x5 = CODE, so offsets are relative to RAM
        li(1, -1),
        store(0x40, 1, 5, 0b010), // sw x1, 0x40(x5)
        value[0],
        value[1],                 // x2 = 0x1234_5678
        store(0x40, 2, 5, 0b010), // sw x2, 0x40(x5)
        load(0x40, 5, 0b010, 3),  // lw x3, 0x40(x5)
        load(0x41, 5, 0b100, 4),  // lbu x4, 0x41(x5)
        ebreak(),
    ];

    let mut results = Vec::new();
    for backing in ["flat", "sparse"] {
        let region = match backing {
            "flat" => Region::ram(CODE, 0x1000, Permissions::READ_WRITE),
            _ => Region::sparse(CODE, 0x1000, Permissions::READ_WRITE),
        };
        let mut mem = Memory::from_regions(vec![region]);
        load_words(&mut mem, CODE, &program);
        let mut cpu = Cpu::new();
        cpu.set_pc(CODE);
        let reason = cpu.run(&mut mem, 100).expect("program must not trap");
        assert!(matches!(reason, chips::cpu::StopReason::Ebreak));
        results.push((cpu.reg(3), cpu.reg(4), mem.peek_u32(CODE + 0x40)));
    }

    assert_eq!(results[0], results[1], "backing must not be observable");
    assert_eq!(results[0].0, 0x1234_5678, "lw reads back the stored word");
    assert_eq!(
        results[0].1, 0x56,
        "lbu at +1 is the second byte: memory is little-endian"
    );
}

#[test]
fn regions_are_sorted_by_base_regardless_of_input_order() {
    let mem = Memory::from_regions(vec![
        Region::sparse(0x9000_0000, 0x100, Permissions::READ_WRITE),
        Region::ram(CODE, 0x1000, Permissions::READ_WRITE),
        Region::sparse(RAM_END, 0x100, Permissions::READ_ONLY),
    ]);

    let bases: Vec<u32> = mem.regions().iter().map(|r| r.base()).collect();
    assert_eq!(bases, vec![CODE, RAM_END, 0x9000_0000]);
}

#[test]
fn poke_and_peek_panic_on_an_unmapped_address() {
    // They are unchecked on purpose, so a harness bug must be loud rather than
    // silently reading zero.
    let mut mem = platform();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mem.poke_u32(NOWHERE, 1);
    }));
    assert!(result.is_err(), "poke to an unmapped address must panic");
}

#[test]
fn an_access_fault_names_its_kind_for_diagnostics() {
    assert_eq!(Access::Fetch.mcause(), 1);
    assert_eq!(Access::Load.mcause(), 5);
    assert_eq!(Access::Store.mcause(), 7);
    assert_eq!(Access::Load.name(), "load");
}
