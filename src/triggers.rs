//! Two native address triggers using the legacy type-2 mcontrol encoding.
//!
//! Only equality, address selection, any size, before timing, and breakpoint
//! exception action are implemented. Unsupported fields read zero. No external
//! Debug Mode, chaining, data matching, context filtering, or hit latch exists.

use crate::isa::Privilege;
use crate::mem::Access;

/// Number of independently configurable triggers.
pub const COUNT: usize = 2;
const TYPE: u32 = 2 << 28;
const CONTROL_MASK: u32 = (1 << 6) | (1 << 4) | (1 << 3) | 7;

/// The trigger CSR bank. Trigger state belongs to the hart, not the memory map.
#[derive(Debug)]
pub struct Triggers {
    selected: usize,
    control: [u32; COUNT],
    address: [u32; COUNT],
}

impl Default for Triggers {
    fn default() -> Self {
        Self {
            selected: 0,
            control: [TYPE; COUNT],
            address: [0; COUNT],
        }
    }
}

impl Triggers {
    /// Read a trigger CSR by offset from tselect: 0..=4.
    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0 => self.selected as u32,
            1 => self.control[self.selected],
            2 => self.address[self.selected],
            3 => 0,                    // textra32: context selection is not implemented
            4 => (1 << 24) | (1 << 2), // version 1, type 2 only
            _ => unreachable!("unsupported trigger CSR offset"),
        }
    }

    /// WARL writes affect only the selected trigger. An invalid selector keeps
    /// its old value so enumeration terminates by readback. Type is fixed at 2;
    /// zero or an unsupported type disables the trigger without erasing tdata2.
    pub fn write(&mut self, offset: u32, value: u32) {
        match offset {
            0 if value < COUNT as u32 => self.selected = value as usize,
            1 => {
                self.control[self.selected] = TYPE
                    | if value >> 28 == 2 {
                        value & CONTROL_MASK
                    } else {
                        0
                    }
            }
            2 => self.address[self.selected] = value,
            0 | 3 | 4 => {}
            _ => unreachable!("unsupported trigger CSR offset"),
        }
    }

    /// Find the address matched by an enabled access/privilege combination.
    /// Each byte is a compare address, using wide arithmetic to avoid wrapping
    /// a cross-boundary access onto address zero. Lowest trigger index wins.
    /// Global native-trigger gates are enforced by the CPU before this call.
    pub fn matched(
        &self,
        privilege: Privilege,
        addr: u32,
        len: u32,
        access: Access,
    ) -> Option<u32> {
        let mode = match privilege {
            Privilege::Machine => 1 << 6,
            Privilege::Supervisor => 1 << 4,
            Privilege::User => 1 << 3,
        };
        let operation = match access {
            Access::Fetch => 4,
            Access::Load => 1,
            Access::Store => 2,
        };
        (0..COUNT).find_map(|index| {
            let control = self.control[index];
            let target = self.address[index];
            (control & mode != 0
                && control & operation != 0
                && u64::from(target) >= u64::from(addr)
                && u64::from(target) < u64::from(addr) + u64::from(len))
            .then_some(target)
        })
    }
}
