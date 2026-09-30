//! A stable per-instruction trace, for differential testing against RTL.
//!
//! ROADMAP P3 compares this model against a hardware implementation
//! instruction by instruction. That comparison is only possible if both sides
//! can be made to *say the same thing* about what happened, and the format has
//! to be fixed before the RTL exists — otherwise the hardware gets written
//! against a format that is still moving, and every disagreement turns into an
//! argument about the format rather than about the core.
//!
//! So this is deliberately not a debugging aid. It is a wire format, and it is
//! specified rather than emergent:
//!
//! # Grammar
//!
//! One line per executed step, in execution order. Lines start with a single
//! letter identifying the record kind, then a step number, then `key=value`
//! fields separated by single spaces. There is no other structure: no
//! continuation lines, no nesting, no trailing punctuation. A reader can parse a
//! line without knowing anything about the ones before it.
//!
//! ```text
//! # comments and the version line are the only lines not matching this
//! v1
//! i <step> pc=<8 hex> inst=<8 hex> [x<reg>=<8 hex>]... [c<csr>=<8 hex>]... [m<addr>=<8 hex>]...
//! t <step> pc=<8 hex> inst=<8 hex> cause=<decimal> tval=<8 hex>
//! x <step> pc=<8 hex> inst=<8 hex> x<reg>=<8 hex>
//! ```
//!
//! - `i` — an instruction retired normally. The `x` fields are the registers it
//!   wrote, the `c` fields the CSRs, the `m` fields the memory. All three groups
//!   are optional and a step that changes nothing simply omits them.
//! - `t` — a trap was taken. Cause and `tval` are the architectural values.
//! - `x` — the step did not retire: an exception, an interrupt, or a halt. The
//!   one register it wrote is named, which is how `mret`/`sret` re-enable an
//!   interrupt or `csrrw` publish an old value.
//!
//! Registers are `x0`–`x31` by number, not ABI name, because the ABI names are a
//! convention of the toolchain and this is a machine-to-machine comparison.
//!
//! # What is deliberately absent
//!
//! - **Cycle counts.** The model executes one instruction per step; RTL will not.
//!   Including timing would make the two traces differ on every line for a
//!   reason that says nothing about correctness.
//! - **Memory reads.** Only writes are recorded. A read that misses is already
//!   reported as a load access fault, and a read that hits cannot change state,
//!   so recording them would triple the trace size for no diagnostic value.
//! - **Unchanged state.** A register written with the value it already held is
//!   recorded, because the *write* is what the hardware also does; what is
//!   omitted is anything the instruction did not touch at all.

use std::fmt::Write as _;

/// A single state change made by one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A write to an integer register. `x0` never appears: writes to it are
    /// discarded and so are not changes.
    Register {
        /// Register number, 0–31.
        index: u32,
        /// The value written, after any truncation to 32 bits.
        value: u32,
    },
    /// A write to a control and status register.
    Csr {
        /// The CSR address.
        address: u32,
        /// The value the register holds afterwards, after WARL legalization.
        value: u32,
    },
    /// A store to memory. `value` holds the low `8 * len` bits, little-endian as
    /// written.
    Memory {
        /// The faulting-or-not address of the first byte.
        address: u32,
        /// The access width in bytes: 1, 2, 4, or 8.
        len: u32,
        /// The stored value.
        value: u64,
    },
}

/// Why a step did or did not retire.
///
/// Named `StepResult` rather than `Outcome` because [`crate::htif::Outcome`]
/// already uses that word for something unrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// The instruction retired.
    Retired,
    /// A trap was taken. The step did not retire.
    Trap {
        /// The architectural `mcause` value.
        cause: u32,
        /// The architectural `mtval` value.
        tval: u32,
    },
    /// The run stopped: `ecall` or `ebreak` with no handler installed. The
    /// instruction did not retire, and nothing will run again.
    Halt,
}

/// What happened during one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// 1-based index of the step, so a trace can be read against a step count.
    pub step: u64,
    /// The PC the step began at.
    pub pc: u32,
    /// The instruction word fetched, or the encoding that was rejected.
    pub instruction: u32,
    /// Whether the step retired, and why not if it did not.
    pub outcome: StepResult,
    /// Everything the step changed, in the order it happened.
    pub changes: Vec<Change>,
}

impl Record {
    /// Format this record as one line, without a trailing newline.
    pub fn to_line(&self) -> String {
        let mut line = String::with_capacity(64);
        match self.outcome {
            StepResult::Retired => {
                let _ = write!(
                    line,
                    "i {} pc={:08x} inst={:08x}",
                    self.step, self.pc, self.instruction
                );
            }
            StepResult::Trap { cause, tval } => {
                let _ = write!(
                    line,
                    "t {} pc={:08x} inst={:08x} cause={} tval={:08x}",
                    self.step, self.pc, self.instruction, cause, tval
                );
                // A trap also changes registers, so its changes follow on the
                // same line rather than being dropped.
                for change in &self.changes {
                    append_change(&mut line, change);
                }
                return line;
            }
            StepResult::Halt => {
                let _ = write!(
                    line,
                    "x {} pc={:08x} inst={:08x}",
                    self.step, self.pc, self.instruction
                );
            }
        }
        for change in &self.changes {
            append_change(&mut line, change);
        }
        line
    }
}

fn append_change(line: &mut String, change: &Change) {
    match change {
        Change::Register { index, value } => {
            let _ = write!(line, " x{index}={value:08x}");
        }
        Change::Csr { address, value } => {
            let _ = write!(line, " c{address:03x}={value:08x}");
        }
        Change::Memory {
            address,
            len,
            value,
        } => {
            // Mask to the access width, not merely pad to it: a `sw` of
            // 0xdeadbeef must not print as `deadbeef` for a one-byte store, and
            // zero-padding alone would print all eight digits regardless.
            let mask = if *len >= 8 {
                u64::MAX
            } else {
                (1u64 << (8 * len)) - 1
            };
            let _ = write!(
                line,
                " m{address:08x}={:0width$x}",
                value & mask,
                width = (*len as usize) * 2
            );
        }
    }
}

/// A consumer of [`Record`]s.
///
/// Takes `&mut self` so a sink can buffer and flush without interior mutability,
/// and so a caller can hand the CPU a sink it also owns.
pub trait Trace {
    /// Consume one step.
    fn record(&mut self, record: &Record);
}

/// A [`Trace`] that writes the textual format to an underlying writer.
#[derive(Debug)]
pub struct TextTrace<W: std::io::Write> {
    out: W,
    /// Whether the version line has been written yet.
    started: bool,
}

impl<W: std::io::Write> TextTrace<W> {
    /// Wrap `out`, emitting the version line before the first record.
    pub fn new(out: W) -> Self {
        TextTrace {
            out,
            started: false,
        }
    }

    /// Write the version line now rather than lazily. A harness that diffs two
    /// traces wants the header present even for an empty run.
    pub fn write_header(&mut self) -> std::io::Result<()> {
        if !self.started {
            writeln!(self.out, "v1")?;
            self.started = true;
        }
        Ok(())
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl<W: std::io::Write> Trace for TextTrace<W> {
    fn record(&mut self, record: &Record) {
        // A trace is diagnostic output, so a failure to write must not take the
        // simulation down with it. Losing trace lines is bad; aborting a run
        // because a pipe closed is worse.
        let _ = self.write_header();
        let _ = writeln!(self.out, "{}", record.to_line());
    }
}

/// A [`Trace`] that keeps the last `capacity` records in memory.
///
/// Cheaper than text for a harness that only wants to look at the end of a run,
/// which is where a divergence usually shows up.
#[derive(Debug)]
pub struct TailTrace {
    records: std::collections::VecDeque<Record>,
    capacity: usize,
}

impl TailTrace {
    /// Keep at most `capacity` records.
    pub fn new(capacity: usize) -> Self {
        TailTrace {
            records: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// The retained records, oldest first.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.iter()
    }

    /// The most recent record.
    pub fn last(&self) -> Option<&Record> {
        self.records.back()
    }
}

impl Trace for TailTrace {
    fn record(&mut self, record: &Record) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record.clone());
    }
}
