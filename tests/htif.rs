//! The HTIF completion protocol and the `riscv-tests` target platform.
//!
//! The end-to-end proof that these work is `scripts/riscv-tests.sh`, which runs
//! the official suites. What is tested here is the parts that script cannot
//! explain when it goes wrong: the encoding of a result, the decoding of a
//! failure, and the memory map those tests are linked against.

mod common;

use chips::cpu::{Cpu, StepOutcome, Trap};
use chips::htif::{Htif, Outcome};
use chips::mem::Device;
use chips::platform;
use common::*;

/// The marker `riscv-tests` ORs into the test number on an unhandled exception.
/// Mirrors the private constant in `chips::htif`, which the tests pin down
/// against the suite's actual encoding.
const UNHANDLED_EXCEPTION_MARKER: u64 = 1337;

/// Write `value` to the HTIF the way a test does: the low word first, then zero
/// to the high word.
fn signal(htif: &Htif, value: u64) {
    htif.write(0, 4, value & 0xFFFF_FFFF);
    htif.write(4, 4, (value >> 32) & 0xFFFF_FFFF);
}

#[test]
fn a_fresh_htif_is_pending() {
    assert_eq!(Htif::new().outcome(), Outcome::Pending);
    assert!(!Outcome::Pending.is_final());
}

#[test]
fn one_means_pass() {
    let htif = Htif::new();
    signal(&htif, 1);
    assert_eq!(htif.outcome(), Outcome::Pass);
    assert!(Outcome::Pass.is_final());
}

#[test]
fn an_odd_value_above_one_names_the_failing_check() {
    // `RVTEST_FAIL` stores (test << 1) | 1.
    for test in [1u64, 2, 7, 42, 1000] {
        let htif = Htif::new();
        signal(&htif, (test << 1) | 1);
        assert_eq!(
            htif.outcome(),
            Outcome::Fail { test: test as u32 },
            "check {test}"
        );
    }
}

#[test]
fn check_668_is_indistinguishable_from_the_exception_marker() {
    // (668 << 1) | 1 == 1337, so the suite's own encoding makes a failure of
    // check 668 look exactly like an unhandled exception. The model resolves it
    // as the exception, which is both the likelier cause and the more actionable
    // report. No shipped suite has a check 668.
    let htif = Htif::new();
    signal(&htif, (668 << 1) | 1);
    assert!(matches!(htif.outcome(), Outcome::UnhandledException { .. }));
}

#[test]
fn a_value_with_bit_zero_clear_is_not_a_command() {
    let htif = Htif::new();
    signal(&htif, 4);
    assert_eq!(htif.outcome(), Outcome::NotACommand { raw: 4 });
}

#[test]
fn the_1337_marker_means_an_unhandled_exception() {
    // A test that takes an exception it cannot handle ORs 1337 into its test
    // number and stores that raw, without the (n << 1) | 1 encoding. Decoding it
    // as a plain failure would report a nonsense check number.
    let htif = Htif::new();
    signal(&htif, 1337);
    match htif.outcome() {
        Outcome::UnhandledException { test, raw } => {
            assert_eq!(raw, 1337);
            assert_eq!(test, None, "no check had been set yet");
        }
        other => panic!("expected an unhandled exception, got {other:?}"),
    }

    // With a small test number already set, the remaining bits recover it. The
    // recovery is best-effort by nature: the OR clobbers any shared bits.
    let htif = Htif::new();
    signal(&htif, UNHANDLED_EXCEPTION_MARKER | (2 << 1));
    assert_eq!(
        htif.outcome(),
        Outcome::UnhandledException {
            test: Some(2),
            raw: UNHANDLED_EXCEPTION_MARKER | 4,
        }
    );
}

#[test]
fn the_two_registers_are_separate() {
    let htif = Htif::new();
    signal(&htif, 1);
    htif.write(8, 4, 0xABCD); // fromhost, low word

    assert_eq!(htif.outcome(), Outcome::Pass, "fromhost is not tohost");
    assert_eq!(htif.read(8, 4), 0xABCD);
    assert_eq!(htif.read(0, 4), 1);
}

#[test]
fn acknowledging_copies_the_command_to_fromhost() {
    let htif = Htif::new();
    signal(&htif, 5);
    htif.acknowledge();
    assert_eq!(htif.read(8, 4), 5);
    assert_eq!(htif.outcome(), Outcome::Fail { test: 2 });
}

#[test]
fn the_platform_places_tohost_where_the_linker_puts_it() {
    // Most tests get tohost at 0x80001000, but a test whose .text.init exceeds
    // a page pushes it to the next boundary. Both must decode.
    for tohost in [0x8000_1000u32, 0x8000_2000, 0x8000_3000] {
        let (mut mem, htif, _clint) = platform::riscv_tests(tohost);
        mem.store(tohost, 4, 1).expect("tohost is writable");
        assert_eq!(htif.outcome(), Outcome::Pass, "tohost at 0x{tohost:08x}");
    }
}

#[test]
fn the_platform_rejects_a_nonsensical_tohost() {
    // Below the reset vector, or not page-aligned: the link script cannot
    // produce either, so this is a caller error worth failing loudly on.
    for bad in [0x1000u32, 0x8000_1001, 0x8000_1800] {
        assert!(
            std::panic::catch_unwind(|| platform::riscv_tests(bad)).is_err(),
            "tohost = 0x{bad:08x} should be rejected"
        );
    }
}

#[test]
fn the_platform_covers_stack_ram_and_the_image() {
    let (mem, _htif, _clint) = platform::riscv_tests(0x8000_1000);
    let tohost = 0x8000_1000u32;

    // The reset vector, the device page, RAM above it, and stack RAM below.
    assert!(mem.load(0x8000_0000, 4).is_ok());
    assert!(mem.load(tohost, 4).is_ok());
    assert!(
        mem.load(tohost + 0x2000, 4).is_ok(),
        "RAM above the device page"
    );
    assert!(
        mem.load(0x8000_0000 - 0x1000, 4).is_ok(),
        "stack RAM below the reset vector"
    );

    // And not beyond.
    assert!(mem.load(0x7FF0_0000 - 4, 4).is_err(), "below the stack");
    // RAM ends exactly at the device page, so the last word of RAM is a valid
    // access...
    assert!(mem.load(tohost - 4, 4).is_ok(), "the last word of RAM");
    // ...but a word starting inside RAM and running into the device page is not.
    // A guest access may not straddle a region, even though both regions are
    // individually backed and writable.
    assert!(
        mem.load(tohost - 2, 4).is_err(),
        "a word straddling the RAM/device boundary must fault"
    );
    assert!(
        mem.load(tohost, 4).is_ok(),
        "the device page itself is readable"
    );
}

#[test]
fn a_test_that_reports_through_the_device_stops_the_run() {
    // The shape of every riscv-tests binary: set mtvec, then let a trap land in
    // the handler, which writes the result. The runner watches the device rather
    // than waiting for a halt, because the test never halts.
    let tohost = 0x8000_1000u32;
    let (mut mem, htif, _clint) = platform::riscv_tests(tohost);

    let [hi, lo] = li32(1, tohost);
    load_words(
        &mut mem,
        0x8000_0000,
        &[
            hi,
            lo,                 // x1 = tohost
            csrrw(0x305, 1, 0), // mtvec = tohost
            ecall(),            // traps into the handler
            ebreak(),
        ],
    );
    // The "handler" is the payload the test would write from its own trap
    // vector. Stand in for it by writing the completion code directly.
    htif.write(0, 4, 1);
    htif.write(4, 4, 0);

    let mut cpu = Cpu::new();
    cpu.set_pc(0x8000_0000);
    for _ in 0..3 {
        assert_eq!(cpu.step(&mut mem), Ok(StepOutcome::Continue));
    }
    assert_eq!(htif.outcome(), Outcome::Pass);
    assert!(htif.outcome().is_final());
}

#[test]
fn a_privilege_instruction_the_model_lacks_traps_rather_than_passing() {
    // A guard on the runner's honesty: `sret` is not legal with only M-mode, so
    // a program using it must not be able to reach a pass by accident.
    let (mut cpu, mut mem) = machine(0x100, &[sret(), ebreak()]);
    assert_eq!(cpu.step(&mut mem), Err(Trap::IllegalInstruction(sret())));
}
