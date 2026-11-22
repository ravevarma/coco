//! Kernel memory initialization: drops the loader's identity map, builds
//! the physical frame allocator from the boot memory map and exposes a
//! page-table mapper over the live address space.

use coco_abi::layout::{DIRECT_MAP_BASE, DIRECT_MAP_MAX_LEN, PAGE_SIZE, phys_to_virt};
use coco_abi::{BootInfo, MemoryKind, MemoryRegion, flags};
use coco_arch::cpu;
use coco_memory::frame::{FrameAllocator, FrameError, PhysFrame, words_for};
use coco_memory::paging::{MapError, Mapper, PageFlags, PageSize, PhysAccess, TableFrameSource};

use core::cell::RefCell;

use crate::sync::IrqSpinLock;

/// The kernel's memory managers after [`install`].
///
/// Lock order (rule L3): `heap::HEAP` may be held while taking `MEMORY`
/// (the heap grows by mapping frames); code holding `MEMORY` must never
/// allocate from the heap.
static MEMORY: IrqSpinLock<Option<KernelMemory>> = IrqSpinLock::new(None);

/// Makes `mem` the global memory manager.
pub fn install(mem: KernelMemory) {
    *MEMORY.lock() = Some(mem);
}

/// Runs `f` with exclusive access to the memory managers (interrupts are
/// disabled meanwhile). `None` before [`install`].
pub fn with<R>(f: impl FnOnce(&mut KernelMemory) -> R) -> Option<R> {
    MEMORY.lock().as_mut().map(f)
}

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

impl KernelMemory {
    fn map_page(&mut self, virt: u64, phys: u64, flags: PageFlags) -> Result<(), MapError> {
        let flush = self.mapper.map(virt, phys, PageSize::Size4K, flags, &mut AllocatorTables(&mut self.frames))?;
        cpu::invlpg(flush.addr());
        Ok(())
    }

    /// Ensures the page containing `phys` is readable through the direct
    /// map (read-only, NX). Used for firmware tables that live outside the
    /// RAM regions the loader mapped.
    ///
    /// # Errors
    /// Mapping failures, or `phys` beyond the direct map.
    pub fn ensure_direct_mapped(&mut self, phys: u64) -> Result<u64, MapError> {
        let page = phys & !(PAGE_SIZE - 1);
        let virt = phys_to_virt(page).ok_or(MapError::PhysOutOfRange)?;
        if self.mapper.translate(virt).is_none() {
            self.map_page(virt, page, PageFlags::NO_EXECUTE)?;
        }
        Ok(virt + (phys - page))
    }

    /// Maps the MMIO page containing `phys` uncached, writable and NX in the
    /// direct map and returns the virtual address of `phys`.
    ///
    /// # Errors
    /// Mapping failures, or `phys` beyond the direct map.
    pub fn map_mmio(&mut self, phys: u64) -> Result<u64, MapError> {
        if phys >= DIRECT_MAP_MAX_LEN {
            return Err(MapError::PhysOutOfRange);
        }
        let page = phys & !(PAGE_SIZE - 1);
        let virt = phys_to_virt(page).ok_or(MapError::PhysOutOfRange)?;
        match self.mapper.translate(virt) {
            // Already mapped (e.g. inside a RAM-backed 2 MiB page): MMIO
            // must not alias cached RAM mappings, so refuse.
            Some(t) if t.flags.contains(PageFlags::NO_CACHE) => {}
            Some(_) => return Err(MapError::AlreadyMapped),
            None => self.map_page(
                virt,
                page,
                PageFlags::WRITABLE | PageFlags::NO_CACHE | PageFlags::WRITE_THROUGH | PageFlags::NO_EXECUTE,
            )?,
        }
        Ok(virt + (phys - page))
    }

    /// Backs `[virt, virt + len)` with fresh zeroed frames (RW, NX).
    /// All-or-nothing: on failure every page mapped by this call is
    /// unmapped and its frame freed.
    pub fn map_fresh(&mut self, virt: u64, len: u64) -> bool {
        let pages = len / PAGE_SIZE;
        for i in 0..pages {
            let va = virt + i * PAGE_SIZE;
            let ok = match self.frames.allocate() {
                Ok(frame) => {
                    let phys = frame.into_raw();
                    let dm = DIRECT_MAP_BASE.wrapping_add(phys) as *mut u8;
                    // SAFETY: the frame was just allocated (exclusively ours)
                    // and is direct-mapped; zeroing prevents leaking stale
                    // data into the heap.
                    unsafe { core::ptr::write_bytes(dm, 0, PAGE_SIZE as usize) };
                    if self.map_page(va, phys, PageFlags::WRITABLE | PageFlags::NO_EXECUTE).is_ok() {
                        true
                    } else {
                        // SAFETY: mapping failed, so ownership is still ours.
                        let _ = self.frames.free(unsafe { PhysFrame::from_raw(phys) });
                        false
                    }
                }
                Err(_) => false,
            };
            if !ok {
                for j in 0..i {
                    if let Ok((phys, _, flush)) = self.mapper.unmap(virt + j * PAGE_SIZE) {
                        cpu::invlpg(flush.addr());
                        // SAFETY: the mapping owned this frame (created above).
                        let _ = self.frames.free(unsafe { PhysFrame::from_raw(phys) });
                    }
                }
                return false;
            }
        }
        true
    }

    /// Unmaps `[virt, virt + len)` (4 KiB pages) and frees the frames that
    /// backed it. Pages that are not mapped are skipped.
    pub fn unmap_and_free(&mut self, virt: u64, len: u64) {
        for i in 0..len / PAGE_SIZE {
            if let Ok((phys, _, flush)) = self.mapper.unmap(virt + i * PAGE_SIZE) {
                cpu::invlpg(flush.addr());
                // SAFETY: the mapping owned this frame (created by map_fresh
                // or map_user_page) and was just removed.
                let _ = self.frames.free(unsafe { PhysFrame::from_raw(phys) });
            }
        }
    }

    /// Allocates a zeroed frame (e.g. a new PML4) and returns its address.
    pub fn alloc_zeroed_frame(&mut self) -> Option<u64> {
        let phys = self.frames.allocate().ok()?.into_raw();
        // SAFETY: freshly allocated, exclusively owned, direct-mapped frame.
        unsafe { core::ptr::write_bytes(DIRECT_MAP_BASE.wrapping_add(phys) as *mut u8, 0, PAGE_SIZE as usize) };
        Some(phys)
    }

    /// Makes every kernel-half PML4 slot (256..512) of the live address
    /// space present, so process address spaces that copy the kernel half
    /// share all future kernel mappings. Returns the number of slots added.
    pub fn prepopulate_kernel_half(&mut self) -> Result<u32, MapError> {
        let mut added = 0;
        for slot in 256..512 {
            if self.mapper.root_entry(slot) & PageFlags::PRESENT.bits() == 0 {
                let table = self.alloc_zeroed_frame().ok_or(MapError::OutOfFrames)?;
                // SAFETY: `table` is a zeroed, exclusively owned frame: a
                // valid empty level-3 table. Adding a non-present-to-present
                // entry needs no TLB flush. Kernel-only (no USER bit).
                unsafe {
                    self.mapper.set_root_entry(slot, table | PageFlags::PRESENT.bits() | PageFlags::WRITABLE.bits())
                };
                added += 1;
            }
        }
        Ok(added)
    }

    /// Creates a new address space: a fresh PML4 whose kernel half is a copy
    /// of the live one (sharing all kernel tables) and whose user half is
    /// empty. Returns the PML4's physical address.
    pub fn new_address_space(&mut self) -> Option<u64> {
        let root = self.alloc_zeroed_frame()?;
        let dst = DIRECT_MAP_BASE.wrapping_add(root) as *mut u64;
        for slot in 256..512usize {
            let entry = self.mapper.root_entry(slot);
            // SAFETY: `dst` is the freshly zeroed PML4 frame (512 entries);
            // `slot < 512`.
            unsafe { dst.wrapping_add(slot).write(entry) };
        }
        Some(root)
    }

    /// Maps a fresh zeroed frame at `virt` in the address space `root` with
    /// `flags` (USER is added). Returns the frame's physical address.
    pub fn map_user_page(&mut self, root: u64, virt: u64, flags: PageFlags) -> Result<u64, MapError> {
        let phys = self.alloc_zeroed_frame().ok_or(MapError::OutOfFrames)?;
        // SAFETY: `root` is a process PML4 created by `new_address_space`;
        // its user half is owned exclusively by that process and only
        // modified here and in `teardown_user`, under the MEMORY lock.
        let mut m = unsafe { Mapper::new(root, DirectMap) };
        match m.map(virt, phys, PageSize::Size4K, flags | PageFlags::USER, &mut AllocatorTables(&mut self.frames)) {
            Ok(flush) => {
                // The address space is not live on this CPU while being
                // built; a stale entry cannot exist for a fresh mapping.
                flush.ignore();
                Ok(phys)
            }
            Err(e) => {
                // SAFETY: the mapping failed, so the frame is still ours.
                let _ = self.frames.free(unsafe { PhysFrame::from_raw(phys) });
                Err(e)
            }
        }
    }

    /// Translates `virt` in address space `root`.
    pub fn translate_in(&mut self, root: u64, virt: u64) -> Option<coco_memory::paging::Translation> {
        // SAFETY: read-only walk of a valid PML4 under the MEMORY lock.
        let m = unsafe { Mapper::new(root, DirectMap) };
        m.translate(virt)
    }

    /// Frees every user page and user page table of address space `root`,
    /// then the PML4 itself. Returns the number of frames released.
    ///
    /// The address space must not be live on any CPU.
    pub fn teardown_user(&mut self, root: u64) -> u64 {
        // No heap allocation here: MEMORY is held, and the heap may need
        // MEMORY to grow (lock order L3). Frames are freed in the callbacks.
        let frames = RefCell::new((&mut self.frames, 0u64));
        let release = |p: u64| {
            let mut f = frames.borrow_mut();
            // SAFETY: each frame was owned by the torn-down page tables (or
            // is the PML4 itself); freeing only updates the bitmap, so the
            // walk can still read tables freed after their children.
            if f.0.free(unsafe { PhysFrame::from_raw(p) }).is_ok() {
                f.1 += 1;
            }
        };
        // SAFETY: as in `map_user_page`; slots 0..256 belong only to this
        // process, and the caller guarantees the space is not live.
        let mut m = unsafe { Mapper::new(root, DirectMap) };
        // SAFETY: user slots are private to the process; every frame handed
        // to the callbacks is released exactly once.
        unsafe { m.teardown_slots(0..256, &mut |p, _size| release(p), &mut |t| release(t)) };
        release(root);
        frames.into_inner().1
    }
}
