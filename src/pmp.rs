//! Physical memory protection for a 32-bit physical address space.
//!
//! Sixteen entries, four-byte grain (G=0), OFF/TOR/NA4/NAPOT matching.
//! Entries are checked in index order against the entire memory operation,
//! before any RAM or device access. Host-side image loading bypasses PMP.

use crate::isa::Privilege;
use crate::mem::Access;

/// Number of implemented PMP entries.
pub const ENTRIES: usize = 16;
const ADDRESS_MASK: u32 = 0x3fff_ffff;
const LOCK: u8 = 0x80;
const TOR: u8 = 0x08;
const MODE: u8 = 0x18;

/// PMP register state. Reset disables every entry; S/U accesses then fail.
#[derive(Debug, Default)]
pub struct Pmp {
    config: [u8; ENTRIES],
    address: [u32; ENTRIES],
}

impl Pmp {
    /// Read one RV32 packed configuration register (index 0..4).
    pub fn read_config(&self, index: usize) -> u32 {
        let mut result = 0;
        for byte in 0..4 {
            result |= u32::from(self.config[index * 4 + byte]) << (byte * 8);
        }
        result
    }

    /// Write a packed configuration CSR, preserving each locked byte.
    /// Reserved bits read zero; the reserved W=1,R=0 combination clears W.
    pub fn write_config(&mut self, index: usize, value: u32) {
        for byte in 0..4 {
            let entry = &mut self.config[index * 4 + byte];
            if *entry & LOCK != 0 {
                continue;
            }
            let mut next = ((value >> (byte * 8)) as u8) & 0x9f;
            if next & 3 == 2 {
                next &= !2;
            }
            *entry = next;
        }
    }

    /// Read `pmpaddr[index]` (index 0..16), in four-byte address units.
    pub fn read_address(&self, index: usize) -> u32 {
        self.address[index]
    }

    /// Write an address unless this entry, or its TOR successor, locks it.
    /// Only physical address bits 31:2 are implemented.
    pub fn write_address(&mut self, index: usize, value: u32) {
        let locked = self.config[index] & LOCK != 0;
        let tor_successor_locked =
            index + 1 < ENTRIES && self.config[index + 1] & (LOCK | MODE) == LOCK | TOR;
        if !locked && !tor_successor_locked {
            self.address[index] = value & ADDRESS_MASK;
        }
    }

    /// Whether one complete memory operation is allowed at effective privilege.
    /// A partial match fails even for an unlocked M-mode entry. No match allows
    /// M-mode and denies S/U. Memory's own map and permissions still apply.
    pub fn permits(&self, privilege: Privilege, addr: u32, len: u32, access: Access) -> bool {
        let start = u64::from(addr);
        let end = start + u64::from(len);
        if len == 0 || end > 1 << 32 {
            return false;
        }
        for index in 0..ENTRIES {
            let config = self.config[index];
            let address = u64::from(self.address[index]) << 2;
            let (bottom, top) = match config & MODE {
                0 => continue,
                TOR => (
                    if index == 0 {
                        0
                    } else {
                        u64::from(self.address[index - 1]) << 2
                    },
                    address,
                ),
                0x10 => (address, address + 4),
                0x18 => {
                    let size = 1u64 << (self.address[index].trailing_ones() + 3);
                    let bottom = address & !(size - 1);
                    (bottom, bottom + size)
                }
                _ => unreachable!(),
            };
            if bottom >= top || start >= top || end <= bottom {
                continue;
            }
            if start < bottom || end > top {
                return false;
            }
            let permission = match access {
                Access::Load => 1,
                Access::Store => 2,
                Access::Fetch => 4,
            };
            return (privilege == Privilege::Machine && config & LOCK == 0)
                || config & permission != 0;
        }
        privilege == Privilege::Machine
    }
}
