//! x86_64 4-level page-table mapper.
//!
//! The mapper never dereferences page tables directly; it asks a
//! [`PhysAccess`] implementation for a pointer to the table at a physical
//! address. This lets the same, host-tested code run in the UEFI loader
//! (identity-mapped physical memory), in the kernel (direct map) and in unit
//! tests (a simulated arena).
//!
//! # Invariants
//! * Intermediate entries created by the mapper are `PRESENT | WRITABLE`
//!   (plus `USER` when a user leaf is installed beneath them) and never carry
//!   `NO_EXECUTE`; permissions are enforced at the leaf.
//! * A 2 MiB leaf (`HUGE` set in a level-2 entry) is never descended into.
//! * The mapper does not flush TLBs: every operation that changes a live
//!   mapping returns a `#[must_use]` [`Flush`] token.

use crate::PAGE_SIZE;

/// 2 MiB page size.
pub const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const ENTRIES: usize = 512;

/// Page-table entry flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFlags(u64);

impl PageFlags {
    /// Entry is valid.
    pub const PRESENT: Self = Self(1 << 0);
    /// Writes allowed.
    pub const WRITABLE: Self = Self(1 << 1);
    /// Ring 3 access allowed.
    pub const USER: Self = Self(1 << 2);
    /// Write-through caching.
    pub const WRITE_THROUGH: Self = Self(1 << 3);
    /// Caching disabled.
    pub const NO_CACHE: Self = Self(1 << 4);
    /// Large page (level 2/3 leaf).
    pub const HUGE: Self = Self(1 << 7);
    /// Not flushed on CR3 reload.
    pub const GLOBAL: Self = Self(1 << 8);
    /// Instruction fetches fault (requires EFER.NXE).
    pub const NO_EXECUTE: Self = Self(1 << 63);
    /// No flags.
    pub const EMPTY: Self = Self(0);

    const ALLOWED_LEAF: u64 = Self::WRITABLE.0
        | Self::USER.0
        | Self::WRITE_THROUGH.0
        | Self::NO_CACHE.0
        | Self::GLOBAL.0
        | Self::NO_EXECUTE.0;

    /// Raw bits.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Builds flags from raw bits (unknown bits retained).
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Union.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Contains all of `other`?
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for PageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// Supported leaf sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PageSize {
    /// 4 KiB.
    Size4K,
    /// 2 MiB.
    Size2M,
}

impl PageSize {
    /// Size in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Size4K => PAGE_SIZE,
            Self::Size2M => LARGE_PAGE_SIZE,
        }
    }

    /// Mask of the in-page offset bits (`bytes() - 1`).
    #[must_use]
    pub const fn offset_mask(self) -> u64 {
        match self {
            Self::Size4K => 0xFFF,
            Self::Size2M => 0x1F_FFFF,
        }
    }
}

/// Mapper errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The virtual address is already mapped.
    AlreadyMapped,
    /// The virtual address is not mapped.
    NotMapped,
    /// Address not aligned to the page size.
    Misaligned,
    /// Virtual address is not canonical.
    NonCanonical,
    /// Physical address exceeds 52 bits.
    PhysOutOfRange,
    /// A frame for an intermediate table could not be allocated.
    OutOfFrames,
    /// The request crosses an existing mapping of a different page size.
    SizeConflict,
    /// Flags contain bits the mapper manages itself (PRESENT/HUGE/reserved).
    InvalidFlags,
}

/// Translates physical addresses of page tables into usable pointers.
///
/// # Safety
/// Implementations must return, for every page-aligned physical address the
/// mapper passes in (all of which are page-table frames reachable from the
/// root or freshly obtained from a [`TableFrameSource`]), a pointer that is
/// valid for reads and writes of 4096 bytes, 8-byte aligned, and not aliased
/// by any live Rust reference for the duration of the mapper's use.
pub unsafe trait PhysAccess {
    /// Pointer to the 512-entry table at physical address `phys`.
    fn table(&self, phys: u64) -> *mut u64;
}

/// Supplies frames for new intermediate page tables.
pub trait TableFrameSource {
    /// Returns the physical address of an exclusively owned, page-aligned
    /// frame, or `None` when exhausted. Ownership passes to the page table.
    fn allocate_table(&mut self) -> Option<u64>;
}

/// Token requiring the caller to invalidate a TLB entry.
#[derive(Debug)]
#[must_use = "the TLB entry for this address must be invalidated"]
pub struct Flush(u64);

impl Flush {
    /// Virtual address to invalidate (`invlpg`).
    #[must_use]
    pub const fn addr(&self) -> u64 {
        self.0
    }

    /// Explicitly skips the flush (only valid when the tables are not live).
    pub const fn ignore(self) {}
}

/// Result of a successful translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    /// Physical address corresponding to the queried virtual address.
    pub phys: u64,
    /// Leaf flags.
    pub flags: PageFlags,
    /// Leaf size.
    pub size: PageSize,
}

/// A 4-level page-table hierarchy rooted at a PML4 frame.
#[derive(Debug)]
pub struct Mapper<A: PhysAccess> {
    root: u64,
    access: A,
}

/// True if `virt` is canonical for 48-bit virtual addresses.
#[must_use]
pub const fn is_canonical(virt: u64) -> bool {
    let top = virt >> 47;
    top == 0 || top == 0x1_FFFF
}

const fn index(virt: u64, level: u32) -> usize {
    let shift = match level {
        4 => 39,
        3 => 30,
        2 => 21,
        _ => 12,
    };
    ((virt >> shift) & 0x1FF) as usize
}

impl<A: PhysAccess> Mapper<A> {
    /// Wraps an existing PML4.
    ///
    /// # Safety
    /// `root` must be the physical address of a valid PML4 whose reachable
    /// tables are exclusively controlled by this mapper (no concurrent
    /// mutation), and `access` must satisfy the [`PhysAccess`] contract for
    /// all of them.
    pub const unsafe fn new(root: u64, access: A) -> Self {
        Self { root, access }
    }

    /// Physical address of the PML4.
    #[must_use]
    pub const fn root(&self) -> u64 {
        self.root
    }

    fn read(&self, table: u64, idx: usize) -> u64 {
        let entry = self.access.table(table).wrapping_add(idx & (ENTRIES - 1));
        // SAFETY: `PhysAccess` guarantees the table pointer is valid for 512
        // aligned u64 reads, and `idx & 511` keeps `entry` inside the table.
        unsafe { entry.read() }
    }

    fn write(&mut self, table: u64, idx: usize, value: u64) {
        let entry = self.access.table(table).wrapping_add(idx & (ENTRIES - 1));
        // SAFETY: as in `read`; the mapper has exclusive control of the
        // tables (contract of `Mapper::new`), so no reference aliases `entry`.
        unsafe { entry.write(value) }
    }

    fn zero_table(&mut self, table: u64) {
        for i in 0..ENTRIES {
            self.write(table, i, 0);
        }
    }

    /// Returns the next-level table for `entry`, creating it if absent.
    fn next_table(
        &mut self,
        table: u64,
        idx: usize,
        user: bool,
        frames: &mut dyn TableFrameSource,
    ) -> Result<u64, MapError> {
        let entry = self.read(table, idx);
        if entry & PageFlags::PRESENT.0 != 0 {
            if entry & PageFlags::HUGE.0 != 0 {
                return Err(MapError::SizeConflict);
            }
            if user && entry & PageFlags::USER.0 == 0 {
                self.write(table, idx, entry | PageFlags::USER.0);
            }
            return Ok(entry & ADDR_MASK);
        }
        let new = frames.allocate_table().ok_or(MapError::OutOfFrames)?;
        if new % PAGE_SIZE != 0 || new & !ADDR_MASK != 0 {
            return Err(MapError::PhysOutOfRange);
        }
        self.zero_table(new);
        let mut e = new | PageFlags::PRESENT.0 | PageFlags::WRITABLE.0;
        if user {
            e |= PageFlags::USER.0;
        }
        self.write(table, idx, e);
        Ok(new)
    }

    /// Maps `virt -> phys` with the given leaf flags.
    ///
    /// # Errors
    /// See [`MapError`]. On error no existing mapping is modified (an
    /// intermediate table may have been allocated and remains linked).
    pub fn map(
        &mut self,
        virt: u64,
        phys: u64,
        size: PageSize,
        flags: PageFlags,
        frames: &mut dyn TableFrameSource,
    ) -> Result<Flush, MapError> {
        if !is_canonical(virt) {
            return Err(MapError::NonCanonical);
        }
        if virt & size.offset_mask() != 0 || phys & size.offset_mask() != 0 {
            return Err(MapError::Misaligned);
        }
        if phys & !ADDR_MASK != 0 {
            return Err(MapError::PhysOutOfRange);
        }
        if flags.0 & !PageFlags::ALLOWED_LEAF != 0 {
            return Err(MapError::InvalidFlags);
        }
        let user = flags.contains(PageFlags::USER);
        let l3 = self.next_table(self.root, index(virt, 4), user, frames)?;
        let l2 = self.next_table(l3, index(virt, 3), user, frames)?;
        let (table, idx, leaf) = match size {
            PageSize::Size2M => (l2, index(virt, 2), phys | flags.0 | PageFlags::PRESENT.0 | PageFlags::HUGE.0),
            PageSize::Size4K => {
                let l1 = self.next_table(l2, index(virt, 2), user, frames)?;
                (l1, index(virt, 1), phys | flags.0 | PageFlags::PRESENT.0)
            }
        };
        let existing = self.read(table, idx);
        if existing & PageFlags::PRESENT.0 != 0 {
            return Err(if size == PageSize::Size2M && existing & PageFlags::HUGE.0 == 0 {
                MapError::SizeConflict
            } else {
                MapError::AlreadyMapped
            });
        }
        self.write(table, idx, leaf);
        Ok(Flush(virt))
    }

    fn walk(&self, virt: u64) -> Option<(u64, usize, PageSize)> {
        if !is_canonical(virt) {
            return None;
        }
        let mut table = self.root;
        for level in [4u32, 3, 2] {
            let idx = index(virt, level);
            let e = self.read(table, idx);
            if e & PageFlags::PRESENT.0 == 0 {
                return None;
            }
            if e & PageFlags::HUGE.0 != 0 {
                return if level == 2 { Some((table, idx, PageSize::Size2M)) } else { None };
            }
            table = e & ADDR_MASK;
        }
        let idx = index(virt, 1);
        (self.read(table, idx) & PageFlags::PRESENT.0 != 0).then_some((table, idx, PageSize::Size4K))
    }

    /// Looks up the mapping for `virt`.
    #[must_use]
    pub fn translate(&self, virt: u64) -> Option<Translation> {
        let (table, idx, size) = self.walk(virt)?;
        let e = self.read(table, idx);
        let offset = virt & size.offset_mask();
        Some(Translation {
            phys: (e & ADDR_MASK & !size.offset_mask()) | offset,
            flags: PageFlags(e & !ADDR_MASK),
            size,
        })
    }

    /// Removes the mapping whose page contains `virt`.
    ///
    /// Returns the physical base of the unmapped page (whose ownership
    /// returns to the caller) and its size. Intermediate tables are kept.
    ///
    /// # Errors
    /// [`MapError::NotMapped`] or [`MapError::NonCanonical`].
    pub fn unmap(&mut self, virt: u64) -> Result<(u64, PageSize, Flush), MapError> {
        if !is_canonical(virt) {
            return Err(MapError::NonCanonical);
        }
        let (table, idx, size) = self.walk(virt).ok_or(MapError::NotMapped)?;
        let e = self.read(table, idx);
        self.write(table, idx, 0);
        let page = virt & !size.offset_mask();
        Ok((e & ADDR_MASK & !size.offset_mask(), size, Flush(page)))
    }

    /// Raw PML4 entry `idx` (`idx < 512`).
    #[must_use]
    pub fn root_entry(&self, idx: usize) -> u64 {
        self.read(self.root, idx)
    }

    /// Overwrites PML4 entry `idx`.
    ///
    /// # Safety
    /// `value` must be zero or reference a valid level-3 table satisfying the
    /// mapper invariants; replacing a live entry requires a full TLB flush
    /// (CR3 reload) and the caller takes ownership of any detached tables.
    pub unsafe fn set_root_entry(&mut self, idx: usize, value: u64) {
        self.write(self.root, idx, value);
    }

    /// Removes every mapping reachable from PML4 slots `slots` and hands
    /// out the frames: `leaf` receives each mapped page (physical base and
    /// size), `table` receives each page-table frame below the root
    /// (level 3, 2 and 1 tables). The root entries are cleared.
    ///
    /// # Safety
    /// The slots must belong exclusively to the address space being torn
    /// down (never shared kernel slots), no CPU may be using this address
    /// space afterwards without a TLB flush, and the callbacks become the
    /// owners of the frames they receive.
    pub unsafe fn teardown_slots(
        &mut self,
        slots: core::ops::Range<usize>,
        leaf: &mut dyn FnMut(u64, PageSize),
        table: &mut dyn FnMut(u64),
    ) {
        for slot in slots.start.min(ENTRIES)..slots.end.min(ENTRIES) {
            let l4 = self.read(self.root, slot);
            if l4 & PageFlags::PRESENT.0 == 0 {
                continue;
            }
            let pdpt = l4 & ADDR_MASK;
            for i3 in 0..ENTRIES {
                let l3 = self.read(pdpt, i3);
                if l3 & PageFlags::PRESENT.0 == 0 || l3 & PageFlags::HUGE.0 != 0 {
                    // 1 GiB leaves are never created by this mapper.
                    continue;
                }
                let pd = l3 & ADDR_MASK;
                for i2 in 0..ENTRIES {
                    let l2 = self.read(pd, i2);
                    if l2 & PageFlags::PRESENT.0 == 0 {
                        continue;
                    }
                    if l2 & PageFlags::HUGE.0 != 0 {
                        leaf(l2 & ADDR_MASK & !PageSize::Size2M.offset_mask(), PageSize::Size2M);
                        continue;
                    }
                    let pt = l2 & ADDR_MASK;
                    for i1 in 0..ENTRIES {
                        let l1 = self.read(pt, i1);
                        if l1 & PageFlags::PRESENT.0 != 0 {
                            leaf(l1 & ADDR_MASK, PageSize::Size4K);
                        }
                    }
                    table(pt);
                }
                table(pd);
            }
            table(pdpt);
            self.write(self.root, slot, 0);
        }
    }
}
