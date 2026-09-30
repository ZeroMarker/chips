//! Minimal RV32IMZicsr command-line driver: load a raw binary image, run it,
//! and dump the final machine state.
//!
//! Usage: `chips <image.bin> [start_addr] [max_instructions]`
//! - `image.bin`: raw instruction bytes (e.g. produced by linking a bare-metal
//!   RISC-V program and extracting the `.text`).
//! - `start_addr`: reset vector, decimal or `0x`-prefixed hex (default
//!   `0x80000000`).
//! - `max_instructions`: instruction budget before the run is reported as
//!   exhausted (default 10,000,000).
//!
//! A program halts itself with `ecall`/`ebreak` when no trap handler is
//! installed; a trap is reported with its `mcause`/`mtval` and a nonzero exit
//! code.

use chips::cpu::Cpu;
use chips::csr::addr as csr_addr;
use chips::isa::reg_name;
use chips::mem::Memory;
use std::process::ExitCode;

const DEFAULT_START: u32 = 0x8000_0000;
const DEFAULT_BUDGET: u64 = 10_000_000;

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
        eprintln!("usage: chips <image.bin> [start_addr] [max_instructions]");
        return ExitCode::from(2);
    }

    let bytes = match std::fs::read(&args[1]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", args[1], e);
            return ExitCode::from(2);
        }
    };

    let base = match args.get(2) {
        Some(s) => match parse_u32(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        None => DEFAULT_START,
    };

    let budget = match args.get(3) {
        Some(s) => match parse_u32(s) {
            Ok(v) => u64::from(v),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        None => DEFAULT_BUDGET,
    };

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

    match cpu.run(&mut mem, budget) {
        Ok(reason) => {
            println!("stopped: {reason:?}");
            dump_state(&cpu);
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
            dump_state(&cpu);
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
