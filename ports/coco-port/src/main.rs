//! coco portable bring-up kernel.
//!
//! One kernel, four architecture backends (`arch/`): RISC-V 64 (S-mode under
//! OpenSBI), RISC-V 32 (M-mode, no firmware), AArch64 (EL1) and 32-bit x86
//! (Multiboot). Each backend provides boot entry, console, trap handling,
//! a periodic timer, firmware memory discovery and machine exit; this file
//! runs the same self-tests on all of them:
//!
//! 1. breakpoint trap delivered and resumed,
//! 2. timer interrupts arrive at the configured rate,
//! 3. physical frame allocator over firmware-reported RAM (full
//!    exhaustion with pattern writes, then exact recovery),
//! 4. kernel heap (`coco-memory::heap`) through `alloc` collections with an
//!    exact leak check,
//! 5. capability tables and channel IPC (`coco-capability`, `coco-ipc`)
//!    running in kernel context.
//!
//! Stage note: the MMU is not enabled yet on these ports (physical =
//! virtual); paging, user mode and the full kernel follow per architecture.
#![no_std]
#![no_main]

extern crate alloc;

mod arch;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use coco_fdt::Range;
use coco_memory::frame::{FrameAllocator, FrameError, PhysFrame, words_for};
use coco_memory::heap::{Heap, HeapBackend, HeapConfig};
use coco_sync::SpinLock;

const PAGE: u64 = 4096;
/// Timer frequency used by every backend.
pub const HZ: u32 = 100;
/// Heap carved out of RAM for the bring-up kernel.
const HEAP_BYTES: u64 = 1 << 20;

/// Breakpoint traps handled.
pub static BREAKPOINTS: AtomicU32 = AtomicU32::new(0);
/// Timer interrupts handled (32-bit atomics: RV32 has no 64-bit atomics).
pub static TICKS: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------- console

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                arch::putc(b'\r');
            }
            arch::putc(b);
        }
        Ok(())
    }
}

/// Prints to the platform console.
pub fn print(args: fmt::Arguments<'_>) {
    let _ = Console.write_fmt(args);
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => { $crate::print(format_args!("{}\n", format_args!($($arg)*))) };
}

// ---------------------------------------------------------------- platform

/// What a backend discovered about the machine.
#[derive(Debug)]
pub struct Platform {
    /// Usable RAM ranges reported by firmware.
    pub ram: [Range; 16],
    /// Number of valid `ram` entries.
    pub ram_len: usize,
    /// Ranges that must not be used (firmware, boot blobs).
    pub reserved: [Range; 16],
    /// Number of valid `reserved` entries.
    pub reserved_len: usize,
    /// Where the memory map came from.
    pub source: &'static str,
}

impl Platform {
    /// An empty platform description.
    pub const fn new(source: &'static str) -> Self {
        Self {
            ram: [Range { base: 0, size: 0 }; 16],
            ram_len: 0,
            reserved: [Range { base: 0, size: 0 }; 16],
            reserved_len: 0,
            source,
        }
    }

    /// Adds a RAM range (ignored when the table is full).
    pub fn add_ram(&mut self, r: Range) {
        if let Some(slot) = self.ram.get_mut(self.ram_len) {
            *slot = r;
            self.ram_len += 1;
        }
    }

    /// Adds a reserved range (ignored when the table is full).
    pub fn add_reserved(&mut self, r: Range) {
        if let Some(slot) = self.reserved.get_mut(self.reserved_len) {
            *slot = r;
            self.reserved_len += 1;
        }
    }
}

/// Highest physical address the kernel can touch with the MMU off.
fn addressable_limit() -> u64 {
    // On 32-bit CPUs physical addresses above 4 GiB are not reachable as
    // pointers: clamp instead of truncating.
    u64::try_from(usize::MAX).map_or(u64::MAX, |m| m.saturating_add(1))
}

/// `ram` minus every excluded range, page aligned, clamped to what the CPU
/// can address and to at least 1 MiB (legacy/firmware area).
fn usable_ranges(ram: &[Range], excluded: &[Range]) -> ([Range; 32], usize) {
    let mut out = [Range::default(); 32];
    let mut n = 0;
    let mut work: [Range; 32] = [Range::default(); 32];
    let mut wn = 0;
    for r in ram {
        let start = r.base.max(1 << 20).div_ceil(PAGE) * PAGE;
        let end = (r.base.saturating_add(r.size)).min(addressable_limit()) / PAGE * PAGE;
        if end > start && wn < work.len() {
            work[wn] = Range { base: start, size: end - start };
            wn += 1;
        }
    }
    for w in work.iter().take(wn) {
        // Split `w` by every excluded range (bounded: at most 32 pieces).
        let mut pieces = [Range::default(); 32];
        let mut pn = 1;
        pieces[0] = *w;
        for ex in excluded {
            let ex_start = ex.base / PAGE * PAGE;
            let ex_end = ex.base.saturating_add(ex.size).div_ceil(PAGE) * PAGE;
            let mut next = [Range::default(); 32];
            let mut nn = 0;
            for p in pieces.iter().take(pn) {
                let (ps, pe) = (p.base, p.base + p.size);
                if ex_end <= ps || ex_start >= pe {
                    if nn < next.len() {
                        next[nn] = *p;
                        nn += 1;
                    }
                    continue;
                }
                if ex_start > ps && nn < next.len() {
                    next[nn] = Range { base: ps, size: ex_start - ps };
                    nn += 1;
                }
                if ex_end < pe && nn < next.len() {
                    next[nn] = Range { base: ex_end, size: pe - ex_end };
                    nn += 1;
                }
            }
            pieces = next;
            pn = nn;
        }
        for p in pieces.iter().take(pn) {
            if p.size > 0 && n < out.len() {
                out[n] = *p;
                n += 1;
            }
        }
    }
    (out, n)
}

// ---------------------------------------------------------------- heap

/// Heap backend over a fixed, pre-reserved physical region (MMU off).
struct RegionBackend {
    end: usize,
}

impl HeapBackend for RegionBackend {
    fn grow(&mut self, start: *mut u8, len: usize) -> bool {
        (start as usize).checked_add(len).is_some_and(|e| e <= self.end)
    }
}

struct Allocator(SpinLock<Option<Heap<RegionBackend>>>);

// SAFETY: blocks returned by `coco_memory::heap` are disjoint, aligned and
// sized as requested (host model tests, Miri); `dealloc` passes back the
// same layout. Interrupt handlers never allocate, so the plain spin lock
// cannot be re-entered from a handler.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut g = self.0.lock();
        match g.as_mut() {
            Some(h) => h.allocate(layout.size(), layout.align()).unwrap_or(core::ptr::null_mut()),
            None => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let result = self.0.lock().as_mut().map(|h| h.free(ptr, layout.size(), layout.align()));
        if !matches!(result, Some(Ok(()))) {
            println!("FATAL: invalid heap free at {:p}: {:?}", ptr, result);
            arch::exit(false);
        }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator(SpinLock::new(None));

fn heap_in_use() -> usize {
    ALLOCATOR.0.lock().as_ref().map_or(0, |h| h.stats().in_use)
}

// ---------------------------------------------------------------- tests

static FAILURES: AtomicUsize = AtomicUsize::new(0);

fn check(name: &str, ok: bool, detail: fmt::Arguments<'_>) {
    if ok {
        println!("  {name}: PASS ({detail})");
    } else {
        FAILURES.fetch_add(1, Ordering::Relaxed);
        println!("  {name}: FAIL ({detail})");
    }
}

fn test_traps_and_timer() {
    let before = BREAKPOINTS.load(Ordering::Relaxed);
    arch::breakpoint();
    let after = BREAKPOINTS.load(Ordering::Relaxed);
    check("breakpoint trap", after == before + 1, format_args!("{} handled, execution resumed", after - before));

    arch::start_timer(HZ);
    let c0 = arch::counter();
    let t0 = TICKS.load(Ordering::Relaxed);
    while TICKS.load(Ordering::Relaxed) < t0 + 20 {
        arch::wait_for_interrupt();
    }
    let elapsed = arch::counter().wrapping_sub(c0);
    match arch::counter_hz() {
        Some(hz) => {
            let ms = elapsed.saturating_mul(1000) / hz.max(1);
            check(
                "timer interrupts",
                (160..=240).contains(&ms),
                format_args!("20 ticks at {HZ} Hz took {ms} ms by the CPU counter"),
            );
        }
        None => {
            check("timer interrupts", true, format_args!("20 ticks received (no fixed-rate counter to cross-check)"))
        }
    }
}

fn test_frames(usable: &[Range]) -> Option<(FrameAllocator<'static>, Range)> {
    let top = usable.iter().map(|r| r.base + r.size).max()?;
    let frames = top / PAGE;
    let words = words_for(frames)?;
    let bitmap_bytes = (words as u64) * 8 * 2;
    let bitmap_pages = bitmap_bytes.div_ceil(PAGE);
    // Carve bitmaps and the heap from the first range large enough.
    let need = bitmap_pages * PAGE + HEAP_BYTES;
    let carve = usable.iter().find(|r| r.size >= need)?;
    let base = carve.base;
    let managed_ptr = base as usize as *mut u64;
    // SAFETY: `[base, base + bitmap_bytes)` is usable RAM reported by
    // firmware, excluded from every reservation and from the kernel image,
    // and never handed to the allocator (removed below); with the MMU off
    // the address is directly usable. The two slices are disjoint halves.
    let managed = unsafe { core::slice::from_raw_parts_mut(managed_ptr, words) };
    // SAFETY: as above, the second half.
    let free = unsafe { core::slice::from_raw_parts_mut(managed_ptr.wrapping_add(words), words) };
    let mut fa = FrameAllocator::new(managed, free, frames).ok()?;
    let reserved_end = base + bitmap_pages * PAGE;
    let heap = Range { base: reserved_end, size: HEAP_BYTES };
    for r in usable {
        let mut add = |s: u64, e: u64| {
            if e > s {
                let _ = fa.add_free_range(s, (e - s) / PAGE);
            }
        };
        if r.base == base {
            add(heap.base + heap.size, r.base + r.size);
        } else {
            add(r.base, r.base + r.size);
        }
    }
    // Exhaustion with pattern writes, then exact recovery.
    let initial = fa.free_frames();
    let mut head = 0u64;
    let mut count = 0u64;
    while let Ok(f) = fa.allocate() {
        let addr = f.into_raw();
        let p = addr as usize as *mut u64;
        // SAFETY: the frame was just allocated (exclusively ours) and lies
        // in usable RAM; word 0 links the list, word 1 is a check tag.
        unsafe { p.write_volatile(head) };
        // SAFETY: as above.
        unsafe { p.wrapping_add(1).write_volatile(!addr) };
        head = addr | 1;
        count += 1;
    }
    let mut intact = true;
    while head & 1 == 1 {
        let addr = head & !1;
        let p = addr as usize as *mut u64;
        // SAFETY: `addr` is a frame this test owns (from the list above).
        let next = unsafe { p.read_volatile() };
        // SAFETY: as above.
        intact &= unsafe { p.wrapping_add(1).read_volatile() } == !addr;
        // SAFETY: re-wraps a frame obtained from `into_raw` exactly once.
        if fa.free(unsafe { PhysFrame::from_raw(addr) }) != Ok(()) {
            intact = false;
        }
        head = next;
    }
    check(
        "frame allocator",
        count == initial
            && fa.free_frames() == initial
            && intact
            && fa.check_invariants()
            && fa.allocate().map(|f| fa.free(f)).is_ok(),
        format_args!("{} frames ({} MiB) allocated with pattern writes and recovered", count, count * PAGE >> 20),
    );
    let _ = FrameError::OutOfMemory;
    Some((fa, heap))
}

fn test_heap(region: Range) {
    let base = region.base as usize as *mut u8;
    let end = (region.base + region.size) as usize;
    // SAFETY: `region` was carved from usable RAM for the heap only and is
    // never handed to the frame allocator; RegionBackend accepts only
    // growth inside it.
    let heap = unsafe { Heap::new(base, region.size as usize, HeapConfig { poison: true }, RegionBackend { end }) };
    *ALLOCATOR.0.lock() = Some(heap);
    let baseline = heap_in_use();
    let mut ok = true;
    for round in 0..3u32 {
        let v: Vec<u32> = (0..10_000).map(|i| i ^ round).collect();
        ok &= v.iter().enumerate().all(|(i, &x)| x == i as u32 ^ round);
        let mut m = BTreeMap::new();
        for i in 0..2_000u32 {
            m.insert(i.wrapping_mul(2_654_435_761) % 997, alloc::vec![i as u8; (i % 64) as usize]);
        }
        ok &= m.len() <= 997;
        drop((v, m));
        ok &= heap_in_use() == baseline;
    }
    check("kernel heap", ok, format_args!("3 churn rounds, in-use back to {} B each round", baseline));
}

fn test_ipc_and_capabilities() {
    use coco_abi::syscall::{Error, rights};
    use coco_capability::HandleTable;
    use coco_ipc::Channel;
    let baseline = heap_in_use();
    let mut ok = true;
    {
        let chan = Arc::new(SpinLock::new(Channel::<u64>::new()));
        let mut table: HandleTable<(Arc<SpinLock<Channel<u64>>>, usize)> = HandleTable::new(16);
        let a = table.insert((Arc::clone(&chan), 0), rights::ALL).map_err(|_| ()).unwrap_or(0);
        let b = table.insert((Arc::clone(&chan), 1), rights::READ).map_err(|_| ()).unwrap_or(0);
        ok &= a != 0 && b != 0;
        ok &= table.get(b, rights::WRITE).err() == Some(Error::AccessDenied);
        for i in 0..100u64 {
            ok &= chan.lock().send(0, &i.to_le_bytes(), alloc::vec![i]).is_ok();
            let m = chan.lock().recv(1);
            ok &= matches!(&m, Ok(msg) if msg.data == i.to_le_bytes() && msg.handles == [i]);
        }
        let (_, ra) = table.remove(a).unwrap_or(((Arc::clone(&chan), 0), 0));
        ok &= ra == rights::ALL && table.get(a, 0).err() == Some(Error::BadHandle);
        ok &= chan.lock().closed(0).is_some();
        ok &= chan.lock().recv(1).err() == Some(Error::PeerClosed);
        drop(table);
    }
    ok &= heap_in_use() == baseline;
    check("capabilities + IPC", ok, format_args!("100 messages with handles, rights enforced, peer-closed, no leak"));
}

/// Portable kernel entry, called by every backend after its early setup.
pub fn kernel_main(platform: &Platform) -> ! {
    println!(
        "coco-port {} on {} ({} bits) - memory map from {}",
        env!("CARGO_PKG_VERSION"),
        arch::NAME,
        usize::BITS,
        platform.source
    );
    for r in platform.ram.iter().take(platform.ram_len) {
        println!("  ram      {:#012x} +{:#x}", r.base, r.size);
    }
    for r in platform.reserved.iter().take(platform.reserved_len) {
        println!("  reserved {:#012x} +{:#x}", r.base, r.size);
    }
    let (kstart, kend) = arch::kernel_image();
    let mut excluded = [Range::default(); 17];
    excluded[0] = Range { base: kstart, size: kend - kstart };
    for (slot, r) in excluded[1..].iter_mut().zip(platform.reserved.iter().take(platform.reserved_len)) {
        *slot = *r;
    }
    let (usable, n) = usable_ranges(&platform.ram[..platform.ram_len], &excluded[..1 + platform.reserved_len]);
    println!("  kernel   {:#012x} +{:#x}; {} usable range(s)", kstart, kend - kstart, n);

    test_traps_and_timer();
    match test_frames(&usable[..n]) {
        Some((_frames, heap)) => {
            test_heap(heap);
            test_ipc_and_capabilities();
        }
        None => check("frame allocator", false, format_args!("no usable memory")),
    }
    if cfg!(feature = "inject-check-failure") {
        check("injected failure", false, format_args!("negative test of the harness"));
    }
    #[cfg(feature = "inject-fault")]
    illegal_instruction();
    let failures = FAILURES.load(Ordering::Relaxed);
    if failures == 0 {
        println!("coco-port: ALL TESTS PASSED on {}", arch::NAME);
        arch::exit(true)
    } else {
        println!("coco-port: {} TEST(S) FAILED on {}", failures, arch::NAME);
        arch::exit(false)
    }
}

/// Executes an undefined instruction (negative test: the trap handler must
/// report it and fail the run instead of continuing).
#[cfg(feature = "inject-fault")]
fn illegal_instruction() {
    println!("  injecting an illegal instruction");
    // SAFETY: each sequence is architecturally undefined and traps to the
    // backend's handler, which terminates the machine; nothing is resumed.
    unsafe {
        #[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
        core::arch::asm!("unimp", options(nostack));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("udf #0", options(nostack));
        #[cfg(target_arch = "x86")]
        core::arch::asm!("ud2", options(nostack));
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    println!("KERNEL PANIC: {info}");
    arch::exit(false)
}
