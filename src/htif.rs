//! The HTIF test-completion protocol used by `riscv-tests`.
//!
//! `riscv-tests` does not exit. On success or failure it writes a 64-bit
//! command to the `tohost` register and then spins forever, so the environment
//! running it has to notice the write and decide what it means. This module is
//! that environment side.
//!
//! # The convention
//!
//! `RVTEST_PASS` writes `1`. `RVTEST_FAIL` writes `(test_number << 1) | 1`, so
//! any odd value greater than one is a failure carrying the number of the check
//! that failed. A test that takes an exception it cannot handle ORs `1337` into
//! its test number before writing, which surfaces as an implausibly large number
//! rather than a plausible one — worth distinguishing when reporting.
//!
//! Both registers are 64-bit, but the tests write them with `sw`, a word at a
//! time: the low half first, then zero to the high half. So the device has to
//! assemble the halves rather than treat a write as a whole-register store.

use std::cell::Cell;

use crate::mem::Device;

/// The marker a test ORs into its test number when it takes an exception it
/// cannot handle. From `RVTEST_CODE_BEGIN`:
///
/// ```c
/// other_exception:
/// 1:    ori TESTNUM, TESTNUM, 1337;
/// write_tohost:
///        sw TESTNUM, tohost, t5;
/// ```
///
/// So this is *not* written through the `(test << 1) | 1` encoding that
/// `RVTEST_FAIL` uses — the raw value is stored. Reporting it as "check 668"
/// instead of "took an unexpected exception" is exactly the kind of misleading
/// output a runner exists to avoid.
///
/// # The encoding is ambiguous, and that is the suite's fault
///
/// `(668 << 1) | 1` is 1337, so a test failing *check 668* is indistinguishable
/// from a test that took an unexpected exception before its first check. This
/// model reports the latter, which is the more likely cause and the more useful
/// thing to be told. No suite ships a check numbered 668, so nothing is lost in
/// practice.
///
/// For the same reason the test number recovered from a marker is best-effort:
/// the OR destroys any bits the test number shares with 1337, so numbers below 4
/// survive intact and larger ones may not. The raw value is always reported so
/// nothing has to be taken on trust.
const UNHANDLED_EXCEPTION_MARKER: u64 = 1337;

/// What a write to `tohost` meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing has been written yet.
    Pending,
    /// `tohost` was written with `1`.
    Pass,
    /// `tohost` was written with `(test << 1) | 1` for a `test` other than 0.
    Fail { test: u32 },
    /// The test took an exception it has no handler for. `test` is the test
    /// number it was running, which may itself be `None` if it had not got as
    /// far as setting one.
    UnhandledException { test: Option<u32>, raw: u64 },
    /// `tohost` was written with a value that is not a valid command, i.e. with
    /// bit 0 clear. The HTIF encoding says no command is present.
    NotACommand { raw: u64 },
}

impl Outcome {
    /// Did the test report a result? The runner stops here and does not care
    /// which result it was.
    pub fn is_final(self) -> bool {
        !matches!(self, Outcome::Pending)
    }
}

/// The `tohost`/`fromhost` pair, 8 bytes each.
#[derive(Debug)]
pub struct Htif {
    tohost: Cell<u64>,
    fromhost: Cell<u64>,
}

impl Default for Htif {
    fn default() -> Self {
        Self::new()
    }
}

impl Htif {
    /// A fresh pair with no command written.
    pub fn new() -> Self {
        Htif {
            tohost: Cell::new(0),
            fromhost: Cell::new(0),
        }
    }

    /// Interpret the current `tohost` value.
    pub fn outcome(&self) -> Outcome {
        let raw = self.tohost.get();
        if raw == 0 {
            return Outcome::Pending;
        }
        if raw & 1 == 0 {
            return Outcome::NotACommand { raw };
        }
        if raw == 1 {
            return Outcome::Pass;
        }
        // The marker is ORed into the test number, so it survives the `<< 1`
        // only in the sense that the raw value has all of 1337's bits set.
        if raw & UNHANDLED_EXCEPTION_MARKER == UNHANDLED_EXCEPTION_MARKER {
            let remaining = raw & !UNHANDLED_EXCEPTION_MARKER;
            return Outcome::UnhandledException {
                // The test number, had the OR not clobbered it. Zero means the
                // test had not set one, which is normal for a fault during setup.
                test: (remaining != 0).then_some((remaining >> 1) as u32),
                raw,
            };
        }
        Outcome::Fail {
            test: (raw >> 1) as u32,
        }
    }

    /// Acknowledge the command, as the host half of the protocol does.
    ///
    /// `riscv-tests` never reads `fromhost`, so this is not needed to get a
    /// result; it exists so a guest that polls for an acknowledgement does not
    /// spin forever.
    pub fn acknowledge(&self) {
        let command = self.tohost.get();
        self.fromhost.set(command);
    }
}

impl Device for Htif {
    fn read(&self, offset: u32, len: u32) -> u64 {
        let base = match offset {
            // The tests address `tohost` and `fromhost` as symbols 8 bytes
            // apart, but read them a word at a time.
            0..=3 => self.tohost.get() & 0xFFFF_FFFF,
            4..=7 => (self.tohost.get() >> 32) & 0xFFFF_FFFF,
            8..=11 => self.fromhost.get() & 0xFFFF_FFFF,
            12..=15 => (self.fromhost.get() >> 32) & 0xFFFF_FFFF,
            _ => 0,
        };
        let mask = if len >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * len)) - 1
        };
        base & mask
    }

    fn write(&self, offset: u32, len: u32, val: u64) {
        let value = val & ((1u64 << (8 * len.min(8))) - 1);
        match offset {
            0..=3 => {
                let low = value & 0xFFFF_FFFF;
                self.tohost.set((self.tohost.get() & !0xFFFF_FFFF) | low);
            }
            4..=7 => {
                let high = (value & 0xFFFF_FFFF) << 32;
                self.tohost.set((self.tohost.get() & 0xFFFF_FFFF) | high);
            }
            8..=11 => {
                let low = value & 0xFFFF_FFFF;
                self.fromhost
                    .set((self.fromhost.get() & !0xFFFF_FFFF) | low);
            }
            12..=15 => {
                let high = (value & 0xFFFF_FFFF) << 32;
                self.fromhost
                    .set((self.fromhost.get() & 0xFFFF_FFFF) | high);
            }
            _ => {}
        }
    }
}
