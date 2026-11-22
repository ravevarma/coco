//! Kernel heap (`#[global_allocator]`).
//!
//! Virtual window: `HEAP_BASE .. HEAP_BASE + HEAP_CAPACITY` (PML4 slot
//! 507), backed on demand with zeroed frames by [`crate::mm`]. The
//! allocator itself is `coco_memory::heap` (host-tested, Miri-checked)
//! with poisoning enabled.
//!
//! Rules:
//! * Allocation in interrupt context is refused (null is returned, which
//!   aborts through `handle_alloc_error`) and reported — handlers must be
//!   allocation-free.
//! * Heap corruption or a double free is a fatal invariant violation.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

use coco_memory::heap::{Heap, HeapBackend, HeapConfig, HeapError, HeapStats};

use crate::sync::IrqSpinLock;
use crate::{cpu, kprintln_emergency, mm};

/// Base of the kernel heap window.
pub const HEAP_BASE: u64 = 0xFFFF_FD80_0000_0000;
/// Size of the heap window (virtual; backed lazily).
pub const HEAP_CAPACITY: usize = 1 << 30;

/// Maps fresh frames for the heap.
#[derive(Debug)]
pub struct KernelBackend;

impl HeapBackend for KernelBackend {
    fn grow(&mut self, start: *mut u8, len: usize) -> bool {
        mm::with(|m| m.map_fresh(start as u64, len as u64)).unwrap_or(false)
    }
}

static HEAP: IrqSpinLock<Option<Heap<KernelBackend>>> = IrqSpinLock::new(None);
/// Allocation attempts refused because they happened in interrupt context.
pub static IRQ_ALLOC_VIOLATIONS: AtomicU64 = AtomicU64::new(0);

/// Creates the heap. Must run after `mm::install`.
pub fn init() {
    // SAFETY: the window at PML4 507 is reserved exclusively for the heap
    // (docs/adr/0003), page aligned, and KernelBackend maps every range it
    // is asked to grow for the kernel's lifetime.
    let heap = unsafe { Heap::new(HEAP_BASE as *mut u8, HEAP_CAPACITY, HeapConfig { poison: true }, KernelBackend) };
    *HEAP.lock() = Some(heap);
}

/// Current heap statistics (`None` before [`init`]).
pub fn stats() -> Option<HeapStats> {
    HEAP.lock().as_ref().map(Heap::stats)
}

fn fatal(what: &str, err: HeapError) -> ! {
    kprintln_emergency!("FATAL: kernel heap {what}: {err:?}");
    crate::fatal_stop()
}

struct KernelAllocator;

// SAFETY: `alloc` returns either null or a block of at least `layout.size()`
// bytes aligned to `layout.align()` that no other live allocation overlaps
// (guaranteed by coco_memory::heap, verified by its model-based tests and
// Miri); `dealloc` receives the same layout, as GlobalAlloc requires.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if cpu::in_interrupt() {
            IRQ_ALLOC_VIOLATIONS.fetch_add(1, Ordering::Relaxed);
            kprintln_emergency!("heap: allocation of {} bytes in interrupt context refused", layout.size());
            return core::ptr::null_mut();
        }
        let mut guard = HEAP.lock();
        let Some(heap) = guard.as_mut() else { return core::ptr::null_mut() };
        match heap.allocate(layout.size(), layout.align()) {
            Ok(p) => p,
            Err(HeapError::OutOfMemory | HeapError::Unsupported) => core::ptr::null_mut(),
            Err(e) => {
                drop(guard);
                fatal("corruption detected on allocation", e)
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut guard = HEAP.lock();
        let result = match guard.as_mut() {
            Some(heap) => heap.free(ptr, layout.size(), layout.align()),
            None => Err(HeapError::InvalidPointer(ptr as usize)),
        };
        drop(guard);
        if let Err(e) = result {
            fatal("invalid free", e);
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;
