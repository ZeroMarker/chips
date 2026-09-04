//! Minimal RV32IMZicsr command-line driver: load a raw binary image, run it,
//! and dump the final register state.
//!
//! Usage: `chips <image.bin> [start_addr]`
//! - `image.bin`: raw instruction bytes (e.g. produced by linking a bare-metal
//!   RISC-V program and extracting the `.text`).
//! - `start_addr`: reset vector in hex (default `0x80000000`).

use chips::cpu::Cpu;
use chips::mem::Memory;
use std::process::ExitCode;

fn parse_addr(s: &str) -> u32 {
    let clean = s.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(clean, 16).expect("invalid address")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: chips <image.bin> [start_addr]");
        return ExitCode::from(2);
    }

    let bytes = match std::fs::read(&args[1]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", args[1], e);
            return ExitCode::from(2);
        }
    };

    let base = args.get(2).map(|s| parse_addr(s)).unwrap_or(0x8000_0000);

    let mut mem = Memory::new();
    mem.load_image(base, &bytes);

    let mut cpu = Cpu::new();
    cpu.set_pc(base);

    match cpu.run(&mut mem, 10_000_000) {
        Ok(reason) => {
            println!("stopped: {reason:?}");
            dump_regs(&cpu);
            ExitCode::SUCCESS
        }
        Err(trap) => {
            eprintln!("trap at pc=0x{:08x}: {trap:?}", cpu.pc());
            dump_regs(&cpu);
            ExitCode::from(1)
        }
    }
}

fn dump_regs(cpu: &Cpu) {
    println!("pc = 0x{:08x}", cpu.pc());
    for i in 0..32u32 {
        println!("x{i:02}  {:08x}", cpu.reg(i));
    }
}
