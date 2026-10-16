//! Driver-level tests for `src/main.rs`: argument handling, exit codes, and the
//! machine-state dump.
//!
//! These run the real binary through the file system, so they cover what the
//! library tests cannot reach: argument parsing, the written state report, and
//! the exit status a script or CI job would see.

mod common;

use common::*;
use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_chips");

/// Write `words` as a little-endian raw image and return its path.
fn image(name: &str, words: &[u32]) -> PathBuf {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    let path = std::env::temp_dir().join(format!("chips-cli-{}-{name}.bin", std::process::id()));
    std::fs::write(&path, bytes).expect("write the test image");
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("run chips")
}

/// Extract register `reg` from the driver's dump.
fn reg(stdout: &str, reg: u32) -> String {
    let tag = format!("x{reg:02}");
    stdout
        .lines()
        .find(|line| line.split_whitespace().next() == Some(tag.as_str()))
        .and_then(|line| line.split_whitespace().nth(2))
        .unwrap_or_else(|| panic!("no x{reg} in output:\n{stdout}"))
        .to_string()
}

/// The line reporting the counters, exactly as the driver prints it.
fn counter_line(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|line| line.starts_with("cycle ="))
        .unwrap_or_else(|| panic!("no counter line in output:\n{stdout}"))
}

#[test]
fn usage_is_reported_without_arguments() {
    let out = run(&[]);

    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: chips"));
}

#[test]
fn runs_a_program_and_dumps_machine_state() {
    let path = image(
        "halt",
        &[
            li(5, 42),             // addi x5, x0, 42
            addi(6, 5, 8),         // addi x6, x5, 8
            r(0, 6, 5, 0b000, 7),  // add  x7, x5, x6
            store(0, 7, 0, 0b010), // sw  x7, 0(x0)
            load(0, 0, 0b010, 8),  // lw   x8, 0(x0)
            ebreak(),
        ],
    );

    let out = run(&[path.to_str().unwrap(), "0x1000"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "a halting program exits 0");
    assert!(stdout.contains("stopped: Ebreak"));
    assert!(stdout.contains("pc = 0x00001014"));
    assert_eq!(reg(&stdout, 5), "0000002a");
    assert_eq!(reg(&stdout, 6), "00000032");
    assert_eq!(reg(&stdout, 7), "0000005c");
    assert_eq!(reg(&stdout, 8), "0000005c", "loaded back from memory");
    assert!(
        stdout.contains("x05 t0"),
        "registers are named by their ABI names"
    );
    assert!(stdout.contains("x10 a0"));
    assert_eq!(
        counter_line(&stdout),
        "cycle = 6  instret = 5  time = 6",
        "ebreak counts as a step but does not retire"
    );
    assert!(stdout.contains("misa = 0x40001100"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn accepts_a_decimal_start_address() {
    let path = image("decimal", &[li(5, 1), ebreak()]);

    // 4096 == 0x1000; the driver takes decimal and 0x-prefixed hex.
    let out = run(&[path.to_str().unwrap(), "4096"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success());
    assert!(stdout.contains("pc = 0x00001004"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn reports_a_trap_with_its_cause_and_mtval() {
    // A reserved `slli` encoding: `funct7` is not one the ISA defines, so this
    // traps as an illegal instruction. It used to be a misaligned load, but
    // misaligned data accesses are now completed rather than trapped.
    let illegal = 0x0200_1013u32;
    let path = image("trap", &[illegal, ebreak()]);

    let out = run(&[path.to_str().unwrap(), "0x1000"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(1), "a trap exits nonzero");
    assert!(stderr.contains("IllegalInstruction(33558547)"), "{stderr}");
    assert!(stderr.contains("mcause=2"));
    assert!(stderr.contains("illegal instruction"));
    assert!(
        stderr.contains("mtval=0x02001013"),
        "an illegal instruction reports its own encoding as mtval"
    );
    assert!(
        stdout.contains("pc = 0x00001000"),
        "a trap without a handler leaves the PC where it faulted"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_misaligned_load_is_completed_not_trapped() {
    // The driver-level counterpart to the model-level test of the same name.
    // `lw x6, 0(x5)` with `x5 = 3` reads across a word boundary and must
    // succeed, so the run ends on the `ebreak` with exit code 0.
    let path = image("misaligned", &[li(5, 3), load(0, 5, 0b010, 6), ebreak()]);

    let out = run(&[path.to_str().unwrap(), "0x1000"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        out.status.code(),
        Some(0),
        "a completed misaligned load is not a failure"
    );
    assert!(stdout.contains("stopped: Ebreak"), "{stdout}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn stops_on_the_instruction_limit() {
    let path = image("limit", &[jal(0, 0)]); // jal x0, +0 — spins forever

    let out = run(&[path.to_str().unwrap(), "0x1000", "3"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "an exhausted budget is not a trap");
    assert!(stdout.contains("stopped: Limit"));
    assert!(
        stdout.contains("pc = 0x00001000"),
        "the spin never advances"
    );
    assert_eq!(counter_line(&stdout), "cycle = 3  instret = 3  time = 3");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rejects_an_invalid_argument() {
    let path = image("bad-arg", &[ebreak()]);

    let out = run(&[path.to_str().unwrap(), "0xzz"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid number"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn reports_a_missing_image() {
    let missing = std::env::temp_dir().join("chips-cli-image-that-does-not-exist.bin");
    let _ = std::fs::remove_file(&missing);

    let out = run(&[missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot read"));
}

#[test]
fn checkpoint_stops_before_execution_and_records_independent_states() {
    let path = image("checkpoint", &[li(5, 42), li(6, 99), ebreak()]);
    let log = path.with_extension("state");
    let out = run(&[
        path.to_str().unwrap(),
        "0x1000",
        "1",
        "--trace",
        "--stop-pc=0x1004",
        &format!("--state-log={}", log.display()),
    ]);
    assert!(out.status.success(), "{:?}", out);
    let trace = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        trace.lines().count(),
        2,
        "version and one retired step only"
    );
    assert!(trace.starts_with("v1\ni 1 "));
    let text = std::fs::read_to_string(&log).unwrap();
    let states: Vec<_> = text.lines().collect();
    assert_eq!(states.len(), 2, "initial and post-step states");
    assert!(states[0].contains("pc=00001000 priv=3"));
    assert!(states[0].contains("x5=00000000"));
    assert!(states[1].contains("pc=00001004 priv=3"));
    assert!(states[1].contains("x5=0000002a x6=00000000"));
    assert!(
        states[1].contains("cb00=00000001"),
        "observation does not tick counters"
    );
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(log);
}

#[test]
fn checkpoint_at_entry_needs_no_steps_even_with_zero_budget() {
    let path = image("entry-checkpoint", &[li(5, 42)]);
    let out = run(&[path.to_str().unwrap(), "0x1000", "0", "--stop-pc=4096"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("stopped: Checkpoint"));
    assert_eq!(reg(&stdout, 5), "00000000");
    assert_eq!(counter_line(&stdout), "cycle = 0  instret = 0  time = 0");
    let _ = std::fs::remove_file(path);
}

#[test]
fn unreached_checkpoint_fails_on_budget_or_early_halt() {
    for (name, word) in [
        ("spin-checkpoint", jal(0, 0)),
        ("halt-checkpoint", ebreak()),
    ] {
        let path = image(name, &[word]);
        let out = run(&[path.to_str().unwrap(), "0x1000", "2", "--stop-pc=0x2000"]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("checkpoint not reached"));
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn failed_step_records_trap_state_without_polluting_trace() {
    let path = image("trap-state", &[0xffff_ffff]);
    let log = path.with_extension("state");
    let out = run(&[
        path.to_str().unwrap(),
        "0x1000",
        "1",
        "--trace",
        &format!("--state-log={}", log.display()),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 2);
    assert!(stdout.starts_with("v1\nt 1 "));
    let states = std::fs::read_to_string(&log).unwrap();
    assert_eq!(states.lines().count(), 2);
    let last = states.lines().last().unwrap();
    assert!(last.contains("c341=00000000 c342=00000000 c343=00000000"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(log);
}

#[test]
fn state_log_observations_do_not_change_execution() {
    let path = image("observer", &[li(5, 7), addi(5, 5, 1), ebreak()]);
    let log = path.with_extension("state");
    let plain = run(&[path.to_str().unwrap(), "0x1000"]);
    let observed = run(&[
        path.to_str().unwrap(),
        "0x1000",
        &format!("--state-log={}", log.display()),
    ]);
    assert!(plain.status.success() && observed.status.success());
    assert_eq!(plain.stdout, observed.stdout);
    assert_eq!(plain.stderr, observed.stderr);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(log);
}

#[test]
fn invalid_state_log_destination_is_a_usage_error() {
    let path = image("bad-state-path", &[ebreak()]);
    let out = run(&[
        path.to_str().unwrap(),
        &format!("--state-log={}", std::env::temp_dir().display()),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot create state log"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn checkpoint_and_htif_cannot_silently_override_each_other() {
    let path = image("conflicting-stops", &[ebreak()]);
    let out = run(&[path.to_str().unwrap(), "--htif", "--stop-pc=0x1000"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be combined"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn delivered_trap_snapshot_includes_implicit_csr_changes() {
    let path = image(
        "delivered-state",
        &[li(5, 0x100), csrrw(0x305, 5, 0), 0xffff_ffff],
    );
    let log = path.with_extension("state");
    let out = run(&[
        path.to_str().unwrap(),
        "0x1000",
        "3",
        "--trace",
        "--stop-pc=0x100",
        &format!("--state-log={}", log.display()),
    ]);
    assert!(out.status.success(), "{:?}", out);
    let trace = String::from_utf8(out.stdout).unwrap();
    assert_eq!(trace.lines().count(), 4);
    assert!(trace.lines().last().unwrap().starts_with("t 3 "));
    let states = std::fs::read_to_string(&log).unwrap();
    assert_eq!(states.lines().count(), 4);
    let last = states.lines().last().unwrap();
    assert!(last.contains("pc=00000100 priv=3"));
    assert!(last.contains("c341=00001008 c342=00000002 c343=ffffffff"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(log);
}
