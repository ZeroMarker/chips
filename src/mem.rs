//! Byte-addressable memory with a decoded address map.
//!
//! Memory is a list of [`Region`]s, each with a base, a size, read/write
//! permissions, and a backing store. An access that no region covers, or that a
//! region does not permit, raises an [`AccessFault`], which the CPU turns into
//! `InstructionAccessFault`/`LoadAccessFault`/`StoreAccessFault`. This is what
//! makes an unmapped region modellable.
//!
//! Two ways to build a memory:
//!
//! - [`Memory::permissive`] covers the whole 32-bit address space with a sparse
//!   map and full permissions. Nothing can fault. This is the right choice for
//!   instruction-level tests, which care about arithmetic rather than about the
//!   platform.
//! - [`Memory::from_regions`] installs a decoded map. Only what the regions
//!   cover exists, so access faults are possible and a real target platform can
//!   be described.
//!
//! # Host-side setup is not a guest access
//!
//! Loading a program into memory is not something the guest does and must not
//! be able to fault, so the [`poke`](Memory::poke) family and [`peek`](Memory::peek)
//! family are unchecked. They panic on an address no region covers, because
//! that is a harness bug rather than guest behaviour. Every access the CPU
//! performs while executing goes through the fallible
//! [`fetch_u32`](Memory::fetch_u32)/[`load`](Memory::load)/[`store`](Memory::store).

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// What an access was attempting, for fault reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// An instruction fetch.
    Fetch,
    /// A read by a load instruction.
    Load,
    /// A write by a store instruction.
    Store,
}

impl Access {
    /// The `mcause` value for an access fault of this kind.
    pub fn mcause(self) -> u32 {
        match self {
            Access::Fetch => 1, // Instruction access fault
            Access::Load => 5,  // Load access fault
            Access::Store => 7, // Store access fault
        }
    }

    /// A short name for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Access::Fetch => "instruction fetch",
            Access::Load => "load",
            Access::Store => "store",
        }
    }
}

/// A refused access: the address, and what was being attempted there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessFault {
    /// The faulting address. This is also the value written to `mtval`.
    pub addr: u32,
    /// Whether the access was a fetch, load, or store.
    pub access: Access,
}

/// Read and write permissions of a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Loads and instruction fetches are permitted.
    pub read: bool,
    /// Stores are permitted.
    pub write: bool,
}

impl Permissions {
    /// Loads, fetches, and stores are permitted.
    pub const READ_WRITE: Permissions = Permissions {
        read: true,
        write: true,
    };
    /// Loads and fetches are permitted, stores are not.
    pub const READ_ONLY: Permissions = Permissions {
        read: true,
        write: false,
    };

    /// Does this permission set allow `access`?
    fn allows(self, access: Access) -> bool {
        match access {
            Access::Fetch | Access::Load => self.read,
            Access::Store => self.write,
        }
    }
}

/// A device region: accesses are serviced by the device rather than by storage.
///
/// Both methods take `&self` so that a [`Memory`] built around devices can
/// still be read through the shared-reference accessors. A device that has
/// read side effects (clearing a status bit, say) uses interior mutability.
///
/// `offset` is relative to the region's base and is always inside the region:
/// containment and permissions are checked by [`Memory`] before the device is
/// reached.
pub trait Device: fmt::Debug {
    /// Read `len` bytes (1, 2, 4, or 8) little-endian from `offset`.
    fn read(&self, offset: u32, len: u32) -> u64;

    /// Write the low `8 * len` bits of `val` little-endian at `offset`.
    fn write(&self, offset: u32, len: u32, val: u64);
}

/// A shared handle to a device is itself a device, which is what lets a caller
/// keep its own [`Rc`] to a device the address space also holds.
impl<D: Device + ?Sized> Device for Rc<D> {
    fn read(&self, offset: u32, len: u32) -> u64 {
        (**self).read(offset, len)
    }

    fn write(&self, offset: u32, len: u32, val: u64) {
        (**self).write(offset, len, val)
    }
}

#[derive(Debug)]
enum Backing {
    /// Contiguous zero-filled storage. O(1) per access, so this is what RAM
    /// should use; it costs one allocation of `size` bytes.
    Flat(Vec<u8>),
    /// Per-byte map. Costs nothing up front, which is what a handful of
    /// device registers or a whole-address-space stand-in wants.
    Sparse(BTreeMap<u32, u8>),
    /// A device with its own behaviour.
    Device(Box<dyn Device>),
}

impl Backing {
    /// Read `len` bytes little-endian from `offset`, in one call, so a device
    /// sees the access width it was given rather than a stream of bytes.
    fn read_block(&self, offset: u32, len: u32) -> u64 {
        match self {
            Backing::Flat(bytes) => {
                let mut v = 0u64;
                for i in 0..len as usize {
                    v |= u64::from(bytes[offset as usize + i]) << (8 * i);
                }
                v
            }
            Backing::Sparse(map) => {
                let mut v = 0u64;
                for i in 0..len {
                    v |= u64::from(map.get(&(offset + i)).copied().unwrap_or(0)) << (8 * i);
                }
                v
            }
            Backing::Device(device) => device.read(offset, len),
        }
    }

    /// Write the low `8 * len` bits of `val` little-endian at `offset`, in one
    /// call, for the same reason as [`Backing::read_block`].
    fn write_block(&mut self, offset: u32, len: u32, val: u64) {
        match self {
            Backing::Flat(bytes) => {
                for i in 0..len as usize {
                    bytes[offset as usize + i] = ((val >> (8 * i)) & 0xff) as u8;
                }
            }
            Backing::Sparse(map) => {
                for i in 0..len {
                    map.insert(offset + i, ((val >> (8 * i)) & 0xff) as u8);
                }
            }
            Backing::Device(device) => device.write(offset, len, val),
        }
    }
}

/// A decoded region of the address space.
#[derive(Debug)]
pub struct Region {
    base: u32,
    /// Width in bytes. `u64` so that a region can describe the whole 32-bit
    /// address space, which needs 2^32 bytes and so does not fit in a `u32`.
    size: u64,
    perms: Permissions,
    backing: Backing,
}

impl Region {
    /// Contiguous storage of `size` bytes at `base`, zero-filled.
    ///
    /// Use for RAM: access is O(1) instead of a per-byte map lookup, which is
    /// what makes running a full `riscv-tests` suite practical.
    pub fn ram(base: u32, size: u64, perms: Permissions) -> Self {
        assert!(size > 0, "a region must not be empty");
        assert!(
            size <= u64::from(u32::MAX) + 1,
            "a region cannot exceed the address space"
        );
        Region {
            base,
            size,
            perms,
            backing: Backing::Flat(vec![0; size as usize]),
        }
    }

    /// Sparse storage of `size` bytes at `base`. Unwritten bytes read as 0.
    ///
    /// Use for device registers and for a whole-address-space stand-in: it
    /// costs nothing until something is written.
    pub fn sparse(base: u32, size: u64, perms: Permissions) -> Self {
        assert!(size > 0, "a region must not be empty");
        assert!(
            size <= u64::from(u32::MAX) + 1,
            "a region cannot exceed the address space"
        );
        Region {
            base,
            size,
            perms,
            backing: Backing::Sparse(BTreeMap::new()),
        }
    }

    /// A device region of `size` bytes at `base`, serviced by `device`.
    ///
    /// Use for anything that is not plain storage: the HTIF test-completion
    /// registers, or a CLINT.
    pub fn device(base: u32, size: u64, perms: Permissions, device: Box<dyn Device>) -> Self {
        assert!(size > 0, "a region must not be empty");
        assert!(
            size <= u64::from(u32::MAX) + 1,
            "a region cannot exceed the address space"
        );
        Region {
            base,
            size,
            perms,
            backing: Backing::Device(device),
        }
    }

    /// A device region around a shared handle.
    ///
    /// A [`Device`] takes `&self` on every access, so a device the host also
    /// needs to read is naturally held behind an [`Rc`]: the address space gets
    /// one clone and the caller keeps the other. That is how a runner inspects
    /// the HTIF completion value after a run without any downcasting.
    pub fn shared_device<D: Device + 'static>(
        base: u32,
        size: u64,
        perms: Permissions,
        device: Rc<D>,
    ) -> Self {
        Region::device(base, size, perms, Box::new(device))
    }

    /// First address of the region.
    pub fn base(&self) -> u32 {
        self.base
    }

    /// Size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Permissions of the region.
    pub fn permissions(&self) -> Permissions {
        self.perms
    }
}

/// A decoded address space.
#[derive(Debug)]
pub struct Memory {
    /// Kept sorted by base so lookups can binary search.
    regions: Vec<Region>,
}

impl Default for Memory {
    /// The same as [`Memory::permissive`].
    fn default() -> Self {
        Self::permissive()
    }
}

impl Memory {
    /// The whole 32-bit address space, readable, writable, and backed.
    ///
    /// Nothing can fault, because every address exists. Convenient for
    /// instruction-level tests; not a platform, because it cannot model an
    /// unmapped region.
    pub fn permissive() -> Self {
        Memory {
            regions: vec![Region::sparse(
                0,
                u64::from(u32::MAX) + 1,
                Permissions::READ_WRITE,
            )],
        }
    }

    /// An address space decoded from `regions`. Only what these cover exists.
    pub fn from_regions(regions: Vec<Region>) -> Self {
        let mut regions = regions;
        regions.sort_by_key(|r| r.base);
        Memory { regions }
    }

    /// The regions, in ascending base order.
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Is every byte of `[addr, addr + len)` backed and permitted for `access`?
    ///
    /// The containment test is done in `u64` so that neither an access ending
    /// exactly at the top of the address space nor a `base + size` that reaches
    /// `2^32` can overflow.
    fn locate(&self, addr: u32, len: u32, access: Access) -> Result<(usize, u32), AccessFault> {
        for (i, region) in self.regions.iter().enumerate() {
            if u64::from(addr) < u64::from(region.base) {
                // Sorted by base: no later region can cover this address.
                break;
            }
            let within = u64::from(addr) - u64::from(region.base);
            if within >= region.size {
                // `addr` is one past this region's last byte: the start of the
                // next region, or a gap. Not a fault on its own.
                continue;
            }
            if within + u64::from(len) > region.size {
                // The access starts inside this region but runs past its end.
                // Do not fall through: a gap in the middle of a region is
                // still a fault, not an invitation to read the next region.
                return Err(AccessFault { addr, access });
            }
            if region.perms.allows(access) {
                return Ok((i, within as u32));
            }
            // Backed, but this kind of access is not permitted here: report a
            // fault rather than falling through, so a read-only device register
            // does not silently become unmapped memory.
            return Err(AccessFault { addr, access });
        }
        Err(AccessFault { addr, access })
    }

    /// Fetch the instruction word at `addr`.
    pub fn fetch_u32(&self, addr: u32) -> Result<u32, AccessFault> {
        Ok(self.load_as(addr, 4, Access::Fetch)? as u32)
    }

    /// Read `len` bytes (1, 2, 4, or 8) little-endian from `addr`.
    pub fn load(&self, addr: u32, len: u32) -> Result<u64, AccessFault> {
        self.load_as(addr, len, Access::Load)
    }

    /// Write the low `8 * len` bits of `val` little-endian at `addr`.
    pub fn store(&mut self, addr: u32, len: u32, val: u64) -> Result<(), AccessFault> {
        let (index, offset) = self.locate(addr, len, Access::Store)?;
        self.regions[index].backing.write_block(offset, len, val);
        Ok(())
    }

    fn load_as(&self, addr: u32, len: u32, access: Access) -> Result<u64, AccessFault> {
        let (index, offset) = self.locate(addr, len, access)?;
        Ok(self.regions[index].backing.read_block(offset, len))
    }

    /// Load a raw binary image at `base`.
    ///
    /// This is host-side setup, not a guest access, but it can still fail: an
    /// image that does not fit the decoded map is a harness error worth
    /// reporting rather than silently truncating.
    ///
    /// The image may span regions — a `riscv-tests` binary covers `.text.init`
    /// *and* the `tohost` page — so coverage is checked byte by byte rather than
    /// demanding that the whole image land in one region. Note that a *guest*
    /// access may not straddle a boundary even where an image load can: an
    /// unaligned or overhanging load is an access fault, which is a different
    /// question from whether the bytes are backed at all.
    pub fn load_image(&mut self, base: u32, bytes: &[u8]) -> Result<(), AccessFault> {
        if u32::try_from(bytes.len()).is_err() {
            return Err(AccessFault {
                addr: base,
                access: Access::Store,
            });
        }
        for (i, b) in bytes.iter().enumerate() {
            let addr = base.wrapping_add(i as u32);
            let (index, offset) = self.locate(addr, 1, Access::Store)?;
            self.regions[index]
                .backing
                .write_block(offset, 1, u64::from(*b));
        }
        Ok(())
    }

    // ---- Host-side setup helpers ------------------------------------------
    //
    // Unchecked on purpose: these are not guest accesses. They panic when the
    // address is not backed, which is a bug in the caller rather than
    // behaviour the guest could observe.

    /// Write one byte without an access check. For test setup.
    pub fn poke_u8(&mut self, addr: u32, val: u8) {
        self.poke(addr, &[val])
    }

    /// Write a 32-bit word little-endian without an access check. For test setup.
    pub fn poke_u32(&mut self, addr: u32, val: u32) {
        self.poke(addr, &val.to_le_bytes())
    }

    /// Write `bytes` at `addr` without an access check. For test setup.
    pub fn poke(&mut self, addr: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            let at = addr.wrapping_add(i as u32);
            let (index, offset) = self
                .locate(at, 1, Access::Store)
                .unwrap_or_else(|_| panic!("poke to unmapped address 0x{at:08x}"));
            self.regions[index]
                .backing
                .write_block(offset, 1, u64::from(*b));
        }
    }

    /// Read one byte without an access check. For test inspection.
    pub fn peek_u8(&self, addr: u32) -> u8 {
        self.peek(addr, 1) as u8
    }

    /// Read a 32-bit word little-endian without an access check.
    pub fn peek_u32(&self, addr: u32) -> u32 {
        self.peek(addr, 4) as u32
    }

    /// Read `len` bytes little-endian without an access check.
    pub fn peek(&self, addr: u32, len: u32) -> u64 {
        self.load_as(addr, len, Access::Load)
            .unwrap_or_else(|_| panic!("peek at unmapped address 0x{addr:08x}"))
    }
}
