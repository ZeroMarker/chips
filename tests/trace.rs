//! The trace format itself.
//!
//! ROADMAP P3 compares this model against RTL instruction by instruction, and
//! the format has to be frozen before the RTL exists. These tests are what
//! "frozen" means in practice: they pin the exact text, so a change to the
//! grammar shows up as a failing test rather than as a mysterious diff between
//! two implementations months later.

mod common;

use chips::cpu::StopReason;
use chips::trace::{Change, Record, StepResult, TailTrace, TextTrace, Trace};
use common::*;
use std::sync::{Arc, Mutex};

/// Code and data live here, chosen so every address fits a 12-bit immediate.
const CODE: u32 = 0x100;

/// A sink that renders each record and keeps the lines, so a test can assert on
/// the text a consumer would actually read.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<String>>>);

impl Trace for Lines {
    fn record(&mut self, record: &Record) {
        self.0.lock().unwrap().push(record.to_line());
    }
}

impl Lines {
    fn get(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// Run `words` from `base` with a trace attached, and return the rendered lines.
fn traced_run(base: u32, words: &[u32]) -> Vec<String> {
    let sink = Lines::default();
    let (mut cpu, mut mem) = machine(base, words);
    cpu.set_trace(Some(Box::new(sink.clone())));
    let _ = cpu.run(&mut mem, 10_000);
    sink.get()
}

fn traced_run_at(words: &[u32]) -> Vec<String> {
    traced_run(CODE, words)
}

#[test]
fn the_version_line_is_written_exactly_once() {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut t = TextTrace::new(&mut buf);
        t.write_header().unwrap();
        t.write_header().unwrap();
        // A record after the header must not produce a second one.
        t.record(&Record {
            step: 1,
            pc: 0,
            instruction: 0x13,
            outcome: StepResult::Retired,
            changes: Vec::new(),
        });
        t.flush().unwrap();
    }
    let text = String::from_utf8(buf).unwrap();
    assert_eq!(
        text.lines().next(),
        Some("v1"),
        "the first line identifies the format"
    );
    assert_eq!(
        text.matches("v1").count(),
        1,
        "one header for the whole trace: {text}"
    );
    assert_eq!(text.lines().count(), 2, "header plus one record");
}

#[test]
fn a_register_write_appears_with_its_value() {
    let inst = li(5, 7);
    let lines = traced_run_at(&[inst, ebreak()]);
    assert_eq!(
        lines[0],
        format!("i 1 pc={CODE:08x} inst={inst:08x} x5=00000007"),
    );
}

#[test]
fn a_step_that_changes_nothing_says_only_that_it_ran() {
    let nop = addi(0, 0, 0);
    let lines = traced_run_at(&[nop]);
    assert_eq!(lines[0], format!("i 1 pc={CODE:08x} inst={nop:08x}"));
}

#[test]
fn x0_is_never_recorded() {
    // `addi x0, x0, 0` retires but writes nothing, because x0 is hardwired to
    // zero. Claiming a write would make a differential run diff forever.
    let lines = traced_run_at(&[addi(0, 0, 0)]);
    assert!(!lines[0].contains("x0="), "{}", lines[0]);
}

#[test]
fn a_register_written_with_the_value_it_had_is_still_recorded() {
    // The hardware performs the write too, so the trace must show it. Omitting
    // it would hide a difference in write behaviour behind identical state.
    let lines = traced_run_at(&[addi(5, 0, 0), addi(5, 0, 0), ebreak()]);
    assert!(lines[0].contains("x5=00000000"), "{}", lines[0]);
    assert!(lines[1].contains("x5=00000000"), "{}", lines[1]);
}

#[test]
fn a_store_records_the_address_width_and_value() {
    let data = CODE + 0x40;
    let [hi, lo] = li32(5, data);
    let [chi, clo] = li32(6, 0xABCD);
    let sw = store(0, 6, 5, 0b010);
    let lines = traced_run_at(&[hi, lo, chi, clo, sw, ebreak()]);
    assert_eq!(
        lines[4],
        format!(
            "i 5 pc={:08x} inst={sw:08x} m{data:08x}=0000abcd",
            CODE + 16
        ),
        "two hex digits per byte, so eight for a word"
    );
}

#[test]
fn the_value_field_is_as_wide_as_the_access() {
    let data = CODE + 0x40;
    let [hi, lo] = li32(5, data);
    let lines = traced_run_at(&[
        hi,
        lo,
        li(6, -1),
        store(0, 6, 5, 0b000), // sb
        store(1, 6, 5, 0b001), // sh
        store(2, 6, 5, 0b010), // sw
        ebreak(),
    ]);
    assert!(
        lines[3].ends_with(&format!("m{data:08x}=ff")),
        "byte: {}",
        lines[3]
    );
    assert!(
        lines[4].ends_with(&format!("m{:08x}=ffff", data + 1)),
        "halfword: {}",
        lines[4]
    );
    assert!(
        lines[5].ends_with(&format!("m{:08x}=ffffffff", data + 2)),
        "word: {}",
        lines[5]
    );
}

#[test]
fn a_csr_write_records_the_legalized_value() {
    // `satp` legalizes a translation mode to Bare, so the trace shows what the
    // register holds afterwards, not what was requested. A hardware comparison
    // has to agree on the legalized value.
    let lines = traced_run_at(&[
        li32(5, 0x8D)[0],
        li32(5, 0x8D)[1],
        csrrw(0x180, 5, 0),
        ebreak(),
    ]);
    // 0x8D is mode 8 (Sv39) plus an ASID of 0x80. Only the mode is cleared; the
    // ASID is a real field and survives.
    let recorded = lines[2]
        .split_whitespace()
        .find_map(|f| f.strip_prefix("c180="))
        .unwrap();
    assert_eq!(recorded, "00000080", "{}", lines[2]);
    assert_eq!(
        u32::from_str_radix(&recorded[5..], 16).unwrap() & 0xF,
        0,
        "the translation mode reads back as Bare"
    );
}

#[test]
fn a_delivered_trap_is_its_own_record_kind() {
    let [hi, lo] = li32(1, 0x300);
    let lines = traced_run_at(&[hi, lo, csrrw(0x305, 1, 0), ecall(), ebreak()]);
    // li32 is two words, so the ecall is the third step, at CODE + 12. The trap
    // record carries the cause and also the CSRs entry wrote, which is why it is
    // checked field by field rather than against a whole string.
    let trap = &lines[3];
    assert!(
        trap.starts_with(&format!("t 4 pc={:08x} inst=00000073", CODE + 12)),
        "{trap}"
    );
    assert!(trap.contains("cause=11"), "{trap}");
    assert!(trap.contains("tval=00000000"), "{trap}");
}

#[test]
fn a_trap_with_nowhere_to_go_is_a_halt_record() {
    // `x` means the run stopped. A trap that had nowhere to be delivered is
    // returned to the caller instead, and the step still happened.
    let lines = traced_run_at(&[ebreak()]);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], format!("x 1 pc={CODE:08x} inst=00100073"));
}

#[test]
fn halt_and_trap_records_are_distinguishable() {
    // Conflating them would make a handled fault look like a dead run.
    let halt = Record {
        step: 1,
        pc: 0,
        instruction: 0x73,
        outcome: StepResult::Halt,
        changes: Vec::new(),
    };
    let trap = Record {
        step: 1,
        pc: 0,
        instruction: 0x73,
        outcome: StepResult::Trap { cause: 11, tval: 0 },
        changes: Vec::new(),
    };
    assert!(halt.to_line().starts_with("x 1 "));
    assert!(trap.to_line().starts_with("t 1 "));
}

#[test]
fn a_trap_records_the_state_it_changed_too() {
    // Trap entry writes mepc, mcause and mtval. A trace that showed only the
    // event and not the effect would not say where the handler went.
    let [hi, lo] = li32(1, 0x300);
    let lines = traced_run_at(&[hi, lo, csrrw(0x305, 1, 0), ecall(), ebreak()]);
    let trap = &lines[3];
    assert!(
        trap.contains(&format!("c341={:08x}", CODE + 12)),
        "mepc: {trap}"
    );
    assert!(trap.contains("c342=0000000b"), "mcause: {trap}");
}

#[test]
fn the_step_number_counts_every_step() {
    // Including the step that trapped, so a consumer can line the trace up
    // against a cycle count.
    //
    // The handler un-installs itself (`csrw mtvec, x0` makes the model treat the
    // run as handlerless) and then ecalls, which halts. Without that the run
    // would take the same trap forever and the numbering would be untestable.
    let (mut cpu, mut mem) = machine(CODE, &[]);
    let sink = Lines::default();
    cpu.set_trace(Some(Box::new(sink.clone())));

    let [hi, lo] = li32(1, 0x180);
    load_words(&mut mem, CODE, &[hi, lo, csrrw(0x305, 1, 0), ecall()]);
    load_words(&mut mem, 0x180, &[li(2, 0), csrrw(0x305, 2, 0), ecall()]);
    cpu.set_pc(CODE);
    let _ = cpu.run(&mut mem, 100);

    let lines = sink.get();
    let numbers: Vec<u64> = lines
        .iter()
        .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect();
    assert_eq!(
        numbers,
        (1..=lines.len() as u64).collect::<Vec<_>>(),
        "consecutive from 1, including the trap and the halt"
    );
    assert!(lines[3].starts_with("t 4 "), "the ecall: {}", lines[3]);
    assert!(
        lines.last().unwrap().starts_with('x'),
        "the run ends on a halt record: {}",
        lines.last().unwrap()
    );
}

#[test]
fn reads_are_not_recorded() {
    // Only writes. A read cannot change state, and a read that misses is already
    // a load access fault.
    let [hi, lo] = li32(5, CODE + 0x40);
    let lines = traced_run_at(&[hi, lo, load(0, 5, 0b010, 6), ebreak()]);
    assert!(lines[2].starts_with("i 3 "), "{}", lines[2]);
    assert!(!lines[2].contains(" m"), "no memory record: {}", lines[2]);
}

#[test]
fn a_fetch_that_never_happens_is_traced_at_the_pc_it_expected() {
    // The instruction word is unknown, so the record carries zero. What matters is
    // that the pc is there: "the hardware jumped somewhere else" is only
    // diagnosable if the trace says where it expected to be.
    let (mut cpu, mut mem) = machine(CODE, &[]);
    let sink = Lines::default();
    cpu.set_trace(Some(Box::new(sink.clone())));
    cpu.set_pc(CODE + 2); // misaligned
    let _ = cpu.step(&mut mem);

    let lines = sink.get();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0],
        format!(
            "t 1 pc={:08x} inst=00000000 cause=0 tval={:08x}",
            CODE + 2,
            CODE + 2
        )
    );
}

#[test]
fn a_change_renders_exactly_as_specified() {
    let cases = [
        (
            Change::Register {
                index: 31,
                value: 0xDEAD_BEEF,
            },
            " x31=deadbeef",
        ),
        (
            Change::Csr {
                address: 0x300,
                value: 0x1808,
            },
            " c300=00001808",
        ),
        (
            Change::Memory {
                address: 0x8000_0000,
                len: 4,
                value: 0x1234_5678,
            },
            " m80000000=12345678",
        ),
        (
            Change::Memory {
                address: 1,
                len: 1,
                value: 0xFF,
            },
            " m00000001=ff",
        ),
    ];
    for (change, expected) in cases {
        let line = Record {
            step: 1,
            pc: 0,
            instruction: 0,
            outcome: StepResult::Retired,
            changes: vec![change],
        }
        .to_line();
        assert!(
            line.ends_with(expected),
            "expected {expected:?} at the end of {line:?}"
        );
    }
}

#[test]
fn every_line_is_one_line() {
    // No continuation lines: a consumer can parse a record without knowing
    // anything about the records before it.
    let program = [li(5, 3), li32(6, 0x1234_5678)[0], ebreak()];
    for line in traced_run_at(&program) {
        assert!(!line.contains('\n'), "embedded newline in {line:?}");
        assert!(!line.ends_with(' '), "trailing space in {line:?}");
        assert!(!line.is_empty());
    }
}

#[test]
fn the_tail_trace_keeps_only_the_last_records() {
    let mut tail = TailTrace::new(3);
    for step in 1..=10u64 {
        tail.record(&Record {
            step,
            pc: CODE,
            instruction: 0x13,
            outcome: StepResult::Retired,
            changes: Vec::new(),
        });
    }
    let steps: Vec<u64> = tail.records().map(|r| r.step).collect();
    assert_eq!(steps, vec![8, 9, 10], "oldest dropped first");
    assert_eq!(tail.last().unwrap().step, 10);
}

#[test]
fn tracing_is_off_by_default() {
    let (mut cpu, mut mem) = machine(CODE, &[li(5, 1), ebreak()]);
    assert!(!cpu.is_tracing());
    assert_eq!(cpu.run(&mut mem, 10), Ok(StopReason::Ebreak));
    assert_eq!(cpu.reg(5), 1);
}

#[test]
fn the_trace_does_not_change_execution() {
    // The point of a golden reference is that instrumentation must not perturb
    // it: same registers, same counters, same memory, with and without.
    let program = [
        li(5, (0x40 + CODE) as i32),
        li(6, 4),
        addi(7, 5, 6),         // x7 = x5 + 6
        store(0, 7, 5, 0b010), // sw x7, 0(x5)
        ebreak(),
    ];

    let (mut plain, mut mem) = machine(CODE, &program);
    plain.run(&mut mem, 100).unwrap();

    let (mut traced, mut mem) = machine(CODE, &program);
    traced.set_trace(Some(Box::new(TailTrace::new(1000))));
    traced.run(&mut mem, 100).unwrap();

    for i in 0..32 {
        assert_eq!(plain.reg(i), traced.reg(i), "x{i} differs");
    }
    assert_eq!(plain.instret(), traced.instret());
    assert_eq!(plain.cycle(), traced.cycle());
    assert_eq!(plain.pc(), traced.pc());
    assert_eq!(
        mem.peek_u32(CODE + 0x40),
        0x40 + CODE + 6,
        "the store landed the same way"
    );
}

#[test]
fn the_trace_distinguishes_an_ecall_from_each_mode() {
    // The cause is the observable difference between the privilege levels, so a
    // trace that always said 11 would hide a mode bug completely.
    let mut causes = Vec::new();
    for mode in chips::isa::Privilege::ALL {
        let sink = Lines::default();
        let (mut cpu, mut mem) = machine(CODE, &[]);
        cpu.set_trace(Some(Box::new(sink.clone())));
        // Install the handler *from machine mode* — writing mtvec from U would
        // itself be an illegal instruction — then drop to `mode` and ecall, so
        // the trap is delivered and appears as a `t` record with a cause.
        load_words(&mut mem, CODE, &[li(1, 0x300), csrrw(0x305, 1, 0), ecall()]);
        let _ = cpu.step(&mut mem);
        let _ = cpu.step(&mut mem);
        cpu.set_privilege(mode);
        let _ = cpu.step(&mut mem);
        let lines = sink.get();
        let line = &lines[2];
        let cause: u32 = line
            .split_whitespace()
            .find_map(|f| f.strip_prefix("cause="))
            .unwrap()
            .parse()
            .unwrap();
        causes.push(cause);
    }
    assert_eq!(causes, vec![8, 9, 11], "U, then S, then M");
}

#[test]
fn an_untraced_run_produces_no_lines() {
    // A sink that is never attached must not be consulted, which is what keeps
    // the default path free.
    let sink = Lines::default();
    let (mut cpu, mut mem) = machine(CODE, &[li(5, 1), ebreak()]);
    cpu.run(&mut mem, 10).unwrap();
    assert!(sink.get().is_empty());
    // Attaching after the fact still works, and only sees what comes next.
    cpu.set_trace(Some(Box::new(sink.clone())));
    assert!(cpu.is_tracing());
    cpu.set_trace(None);
    assert!(!cpu.is_tracing());
}

#[test]
fn a_store_that_faults_is_not_recorded() {
    // Recording a write that did not happen would send a differential run
    // chasing a difference in the wrong direction, so the record is pushed only
    // after the store has succeeded.
    let sink = Lines::default();
    // RAM covers only the first 64 bytes, so a store to 0x9000_0000 is unmapped.
    let mut mem = chips::mem::Memory::from_regions(vec![chips::mem::Region::ram(
        CODE,
        0x40,
        chips::mem::Permissions::READ_WRITE,
    )]);
    let (mut cpu, _) = machine(CODE, &[]);
    cpu.set_trace(Some(Box::new(sink.clone())));

    let [hi, lo] = li32(5, 0x9000_0000);
    load_words(
        &mut mem,
        CODE,
        &[hi, lo, li(6, 1), store(0, 6, 5, 0b010), ebreak()],
    );
    cpu.set_pc(CODE);
    for _ in 0..4 {
        let _ = cpu.step(&mut mem);
    }

    let lines = sink.get();
    assert!(
        !lines.iter().any(|l| l.contains(" m90000000")),
        "an unmapped store must not appear as a write: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("cause=7")),
        "it should appear as a store access fault: {lines:?}"
    );
}
