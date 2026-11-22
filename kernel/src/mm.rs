//! Kernel memory initialization: drops the loader's identity map, builds
//! the physical frame allocator from the boot memory map and exposes a
//! page-table mapper over the live address space.

use coco_abi::layout::{DIRECT_MAP_BASE, PAGE_SIZE, phys_to_virt};
use coco_abi::{BootInfo, MemoryKind, MemoryRegion, flags};
use coco_arch::cpu;
use coco_memory::frame::{FrameAllocator, FrameError, words_for};
use coco_memory::paging::{Mapper, PhysAccess, TableFrameSource};

/// Frames below this address are never allocated: real-mode IVT/BDA, the
/// future SMP trampoline, and protection against stray null-ish physical
/// addresses.
pub const LOW_MEMORY_RESERVED: u64 = 1 << 20;

/// Page tables reached through the direct map.
#[derive(Debug, Clone, Copy)]
pub struct DirectMap;

// SAFETY: the loader direct-maps all RAM, including every page-table frame
// (PageTables regions) and every frame the allocator hands out (Usable /
// BootloaderReclaimable regions), at DIRECT_MAP_BASE + phys. The kernel
// owns the page tables exclusively (single CPU, mapper owned by
// `KernelMemory`).
unsafe impl PhysAccess for DirectMap {
    fn table(&self, phys: u64) -> *mut u64 {
        DIRECT_MAP_BASE.wrapping_add(phys) as *mut u64
    }
}

/// Supplies page-table frames from the frame allocator (zeroed by the mapper).
#[derive(Debug)]
pub struct AllocatorTables<'a, 'b>(pub &'a mut FrameAllocator<'b>);

impl TableFrameSource for AllocatorTables<'_, '_> {
    fn allocate_table(&mut self) -> Option<u64> {
        // Ownership moves into the page table.
        self.0.allocate().ok().map(coco_memory::frame::PhysFrame::into_raw)
    }
}

/// Memory statistics derived from the boot memory map.
#[derive(Debug, Default, Clone, Copy)]
pub struct MemoryStats {
    /// Bytes per kind, indexed by `MemoryKind as usize`.
    pub bytes_by_kind: [u64; 14],
    /// Frames consumed by the allocator's own bitmaps.
    pub bitmap_frames: u64,
    /// Frames withheld below [`LOW_MEMORY_RESERVED`].
    pub low_reserved_frames: u64,
}

/// The kernel's memory managers.
#[derive(Debug)]
pub struct KernelMemory {
    /// Physical frame allocator.
    pub frames: FrameAllocator<'static>,
    /// Mapper over the live (CR3) page tables.
    pub mapper: Mapper<DirectMap>,
    /// Boot-time statistics.
    pub stats: MemoryStats,
}

/// Memory initialization errors (reported through `Debug`).
#[derive(Debug)]
#[allow(dead_code, reason = "payloads are read through the Debug impl")]
pub enum MmError {
    /// No usable region can hold the allocator bitmaps.
    NoBitmapStorage,
    /// Frame allocator rejected a region.
    Frame(FrameError),
    /// The direct map does not cover a required range.
    NotDirectMapped,
}

fn region_end(r: &MemoryRegion) -> u64 {
    r.end().unwrap_or(0)
}

fn allocatable(r: &MemoryRegion) -> bool {
    matches!(r.kind(), Some(MemoryKind::Usable | MemoryKind::BootloaderReclaimable))
}

/// Removes the loader's lower-half identity alias.
///
/// Must run after the kernel's own GDT/IDT are loaded (the firmware GDT
/// lives in low memory) and before any lower-half address is reused.
pub fn drop_identity_map(info: &BootInfo) {
    if info.flags & flags::IDENTITY_MAPPED == 0 {
        return;
    }
    let root = cpu::read_cr3() & 0x000F_FFFF_FFFF_F000;
    // SAFETY: CR3 is the loader-built PML4, reachable through the direct
    // map; the kernel is the only user of the page tables.
    let mut mapper = unsafe { Mapper::new(root, DirectMap) };
    for i in 0..256 {
        // SAFETY: clearing lower-half PML4 entries detaches only the
        // identity alias, whose tables are shared with (and still owned by)
        // the direct map; nothing in the kernel uses lower-half addresses.
        unsafe { mapper.set_root_entry(i, 0) };
    }
    // SAFETY: reloading the same CR3 flushes the TLB; the current code,
    // stack and data are all in the higher half and remain mapped.
    unsafe { cpu::write_cr3(cpu::read_cr3()) };
}

/// Builds the frame allocator and mapper.
///
/// # Errors
/// See [`MmError`].
pub fn init(info: &BootInfo, regions: &[MemoryRegion]) -> Result<KernelMemory, MmError> {
    let mut stats = MemoryStats::default();
    for r in regions {
        if let (Some(kind), Some(len)) = (r.kind(), r.byte_len())
            && let Some(slot) = stats.bytes_by_kind.get_mut(kind as usize)
        {
            *slot = slot.saturating_add(len);
        }
    }
    let top = regions.iter().filter(|r| allocatable(r)).map(region_end).max().unwrap_or(0);
    let frames_total = top / PAGE_SIZE;
    let words = words_for(frames_total);
    let bitmap_bytes = (words as u64).saturating_mul(8).saturating_mul(2);
    let bitmap_pages = bitmap_bytes.div_ceil(PAGE_SIZE).max(1);

    // Bitmap storage: the first Usable region above the reserved low
    // memory that is large enough and inside the direct map.
    let storage = regions
        .iter()
        .filter(|r| r.kind() == Some(MemoryKind::Usable))
        .map(|r| (r.base.max(LOW_MEMORY_RESERVED), region_end(r)))
        .find(|&(start, end)| end > start && (end - start) / PAGE_SIZE >= bitmap_pages)
        .map(|(start, _)| start)
        .ok_or(MmError::NoBitmapStorage)?;
    let storage_end = storage + bitmap_pages * PAGE_SIZE;
    if !info.in_direct_map(DIRECT_MAP_BASE + storage, bitmap_pages * PAGE_SIZE) {
        return Err(MmError::NotDirectMapped);
    }
    let base = phys_to_virt(storage).ok_or(MmError::NotDirectMapped)? as *mut u64;
    // SAFETY: `[storage, storage_end)` is Usable RAM inside the direct map,
    // not yet given to any allocator and excluded from the free ranges
    // below, so the kernel owns it exclusively for its whole lifetime
    // ('static). It is 8-byte aligned (page aligned) and any bit pattern is
    // a valid u64. The two slices are disjoint halves.
    let managed = unsafe { core::slice::from_raw_parts_mut(base, words) };
    // SAFETY: as above; starts `words` u64s after `managed` ends.
    let free = unsafe { core::slice::from_raw_parts_mut(base.wrapping_add(words), words) };
    let mut frames = FrameAllocator::new(managed, free, frames_total).map_err(MmError::Frame)?;
    stats.bitmap_frames = bitmap_pages;

    for r in regions.iter().filter(|r| allocatable(r)) {
        let (start, end) = (r.base, region_end(r));
        let mut add = |s: u64, e: u64| -> Result<(), MmError> {
            let s = s.max(LOW_MEMORY_RESERVED);
            if e > s {
                frames.add_free_range(s, (e - s) / PAGE_SIZE).map_err(MmError::Frame)?;
            }
            Ok(())
        };
        if start < storage_end && storage < end {
            add(start, storage)?;
            add(storage_end, end)?;
        } else {
            add(start, end)?;
        }
        let low_end = end.min(LOW_MEMORY_RESERVED);
        if low_end > start {
            stats.low_reserved_frames += (low_end - start) / PAGE_SIZE;
        }
    }

    let root = cpu::read_cr3() & 0x000F_FFFF_FFFF_F000;
    // SAFETY: CR3 holds the live PML4 (direct-mapped); the returned mapper
    // is the single owner of page-table mutation from here on.
    let mapper = unsafe { Mapper::new(root, DirectMap) };
    Ok(KernelMemory { frames, mapper, stats })
}
