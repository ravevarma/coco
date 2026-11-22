//! Kernel heap allocator.
//!
//! The heap manages one contiguous virtual range `[base, base + capacity)`
//! that a [`HeapBackend`] makes usable on demand (the kernel maps frames;
//! tests use an arena).
//!
//! * **Small** requests (`max(size, align) <= 2048`) use eight power-of-two
//!   size classes (16..=2048). Each class is an intrusive free list fed one
//!   4 KiB page at a time. Blocks are naturally aligned to their class size.
//! * **Large** requests are whole pages taken first-fit from an
//!   address-ordered, coalescing list of free page runs, or from the
//!   unused tail of the range.
//!
//! # Hardening (always on)
//! * Exact double-free detection: a freed small block carries
//!   [`FREE_MAGIC`]; if a block being freed already carries it, the class
//!   list is searched and a hit is reported as [`HeapError::DoubleFree`]. A
//!   freed large run overlapping an existing free run is a double free.
//! * Pointer validation: freed pointers must lie inside the carved range
//!   and be aligned to their class / page.
//! * Optional poisoning ([`HeapConfig::poison`]): freed small blocks are
//!   filled with [`POISON`] and checked on reuse, detecting writes after
//!   free ([`HeapError::Corruption`]).
//!
//! # Invariants
//! * `base <= brk <= mapped <= base + capacity`, all page aligned.
//! * Every byte in `[base, brk)` belongs to exactly one of: a live
//!   allocation, a small-class free block, a class page's unused tail
//!   (none: pages are split completely), or a free page run.
//! * Free runs are sorted by address, non-overlapping and non-adjacent
//!   (always coalesced).
//! * Small-class pages are never returned to the run list (bounded by peak
//!   use per class).

use core::ptr;

/// Page size used by the heap.
pub const HEAP_PAGE: usize = 4096;
/// Smallest size class.
pub const MIN_CLASS: usize = 16;
/// Largest small size class.
pub const MAX_SMALL: usize = 2048;
const CLASSES: usize = 8; // 16, 32, ..., 2048
/// Marker stored in word 1 of a free small block.
pub const FREE_MAGIC: usize = 0xF4EE_B10C_C0C0_F4EE_u64 as usize;
/// Fill byte for freed small blocks when poisoning is enabled.
pub const POISON: u8 = 0xDF;
/// Minimum growth step requested from the backend.
pub const GROW_STEP: usize = 64 * 1024;

/// Makes parts of the heap range usable.
pub trait HeapBackend {
    /// Makes `[start, start + len)` readable and writable. `start` and
    /// `len` are page aligned. Returns `false` if memory is exhausted.
    fn grow(&mut self, start: *mut u8, len: usize) -> bool;
}

/// Heap errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapError {
    /// Backend or range exhausted.
    OutOfMemory,
    /// Size or alignment unsupported (alignment > 4096, or size 0).
    Unsupported,
    /// Pointer outside the heap or misaligned for its size class.
    InvalidPointer(usize),
    /// Block freed twice.
    DoubleFree(usize),
    /// A freed block was written after free (poison or magic damaged).
    Corruption(usize),
}

/// Heap configuration.
#[derive(Clone, Copy, Debug)]
pub struct HeapConfig {
    /// Poison freed small blocks and verify on reuse.
    pub poison: bool,
}

/// Allocation statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes currently allocated (rounded to class / page size).
    pub in_use: usize,
    /// Highest `in_use` seen.
    pub peak: usize,
    /// Bytes obtained from the backend so far.
    pub mapped: usize,
    /// Successful allocations.
    pub allocations: u64,
    /// Successful frees.
    pub frees: u64,
    /// Failed allocation attempts.
    pub failures: u64,
}

#[repr(C)]
struct FreeBlock {
    next: *mut FreeBlock,
    magic: usize,
}

#[repr(C)]
struct FreeRun {
    next: *mut FreeRun,
    pages: usize,
}

/// The allocator state.
#[derive(Debug)]
pub struct Heap<B: HeapBackend> {
    base: *mut u8,
    capacity: usize,
    brk: usize,
    mapped: usize,
    classes: [*mut FreeBlock; CLASSES],
    runs: *mut FreeRun,
    config: HeapConfig,
    stats: HeapStats,
    backend: B,
}

// SAFETY: the heap exclusively owns its range; all access goes through
// `&mut self`, so moving it to another thread (behind a lock) is sound.
unsafe impl<B: HeapBackend + Send> Send for Heap<B> {}

/// Size class index for a small request, or `None` if it is large.
#[must_use]
pub fn class_of(size: usize, align: usize) -> Option<usize> {
    let need = size.max(align).max(MIN_CLASS);
    if need > MAX_SMALL {
        return None;
    }
    let rounded = need.next_power_of_two();
    Some(rounded.trailing_zeros().saturating_sub(MIN_CLASS.trailing_zeros()) as usize)
}

const fn class_size(class: usize) -> usize {
    MIN_CLASS << class
}

/// Reinterprets a heap address as a free-run header.
///
/// Every caller passes a page-aligned address (runs start on pages), which
/// satisfies `FreeRun`'s 8-byte alignment; checked in debug builds.
#[allow(clippy::cast_ptr_alignment, reason = "runs are page aligned by construction")]
fn as_run(p: *mut u8) -> *mut FreeRun {
    let r = p.cast::<FreeRun>();
    debug_assert!(r.is_aligned());
    r
}

/// Reinterprets a heap address as a free-block header.
///
/// Blocks are aligned to their class size (>= 16), which satisfies
/// `FreeBlock`'s 8-byte alignment; checked in debug builds.
#[allow(clippy::cast_ptr_alignment, reason = "blocks are class-size aligned by construction")]
fn as_block(p: *mut u8) -> *mut FreeBlock {
    let b = p.cast::<FreeBlock>();
    debug_assert!(b.is_aligned());
    b
}

fn pages_for(size: usize) -> Option<usize> {
    size.checked_add(HEAP_PAGE - 1).map(|s| s / HEAP_PAGE)
}

impl<B: HeapBackend> Heap<B> {
    /// Creates an empty heap over `[base, base + capacity)`.
    ///
    /// # Safety
    /// The range must be reserved exclusively for this heap (no other
    /// mapping or object may live there), `base` must be page aligned, and
    /// `backend.grow` must make requested sub-ranges valid for reads and
    /// writes for the heap's lifetime.
    pub const unsafe fn new(base: *mut u8, capacity: usize, config: HeapConfig, backend: B) -> Self {
        Self {
            base,
            capacity: capacity & !(HEAP_PAGE - 1),
            brk: 0,
            mapped: 0,
            classes: [ptr::null_mut(); CLASSES],
            runs: ptr::null_mut(),
            config,
            stats: HeapStats { in_use: 0, peak: 0, mapped: 0, allocations: 0, frees: 0, failures: 0 },
            backend,
        }
    }

    /// Current statistics.
    #[must_use]
    pub const fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Mutable access to the backend (e.g. for statistics).
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn at(&self, offset: usize) -> *mut u8 {
        self.base.wrapping_add(offset)
    }

    fn offset_of(&self, p: *mut u8) -> Option<usize> {
        (p as usize).checked_sub(self.base as usize).filter(|&o| o < self.brk)
    }

    /// Takes `pages` contiguous pages: first fit from the run list, else
    /// from the tail (growing the backend in [`GROW_STEP`] units).
    fn take_pages(&mut self, pages: usize) -> Result<*mut u8, HeapError> {
        let bytes = pages.checked_mul(HEAP_PAGE).ok_or(HeapError::OutOfMemory)?;
        let mut prev: *mut FreeRun = ptr::null_mut();
        let mut cur = self.runs;
        while !cur.is_null() {
            // SAFETY: `cur` is a run header inside the carved range (list
            // invariant); runs are at least one page, so the header fits.
            let FreeRun { next, pages: run_pages } = unsafe { cur.read() };
            if run_pages >= pages {
                let rest = run_pages.saturating_sub(pages);
                let replacement = if rest > 0 {
                    let tail = as_run(cur.cast::<u8>().wrapping_add(bytes));
                    // SAFETY: `tail` is page-aligned inside the same free
                    // run (rest > 0 pages remain after it).
                    unsafe { tail.write(FreeRun { next, pages: rest }) };
                    tail
                } else {
                    next
                };
                if prev.is_null() {
                    self.runs = replacement;
                } else {
                    // SAFETY: `prev` is a valid run header (list invariant).
                    unsafe { (*prev).next = replacement };
                }
                return Ok(cur.cast());
            }
            prev = cur;
            cur = next;
        }
        let new_brk = self.brk.checked_add(bytes).filter(|&b| b <= self.capacity).ok_or(HeapError::OutOfMemory)?;
        if new_brk > self.mapped {
            // needed <= capacity - mapped because new_brk <= capacity, so
            // the clamp never drops below what is needed.
            let needed = new_brk.saturating_sub(self.mapped);
            let grow = needed.max(GROW_STEP).min(self.capacity.saturating_sub(self.mapped));
            if !self.backend.grow(self.at(self.mapped), grow) {
                return Err(HeapError::OutOfMemory);
            }
            self.mapped = self.mapped.saturating_add(grow);
            self.stats.mapped = self.mapped;
        }
        let p = self.at(self.brk);
        self.brk = new_brk;
        Ok(p)
    }

    /// Returns a page run, coalescing with its neighbours.
    fn put_pages(&mut self, p: *mut u8, pages: usize) -> Result<(), HeapError> {
        let start = self.offset_of(p).ok_or(HeapError::InvalidPointer(p as usize))?;
        let len = pages.checked_mul(HEAP_PAGE).ok_or(HeapError::InvalidPointer(p as usize))?;
        let end = start.checked_add(len).filter(|&e| e <= self.brk).ok_or(HeapError::InvalidPointer(p as usize))?;
        let mut prev: *mut FreeRun = ptr::null_mut();
        let mut cur = self.runs;
        while !cur.is_null() {
            let cur_off = (cur as usize).wrapping_sub(self.base as usize);
            // SAFETY: valid run header (list invariant).
            let FreeRun { next, pages: cur_pages } = unsafe { cur.read() };
            let cur_end = cur_off.saturating_add(cur_pages.saturating_mul(HEAP_PAGE));
            if cur_off < end && start < cur_end {
                return Err(HeapError::DoubleFree(p as usize));
            }
            if cur_off >= end {
                break;
            }
            prev = cur;
            cur = next;
        }
        let node = as_run(p);
        // SAFETY: `p..p+len` is a page-aligned run inside the carved range
        // that the caller owned (validated above not to overlap free runs).
        unsafe { node.write(FreeRun { next: cur, pages }) };
        if prev.is_null() {
            self.runs = node;
        } else {
            // SAFETY: valid run header.
            unsafe { (*prev).next = node };
        }
        // Coalesce with the following run.
        if !cur.is_null() && (cur as usize) == (node as usize).wrapping_add(len) {
            // SAFETY: `cur` is a valid run header adjacent to `node`.
            let FreeRun { next: cur_next, pages: cur_pages } = unsafe { cur.read() };
            // SAFETY: `node` was written above.
            unsafe { node.write(FreeRun { next: cur_next, pages: pages.saturating_add(cur_pages) }) };
        }
        // Coalesce with the preceding run.
        if !prev.is_null() {
            // SAFETY: valid run header.
            let FreeRun { pages: prev_pages, .. } = unsafe { prev.read() };
            // SAFETY: `node` was written above.
            let FreeRun { next: node_next, pages: node_pages } = unsafe { node.read() };
            if (prev as usize).wrapping_add(prev_pages.saturating_mul(HEAP_PAGE)) == node as usize {
                // SAFETY: `prev` is a valid run header.
                unsafe { prev.write(FreeRun { next: node_next, pages: prev_pages.saturating_add(node_pages) }) };
            }
        }
        Ok(())
    }

    fn refill(&mut self, class: usize) -> Result<(), HeapError> {
        let page = self.take_pages(1)?;
        let size = class_size(class);
        let mut head = self.classes.get(class).copied().unwrap_or(ptr::null_mut());
        // Push blocks in reverse so allocation proceeds in address order.
        let mut off = HEAP_PAGE;
        while off >= size {
            off = off.saturating_sub(size);
            let block = page.wrapping_add(off);
            self.mark_free(block, size, head);
            head = as_block(block);
        }
        if let Some(slot) = self.classes.get_mut(class) {
            *slot = head;
        }
        Ok(())
    }

    fn mark_free(&self, block: *mut u8, size: usize, next: *mut FreeBlock) {
        if self.config.poison {
            // SAFETY: `block..block+size` is a block this heap owns and
            // that is not live.
            unsafe { ptr::write_bytes(block, POISON, size) };
        }
        // SAFETY: blocks are >= 16 bytes and 16-byte aligned, so the header fits.
        unsafe { as_block(block).write(FreeBlock { next, magic: FREE_MAGIC }) };
    }

    /// Allocates `size` bytes aligned to `align` (a power of two <= 4096).
    ///
    /// # Errors
    /// [`HeapError::OutOfMemory`], [`HeapError::Unsupported`],
    /// [`HeapError::Corruption`].
    pub fn allocate(&mut self, size: usize, align: usize) -> Result<*mut u8, HeapError> {
        let r = self.allocate_inner(size, align);
        match r {
            Ok(_) => self.stats.allocations = self.stats.allocations.saturating_add(1),
            Err(_) => self.stats.failures = self.stats.failures.saturating_add(1),
        }
        r
    }

    fn allocate_inner(&mut self, size: usize, align: usize) -> Result<*mut u8, HeapError> {
        if size == 0 || !align.is_power_of_two() || align > HEAP_PAGE {
            return Err(HeapError::Unsupported);
        }
        let (p, bytes) = if let Some(class) = class_of(size, align) {
            if self.classes.get(class).is_none_or(|h| h.is_null()) {
                self.refill(class)?;
            }
            let head = self.classes.get(class).copied().unwrap_or(ptr::null_mut());
            if head.is_null() {
                return Err(HeapError::OutOfMemory);
            }
            // SAFETY: `head` is a free block of this class (list invariant).
            let FreeBlock { next, magic } = unsafe { head.read() };
            let csize = class_size(class);
            if magic != FREE_MAGIC {
                return Err(HeapError::Corruption(head as usize));
            }
            if self.config.poison {
                let tail = head.cast::<u8>().wrapping_add(core::mem::size_of::<FreeBlock>());
                let tail_len = csize.saturating_sub(core::mem::size_of::<FreeBlock>());
                // SAFETY: the block is `csize` bytes owned by the heap.
                let bytes = unsafe { core::slice::from_raw_parts(tail, tail_len) };
                if bytes.iter().any(|&b| b != POISON) {
                    return Err(HeapError::Corruption(head as usize));
                }
            }
            // SAFETY: as above; clear the magic so a live block never
            // carries it by our hand.
            unsafe { (*head).magic = 0 };
            if let Some(slot) = self.classes.get_mut(class) {
                *slot = next;
            }
            (head.cast::<u8>(), csize)
        } else {
            let pages = pages_for(size).ok_or(HeapError::OutOfMemory)?;
            (self.take_pages(pages)?, pages.saturating_mul(HEAP_PAGE))
        };
        self.stats.in_use = self.stats.in_use.saturating_add(bytes);
        self.stats.peak = self.stats.peak.max(self.stats.in_use);
        Ok(p)
    }

    /// Frees a block previously returned by [`Heap::allocate`] with the same
    /// `size` and `align`.
    ///
    /// # Errors
    /// [`HeapError::InvalidPointer`] or [`HeapError::DoubleFree`]; the heap
    /// is unchanged on error.
    pub fn free(&mut self, p: *mut u8, size: usize, align: usize) -> Result<(), HeapError> {
        if size == 0 || !align.is_power_of_two() || align > HEAP_PAGE {
            return Err(HeapError::Unsupported);
        }
        let off = self.offset_of(p).ok_or(HeapError::InvalidPointer(p as usize))?;
        let bytes = if let Some(class) = class_of(size, align) {
            let csize = class_size(class);
            if !off.is_multiple_of(csize) {
                return Err(HeapError::InvalidPointer(p as usize));
            }
            let block = as_block(p);
            // SAFETY: `p` is inside the carved range and class-aligned, so
            // the 16-byte header lies within a block of this heap.
            let magic = unsafe { (*block).magic };
            if magic == FREE_MAGIC && self.class_contains(class, block) {
                return Err(HeapError::DoubleFree(p as usize));
            }
            let head = self.classes.get(class).copied().unwrap_or(ptr::null_mut());
            self.mark_free(p, csize, head);
            if let Some(slot) = self.classes.get_mut(class) {
                *slot = block;
            }
            csize
        } else {
            if !off.is_multiple_of(HEAP_PAGE) {
                return Err(HeapError::InvalidPointer(p as usize));
            }
            let pages = pages_for(size).ok_or(HeapError::InvalidPointer(p as usize))?;
            self.put_pages(p, pages)?;
            pages.saturating_mul(HEAP_PAGE)
        };
        self.stats.in_use = self.stats.in_use.saturating_sub(bytes);
        self.stats.frees = self.stats.frees.saturating_add(1);
        Ok(())
    }

    fn class_contains(&self, class: usize, needle: *mut FreeBlock) -> bool {
        let mut cur = self.classes.get(class).copied().unwrap_or(ptr::null_mut());
        while !cur.is_null() {
            if cur == needle {
                return true;
            }
            // SAFETY: list invariant.
            cur = unsafe { (*cur).next };
        }
        false
    }

    /// Verifies the run list invariants (sorted, disjoint, coalesced,
    /// inside the carved range). O(n); for tests and self-checks.
    #[must_use]
    pub fn check_runs(&self) -> bool {
        let mut cur = self.runs;
        let mut prev_end: Option<usize> = None;
        while !cur.is_null() {
            let off = (cur as usize).wrapping_sub(self.base as usize);
            // SAFETY: list invariant.
            let FreeRun { next, pages } = unsafe { cur.read() };
            let Some(end) = pages.checked_mul(HEAP_PAGE).and_then(|l| off.checked_add(l)) else { return false };
            if pages == 0 || !off.is_multiple_of(HEAP_PAGE) || end > self.brk || prev_end.is_some_and(|pe| pe >= off) {
                return false;
            }
            prev_end = Some(end);
            cur = next;
        }
        true
    }
}
