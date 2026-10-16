//! Minimal RV32IMZicsr command-line driver: load a raw binary image, run it,
//! and dump the final machine state.
//!
//! Usage: `chips <image.bin> [start_addr] [max_instructions] [--htif[=tohost]]`
//! - `image.bin`: raw instruction bytes (e.g. produced by linking a bare-metal
//!   RISC-V program and extracting the `.text`).
//! - `start_addr`: reset vector, decimal or `0x`-prefixed hex (default
//!   `0x80000000`).
//! - `max_instructions`: instruction budget before the run is reported as
//!   exhausted (default 10,000,000).
//! - `--htif`: run against the `riscv-tests` platform and report the result
//!   from the `tohost` register instead of dumping state. The optional
//!   `=tohost` value is the address of that register, which `riscv-tests` places
//!   at a page boundary chosen by the linker and so varies per test; read it
//!   from the image's symbol table.
//! - `--trace`: write a per-instruction trace to stdout and nothing else, in the
//!   format `docs/ARCHITECTURE.md` specifies. Intended for differential testing,
//!   so the state dump is suppressed rather than interleaved.
//!
//! - `--stop-pc=ADDR`: stop before executing this address; fail if not reached.
//! - `--state-log=FILE`: write independent `s1` snapshots initially and after
//!   every attempted step, including trap entry, without advancing the CPU.
//!
//! A program halts itself with `ecall`/`ebreak` when no trap handler is
//! installed; a trap is reported with its `mcause`/`mtval` and a nonzero exit
//! code.

use chips::cpu::{Cpu, StepOutcome, StopReason};
use chips::csr::addr as csr_addr;
use chips::htif::Outcome;
use chips::isa::reg_name;
use chips::mem::Memory;
use chips::platform;
use chips::trace::TextTrace;
use std::io::Write;
use std::process::ExitCode;

const DEFAULT_START: u32 = 0x8000_0000;
const DEFAULT_BUDGET: u64 = 10_000_000;

const USAGE: &str = "usage: chips <image.bin> [start_addr] [max_instructions] \
                   [--htif[=tohost_addr]] [--trace] [--stop-pc=ADDR] [--state-log=FILE]";

fn parse_u32(s: &str) -> Result<u32, String> {
    let clean = s.trim_start_matches("0x").trim_start_matches("0X");
    let radix = if clean.len() != s.len() { 16 } else { 10 };
    u32::from_str_radix(clean, radix).map_err(|e| format!("invalid number {s:?}: {e}"))
}

fn trap_cause_name(cause: u32) -> &'static str {
    match cause {
        0 => "instruction address misaligned",
        1 => "instruction access fault",
        2 => "illegal instruction",
        3 => "breakpoint",
        4 => "load address misaligned",
        5 => "load access fault",
        6 => "store address misaligned",
        7 => "store access fault",
        11 => "environment call from M-mode",
        _ => "trap",
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    // `--htif` may appear anywhere after the image, and may carry the tohost
    // address as `--htif=0x...`. The positional arguments keep their order, so
    // the flag does not shift them.
    let mut tohost = platform::riscv_tests::TOHOST;
    let mut htif_mode = false;
    let mut trace = false;
    let mut stop_pc = None;
    let mut state_path = None;
    let mut positional: Vec<&String> = Vec::new();
    for arg in &args[1..] {
        if let Some(value) = arg.strip_prefix("--stop-pc=") {
            match parse_u32(value) {
                Ok(address) => stop_pc = Some(address),
                Err(error) => {
                    eprintln!("error: --stop-pc: {error}");
                    return ExitCode::from(2);
                }
            }
            continue;
        }
        if let Some(value) = arg.strip_prefix("--state-log=") {
            state_path = Some(value);
            continue;
        }
        if arg == "--trace" {
            trace = true;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--htif=") {
            match parse_u32(value) {
                Ok(v) => tohost = v,
                Err(e) => {
                    eprintln!("error: --htif={value}: {e}");
                    return ExitCode::from(2);
                }
            }
            htif_mode = true;
        } else if arg == "--htif" {
            htif_mode = true;
        } else {
            positional.push(arg);
        }
    }
    if positional.is_empty() || positional.len() > 3 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let bytes = match std::fs::read(positional[0]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", positional[0], e);
            return ExitCode::from(2);
        }
    };

    let base = match positional.get(1) {
        Some(s) => match parse_u32(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        None => DEFAULT_START,
    };

    let budget = match positional.get(2) {
        Some(s) => match parse_u32(s) {
            Ok(v) => u64::from(v),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        None => DEFAULT_BUDGET,
    };

    if htif_mode && stop_pc.is_some() {
        eprintln!("error: --stop-pc and --htif cannot be combined");
        return ExitCode::from(2);
    }
    let mut state_log = match state_path.map(std::fs::File::create).transpose() {
        Ok(file) => file,
        Err(error) => {
            eprintln!("error: cannot create state log: {error}");
            return ExitCode::from(2);
        }
    };
    if htif_mode {
        return run_htif(&bytes, base, budget, tohost, trace, &mut state_log);
    }

    let mut mem = Memory::permissive();
    if let Err(fault) = mem.load_image(base, &bytes) {
        eprintln!(
            "error: image does not fit the address space at 0x{base:08x} \
             ({} bytes, {} fault at 0x{:08x})",
            bytes.len(),
            fault.access.name(),
            fault.addr
        );
        return ExitCode::from(2);
    }

    let mut cpu = Cpu::new();
    cpu.set_pc(base);
    attach_trace(&mut cpu, trace);

    if !record_state(&cpu, &mut state_log) {
        return ExitCode::from(2);
    }
    let mut result = Ok(StopReason::Limit);
    for _ in 0..budget {
        if stop_pc == Some(cpu.pc()) {
            break;
        }
        let step = cpu.step(&mut mem);
        if !record_state(&cpu, &mut state_log) {
            return ExitCode::from(2);
        }
        match step {
            Ok(StepOutcome::Continue | StepOutcome::TrapTaken) => {}
            Ok(StepOutcome::Ecall) => {
                result = Ok(StopReason::Ecall);
                break;
            }
            Ok(StepOutcome::Ebreak) => {
                result = Ok(StopReason::Ebreak);
                break;
            }
            Err(trap) => {
                result = Err(trap);
                break;
            }
        }
    }
    if stop_pc.is_some() && stop_pc != Some(cpu.pc()) && result.is_ok() {
        eprintln!("FAIL: checkpoint not reached within {budget} steps");
        return ExitCode::from(1);
    }
    match result {
        Ok(reason) => {
            if !trace {
                if stop_pc == Some(cpu.pc()) {
                    println!("stopped: Checkpoint");
                } else {
                    println!("stopped: {reason:?}");
                }
                dump_state(&cpu);
            }
            ExitCode::SUCCESS
        }
        Err(trap) => {
            eprintln!(
                "trap at pc=0x{:08x}: {trap:?} (mcause={} {})",
                cpu.pc(),
                trap.mcause(),
                trap_cause_name(trap.mcause())
            );
            eprintln!("  mtval=0x{:08x}", trap.mtval());
            eprintln!("  no trap handler was installed (mtvec = 0), so the run stopped here");
            if !trace {
                dump_state(&cpu);
            }
            ExitCode::from(1)
        }
    }
}

/// Attach a stdout trace if `trace` was requested.
fn attach_trace(cpu: &mut Cpu, trace: bool) {
    if trace {
        cpu.set_trace(Some(Box::new(TextTrace::new(std::io::stdout()))));
    }
}

fn record_state(cpu: &Cpu, log: &mut Option<std::fs::File>) -> bool {
    if let Some(file) = log {
        if let Err(error) = writeln!(file, "{}", cpu.state_line()) {
            eprintln!("error: cannot write state log: {error}");
            return false;
        }
    }
    true
}

fn run_htif(
    bytes: &[u8],
    base: u32,
    budget: u64,
    tohost: u32,
    trace: bool,
    state_log: &mut Option<std::fs::File>,
) -> ExitCode {
    let (mut mem, htif, _clint) = platform::riscv_tests(tohost);
    if let Err(fault) = mem.load_image(base, bytes) {
        eprintln!(
            "error: image does not fit the riscv-tests platform at 0x{base:08x} \
             ({} bytes, {} fault at 0x{:08x})",
            bytes.len(),
            fault.access.name(),
            fault.addr
        );
        return ExitCode::from(2);
    }

    let mut cpu = Cpu::new();
    cpu.set_pc(base);
    attach_trace(&mut cpu, trace);

    if !record_state(&cpu, state_log) {
        return ExitCode::from(2);
    }
    for step in 1..=budget {
        let executed = cpu.step(&mut mem);
        if !record_state(&cpu, state_log) {
            return ExitCode::from(2);
        }
        let outcome = match executed {
            Ok(_) => htif.outcome(),
            Err(trap) => {
                eprintln!(
                    "FAIL: trap at pc=0x{:08x} after {step} instructions: {trap:?} \
                     (mcause={} {})",
                    cpu.pc(),
                    trap.mcause(),
                    trap_cause_name(trap.mcause())
                );
                eprintln!("  mtval=0x{:08x}", trap.mtval());
                if !trace {
                    dump_state(&cpu);
                }
                return ExitCode::from(1);
            }
        };

        if outcome.is_final() {
            if trace && outcome == Outcome::Pass {
                return ExitCode::SUCCESS;
            }
            return report_htif(&cpu, outcome, step);
        }
    }

    eprintln!("FAIL: no result in tohost after {budget} instructions");
    if !trace {
        dump_state(&cpu);
    }
    ExitCode::from(1)
}

fn report_htif(cpu: &Cpu, outcome: Outcome, steps: u64) -> ExitCode {
    match outcome {
        Outcome::Pass => {
            println!(
                "PASS ({steps} instructions, {instret} retired)",
                instret = cpu.instret()
            );
            ExitCode::SUCCESS
        }
        Outcome::Fail { test } => {
            eprintln!("FAIL: check {test} ({steps} instructions)");
            ExitCode::from(1)
        }
        Outcome::UnhandledException { test, raw } => {
            match test {
                Some(test) => eprintln!(
                    "FAIL: took an unexpected exception during check {test} \
                     (tohost = 0x{raw:016x}, {steps} instructions)"
                ),
                None => eprintln!(
                    "FAIL: took an unexpected exception before the first check \
                     (tohost = 0x{raw:016x}, {steps} instructions)"
                ),
            }
            ExitCode::from(1)
        }
        Outcome::NotACommand { raw } => {
            eprintln!("FAIL: tohost = 0x{raw:016x} is not a valid command (bit 0 clear)");
            ExitCode::from(1)
        }
        Outcome::Pending => {
            eprintln!("FAIL: no result");
            ExitCode::from(1)
        }
    }
}

fn dump_state(cpu: &Cpu) {
    println!("pc = 0x{:08x}", cpu.pc());
    for i in 0..32u32 {
        println!("x{i:02} {:<4} {:08x}", reg_name(i), cpu.reg(i));
    }
    println!(
        "cycle = {}  instret = {}  time = {}",
        cpu.cycle(),
        cpu.instret(),
        cpu.mtime()
    );
    if cpu.handler_installed() {
        println!(
            "mtvec = 0x{:08x}  mepc = 0x{:08x}  mcause = {}  mtval = 0x{:08x}  mstatus = 0x{:08x}",
            cpu.csr().read(csr_addr::MTVEC),
            cpu.csr().read(csr_addr::MEPC),
            cpu.csr().read(csr_addr::MCAUSE),
            cpu.csr().read(csr_addr::MTVAL),
            cpu.csr().read(csr_addr::MSTATUS),
        );
    }
    println!("misa = 0x{:08x}", cpu.csr().read(csr_addr::MISA));
}
