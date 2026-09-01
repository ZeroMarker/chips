//! Byte-addressable memory model for the functional model.
//!
//! Implemented as a sparse map so any address may hold a byte without a fixed
//! address-space limit. Reads of unwritten addresses return `0`. The model
//! performs little-endian accesses and does **not** raise a trap on
//! misaligned accesses — a deliberate simplification for a functional model
//! (the hardware RTL is expected to trap; see `ROADMAP.md`, phase P2).

use std::collections::BTreeMap;

/// A sparse, little-endian byte-addressable memory.
#[derive(Debug, Default)]
pub struct Memory {
    data: BTreeMap<u32, u8>,
}

impl Memory {
    /// Create an empty memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a single byte; returns 0 for unwritten addresses.
    pub fn read_u8(&self, addr: u32) -> u8 {
        self.data.get(&addr).copied().unwrap_or(0)
    }

    /// Write a single byte.
    pub fn write_u8(&mut self, addr: u32, val: u8) {
        self.data.insert(addr, val);
    }

    /// Read `len` (1, 2, 4 or 8) bytes little-endian starting at `addr`.
    pub fn read_bytes(&self, addr: u32, len: u32) -> u64 {
        let mut v = 0u64;
        for i in 0..len {
            v |= (self.read_u8(addr.wrapping_add(i)) as u64) << (8 * i);
        }
        v
    }

    /// Write `len` (1, 2, 4 or 8) bytes little-endian starting at `addr`.
    /// Only the low `8 * len` bits of `val` are used. For `len` < 8 the high
    /// bytes of `val` are ignored.
    pub fn write_bytes(&mut self, addr: u32, len: u32, val: u64) {
        for i in 0..len {
            let byte = ((val >> (8 * i)) & 0xff) as u8;
            self.write_u8(addr.wrapping_add(i), byte);
        }
    }

    /// Convenience: read a 32-bit little-endian word.
    pub fn load_u32(&self, addr: u32) -> u32 {
        self.read_bytes(addr, 4) as u32
    }

    /// Convenience: store a 32-bit little-endian word.
    pub fn store_u32(&mut self, addr: u32, val: u32) {
        self.write_bytes(addr, 4, val as u64);
    }

    /// Load a raw binary image at `base` address, one byte at a time.
    pub fn load_image(&mut self, base: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            self.write_u8(base.wrapping_add(i as u32), *b);
        }
    }
}
