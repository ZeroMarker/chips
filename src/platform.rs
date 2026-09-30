//! Predefined target platforms.
//!
//! An instruction-level test does not care where its addresses land, so it runs
//! on [`Memory::permissive`]. A conformance suite does care: it was linked
//! against a specific memory map and expects `tohost` to be where its linker
//! script put it. This module describes those maps so the model and the suite
//! agree by construction rather than by a comment staying correct.

use std::rc::Rc;

use crate::htif::Htif;
use crate::mem::{Memory, Permissions, Region};

/// Where `riscv-tests` puts things.
///
/// From `env/p/link.ld`:
///
/// ```text
/// . = 0x80000000;  .text.init : { *(.text.init) }
/// . = ALIGN(0x1000);  .tohost : { *(.tohost) }
/// . = ALIGN(0x1000);  .text   : { *(.text) }
/// ```
///
/// and from `RVTEST_DATA_BEGIN` in `env/p/riscv_test.h`, `tohost` and `fromhost`
/// are 8-byte `.dword`s, each 64-byte aligned, so they land at `0x8000_1000` and
/// `0x8000_1040`. Because the link script aligns to pages, the natural map is
/// three consecutive 4 KiB pages followed by the rest of RAM.
pub mod riscv_tests {
    /// First byte of `.text.init`, the reset vector.
    pub const TEXT_INIT: u32 = 0x8000_0000;
    /// Where `tohost` lands for a test whose `.text.init` fits in one page.
    ///
    /// This is the common case, not a guarantee — read the symbol.
    pub const TOHOST: u32 = 0x8000_1000;
    /// The `fromhost` acknowledgement register, 64 bytes after `tohost`.
    pub const FROMHOST_OFFSET: u64 = 0x40;

    /// Granularity the link script aligns to.
    pub const PAGE: u64 = 0x1000;

    /// RAM size above the device page. The suites are small, but `.data` and
    /// `.bss` sit above the device page and there is no upper bound worth
    /// guessing at, so leave room rather than tuning this against today's
    /// tests.
    pub const RAM_SIZE: u64 = 0x0100_0000;

    /// RAM below the reset vector, for a downward-growing stack.
    ///
    /// `link.ld` defines no stack and `RVTEST_CODE_BEGIN` leaves `sp` zeroed, so
    /// a test that pushes would fault below `0x8000_0000`. Mapping a megabyte
    /// below the reset vector gives such a test somewhere to go without
    /// inventing a stack pointer convention the suite does not have.
    pub const STACK_BASE: u32 = 0x8000_0000 - 0x0010_0000;
    /// 1 MiB of stack RAM.
    pub const STACK_SIZE: u64 = 0x0010_0000;
}

/// The `riscv-tests` platform: RAM, the HTIF pair, and stack RAM.
///
/// `tohost` cannot be a constant. `link.ld` places it at the first 4 KiB
/// boundary after `.text.init`, so a test whose `.text.init` fits in a page
/// gets `tohost` at `0x8000_1000` while a larger one gets `0x8000_2000` or
/// higher. A runner therefore has to read the address out of the linked image's
/// symbol table and pass it here, rather than trusting a number written down
/// once.
///
/// The regions are adjacent and non-overlapping. That matters: [`Memory`]
/// resolves an address to the first region that covers it, so a device window
/// drawn *inside* a RAM region would be unreachable and the suite would hang
/// instead of reporting a result.
pub fn riscv_tests(tohost: u32) -> (Memory, Rc<Htif>) {
    use riscv_tests as rt;

    assert!(
        tohost >= rt::TEXT_INIT && tohost.is_multiple_of(rt::PAGE as u32),
        "tohost = 0x{tohost:08x} is not a page-aligned address above the reset vector"
    );

    let htif = Rc::new(Htif::new());
    let mem = Memory::from_regions(vec![
        // Stack RAM, below the reset vector.
        Region::ram(rt::STACK_BASE, rt::STACK_SIZE, Permissions::READ_WRITE),
        // Everything from the reset vector up to the device page: `.text.init`,
        // and `.text` if the linker put any there.
        Region::ram(
            rt::TEXT_INIT,
            u64::from(tohost - rt::TEXT_INIT),
            Permissions::READ_WRITE,
        ),
        // The page holding tohost and fromhost. One page, because `link.ld`
        // aligns the next section to a page boundary too.
        Region::shared_device(tohost, rt::PAGE, Permissions::READ_WRITE, Rc::clone(&htif)),
        // `.text`, `.data`, `.bss`, and everything else above the device page.
        Region::ram(
            tohost + rt::PAGE as u32,
            rt::RAM_SIZE,
            Permissions::READ_WRITE,
        ),
    ]);
    (mem, htif)
}
