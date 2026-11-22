//! In-kernel self-tests, selected with `test=<name>` on the command line.
//!
//! Non-fatal tests return `Ok`/`Err`. Fault tests deliberately trigger a CPU
//! exception; success is judged externally (QEMU harness) from the fatal
//! diagnostic and exit code, since the kernel must stop on such faults.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::hint::black_box;
use core::sync::atomic::{AtomicU8, Ordering};

use coco_abi::layout::{BOOT_STACK_GUARD, DIRECT_MAP_BASE, PAGE_SIZE};
use coco_arch::cpu;
use coco_memory::frame::{FrameError, PhysFrame};
use coco_memory::paging::{PageFlags, PageSize};

use crate::cpu::Hardening;
use crate::mm::{AllocatorTables, KernelMemory};
use crate::{heap, interrupts, kprintln, power};

/// Scratch virtual region for paging tests (PML4 slot 506, otherwise unused).
const SCRATCH: u64 = 0xFFFF_FD00_0000_0000;
const END_OF_LIST: u64 = u64::MAX;

/// Outcome of a non-fatal self-test.
pub type TestResult = Result<(), &'static str>;

fn ensure(cond: bool, msg: &'static str) -> TestResult {
    if cond { Ok(()) } else { Err(msg) }
}

fn dm(phys: u64) -> *mut u64 {
    DIRECT_MAP_BASE.wrapping_add(phys) as *mut u64
}

/// Allocates and frees frames, writing and verifying a unique pattern in
/// every word of each frame through the direct map.
fn frames_pattern(mem: &mut KernelMemory) -> TestResult {
    const N: usize = 256;
    let before = mem.frames.free_frames();
    let mut held: [u64; N] = [0; N];
    for (i, slot) in held.iter_mut().enumerate() {
        let f = mem.frames.allocate().map_err(|_| "allocation failed")?;
        let p = dm(f.addr());
        for w in 0..512u64 {
            // SAFETY: the frame was just allocated (exclusively ours) and is
            // direct-mapped; w < 512 keeps the write inside the frame.
            unsafe { p.wrapping_add(w as usize).write_volatile(f.addr() ^ w ^ i as u64) };
        }
        *slot = f.into_raw();
    }
    for (i, &addr) in held.iter().enumerate() {
        let p = dm(addr);
        for w in 0..512u64 {
            // SAFETY: as above; the frame is still owned by this test.
            let v = unsafe { p.wrapping_add(w as usize).read_volatile() };
            ensure(v == addr ^ w ^ i as u64, "pattern mismatch (aliased frames?)")?;
        }
        // SAFETY: `addr` came from `into_raw` above and is re-wrapped once.
        mem.frames.free(unsafe { PhysFrame::from_raw(addr) }).map_err(|_| "free failed")?;
    }
    ensure(mem.frames.free_frames() == before, "free count not restored (leak)")?;
    ensure(mem.frames.check_invariants(), "allocator invariants violated")
}

/// Allocates every free frame (threading them into a list stored in the
/// frames themselves), checks OOM is reported, then frees all of them and
/// checks the allocator returned to its exact prior state.
fn frames_exhaustion(mem: &mut KernelMemory) -> TestResult {
    let before = mem.frames.free_frames();
    let t0 = cpu::rdtsc();
    let mut head = END_OF_LIST;
    let mut count = 0u64;
    loop {
        match mem.frames.allocate() {
            Ok(f) => {
                let addr = f.into_raw();
                let p = dm(addr);
                // SAFETY: frame exclusively owned and direct-mapped; word 0
                // stores the list link, word 511 a tag checked on release.
                unsafe { p.write_volatile(head) };
                // SAFETY: as above, last word of the same frame.
                unsafe { p.wrapping_add(511).write_volatile(!addr) };
                head = addr;
                count += 1;
            }
            Err(FrameError::OutOfMemory) => break,
            Err(_) => return Err("unexpected allocator error"),
        }
    }
    let t1 = cpu::rdtsc();
    ensure(count == before, "allocated count != free count")?;
    ensure(mem.frames.free_frames() == 0, "free count not zero at OOM")?;
    ensure(mem.frames.allocate().is_err(), "allocation succeeded after OOM")?;
    while head != END_OF_LIST {
        let p = dm(head);
        // SAFETY: `head` is a frame this test owns (from the list).
        let next = unsafe { p.read_volatile() };
        // SAFETY: as above.
        let tag = unsafe { p.wrapping_add(511).read_volatile() };
        ensure(tag == !head, "frame contents corrupted while allocated")?;
        // SAFETY: `head` came from `into_raw` and is released exactly once.
        mem.frames.free(unsafe { PhysFrame::from_raw(head) }).map_err(|_| "free failed")?;
        head = next;
    }
    let t2 = cpu::rdtsc();
    ensure(mem.frames.free_frames() == before, "frames leaked after exhaustion")?;
    ensure(mem.frames.check_invariants(), "allocator invariants violated")?;
    kprintln!(
        "  exhaustion: {} frames ({} MiB) allocated+freed; alloc {} cyc/frame, free {} cyc/frame",
        count,
        (count * PAGE_SIZE) >> 20,
        (t1 - t0) / count.max(1),
        (t2 - t1) / count.max(1)
    );
    Ok(())
}

/// Maps, uses and unmaps scratch pages repeatedly; the allocated-frame count
/// must be identical after every iteration (no leaks), and translations
/// must disappear after unmapping.
fn paging_cycle(mem: &mut KernelMemory) -> TestResult {
    const PAGES: u64 = 8;
    const ITERATIONS: u32 = 2000;
    let mut baseline = None;
    let t0 = cpu::rdtsc();
    for iter in 0..ITERATIONS {
        for i in 0..PAGES {
            let f = mem.frames.allocate().map_err(|_| "allocation failed")?;
            let phys = f.into_raw();
            let va = SCRATCH + i * PAGE_SIZE;
            let flush = mem
                .mapper
                .map(
                    va,
                    phys,
                    PageSize::Size4K,
                    PageFlags::WRITABLE | PageFlags::NO_EXECUTE,
                    &mut AllocatorTables(&mut mem.frames),
                )
                .map_err(|_| "map failed")?;
            cpu::invlpg(flush.addr());
            let value = u64::from(iter) << 32 | i;
            // SAFETY: `va` was just mapped writable to a frame we own.
            unsafe { (va as *mut u64).write_volatile(value) };
            // SAFETY: same frame through the direct map.
            let seen = unsafe { dm(phys).read_volatile() };
            ensure(seen == value, "VA and direct map disagree")?;
        }
        for i in 0..PAGES {
            let va = SCRATCH + i * PAGE_SIZE;
            let (phys, size, flush) = mem.mapper.unmap(va).map_err(|_| "unmap failed")?;
            cpu::invlpg(flush.addr());
            ensure(size == PageSize::Size4K, "unexpected page size")?;
            ensure(mem.mapper.translate(va).is_none(), "translation survived unmap")?;
            // SAFETY: the mapping owned this frame; unmapping returned it.
            mem.frames.free(unsafe { PhysFrame::from_raw(phys) }).map_err(|_| "free failed")?;
        }
        let allocated = mem.frames.allocated_frames();
        match baseline {
            None => baseline = Some(allocated),
            Some(b) => ensure(b == allocated, "frame count grew across map/unmap cycles (leak)")?,
        }
    }
    let t1 = cpu::rdtsc();
    kprintln!(
        "  paging: {} map+unmap cycles of {} pages, {} cyc/page, allocated frames constant at {} after cycle 1",
        ITERATIONS,
        PAGES,
        (t1 - t0) / (u64::from(ITERATIONS) * PAGES),
        baseline.unwrap_or(0)
    );
    Ok(())
}

static RODATA_PROBE: u64 = 0x0C0C_0C0C;
static DATA_PROBE: AtomicU8 = AtomicU8::new(0xC3); // `ret`, used by the NX test

/// Verifies the live page tables enforce coco's memory-protection policy.
fn protections(mem: &mut KernelMemory, nx: bool, hardening: &Hardening) -> TestResult {
    let m = &mem.mapper;
    let text = crate::kernel_main as *const () as u64;
    let rodata = core::ptr::addr_of!(RODATA_PROBE) as u64;
    let data = DATA_PROBE.as_ptr() as u64;
    let local = 0u64;
    let stack = core::ptr::addr_of!(local) as u64;
    let t = m.translate(text).ok_or("text unmapped")?;
    ensure(!t.flags.contains(PageFlags::WRITABLE), "kernel text is writable")?;
    ensure(!t.flags.contains(PageFlags::NO_EXECUTE), "kernel text is NX")?;
    let r = m.translate(rodata).ok_or("rodata unmapped")?;
    ensure(!r.flags.contains(PageFlags::WRITABLE), "kernel rodata is writable")?;
    let d = m.translate(data).ok_or("data unmapped")?;
    ensure(d.flags.contains(PageFlags::WRITABLE), "kernel data is read-only")?;
    let s = m.translate(stack).ok_or("stack unmapped")?;
    ensure(s.flags.contains(PageFlags::WRITABLE), "stack is read-only")?;
    if nx {
        ensure(r.flags.contains(PageFlags::NO_EXECUTE), "rodata executable")?;
        ensure(d.flags.contains(PageFlags::NO_EXECUTE), "data executable")?;
        ensure(s.flags.contains(PageFlags::NO_EXECUTE), "stack executable")?;
        ensure(m.root_entry(256) & PageFlags::NO_EXECUTE.bits() != 0, "direct map executable")?;
    }
    ensure(m.translate(BOOT_STACK_GUARD).is_none(), "stack guard page is mapped")?;
    ensure(m.translate(0).is_none() && m.translate(0x1000).is_none(), "lower half still mapped")?;
    ensure(cpu::read_cr0() & cpu::CR0_WP != 0, "CR0.WP disabled")?;
    for guard in crate::cpu::ist_guard_pages() {
        ensure(m.translate(guard).is_none(), "IST guard page is mapped")?;
    }
    let cr4 = cpu::read_cr4();
    ensure(!hardening.smep || cr4 & cpu::CR4_SMEP != 0, "SMEP supported but off")?;
    ensure(!hardening.smap || cr4 & cpu::CR4_SMAP != 0, "SMAP supported but off")?;
    ensure(!hardening.umip || cr4 & cpu::CR4_UMIP != 0, "UMIP supported but off")?;
    if let Some(h) = m.translate(heap::HEAP_BASE) {
        ensure(h.flags.contains(PageFlags::WRITABLE | PageFlags::NO_EXECUTE), "heap not RW+NX")?;
    }
    black_box(RODATA_PROBE);
    Ok(())
}

/// Runs the non-fatal memory test suite; returns `(passed, failed)`.
pub fn memory_suite(mem: &mut KernelMemory, nx: bool, hardening: &Hardening) -> (u32, u32) {
    type Test = fn(&mut KernelMemory, bool, &Hardening) -> TestResult;
    let tests: [(&str, Test); 4] = [
        ("frames-pattern", |m, _, _| frames_pattern(m)),
        ("frames-exhaustion", |m, _, _| frames_exhaustion(m)),
        ("paging-cycle", |m, _, _| paging_cycle(m)),
        ("protections", protections),
    ];
    let (mut pass, mut fail) = (0, 0);
    for (name, test) in tests {
        match test(mem, nx, hardening) {
            Ok(()) => {
                pass += 1;
                kprintln!("selftest {name}: PASS");
            }
            Err(why) => {
                fail += 1;
                kprintln!("selftest {name}: FAIL: {why}");
            }
        }
    }
    (pass, fail)
}

/// Checks that #BP is delivered to the handler and execution resumes.
pub fn breakpoint() -> TestResult {
    let before = crate::cpu::BREAKPOINTS.load(Ordering::Relaxed);
    cpu::breakpoint();
    ensure(crate::cpu::BREAKPOINTS.load(Ordering::Relaxed) == before + 1, "breakpoint not counted")
}

/// Reads `addr` with a raw load (no Rust-level dereference).
fn raw_read(addr: u64) -> u64 {
    let v: u64;
    // SAFETY: deliberately faulting probe for fault tests; the access is an
    // asm load (no Rust reference or pointer validity is asserted) and the
    // resulting exception is fatal by design.
    unsafe {
        core::arch::asm!("mov {v}, qword ptr [{a}]", a = in(reg) addr, v = out(reg) v, options(nostack, readonly))
    };
    v
}

#[inline(never)]
#[allow(unconditional_recursion, reason = "deliberate stack overflow test")]
fn recurse(depth: u64) -> u64 {
    let frame = black_box([depth; 32]);
    recurse(black_box(depth + 1)).wrapping_add(frame[1])
}

/// Triggers the deliberately fatal test `name`. Returns `Err` if unknown
/// or if the expected fault did not happen.
pub fn fatal_test(name: &str) -> TestResult {
    kprintln!("fault-test {name}: triggering");
    match name {
        "pagefault" => {
            black_box(raw_read(SCRATCH + 0x5000_0000));
        }
        "null" => {
            black_box(raw_read(0x10));
        }
        "locked-fault" => {
            // Lock rule L2: the trap path must not block on the console lock.
            crate::console::with_console_locked(|| {
                black_box(raw_read(SCRATCH + 0x6000_0000));
            });
        }
        "guard" => {
            black_box(raw_read(BOOT_STACK_GUARD + 8));
        }
        "stack-overflow" => {
            black_box(recurse(0));
        }
        "nx" => {
            // SAFETY: deliberately jumps into a non-executable data page; the
            // instruction fetch faults before anything executes.
            unsafe { core::arch::asm!("call {0}", in(reg) DATA_PROBE.as_ptr(), clobber_abi("C")) };
        }
        "wp" => {
            // SAFETY: deliberately writes to read-only .rodata via asm; the
            // write faults (CR0.WP) and never completes.
            unsafe {
                core::arch::asm!("mov qword ptr [{0}], 1", in(reg) core::ptr::addr_of!(RODATA_PROBE), options(nostack))
            };
        }
        "ud" => {
            // SAFETY: deliberate invalid opcode; the fault is fatal.
            unsafe { core::arch::asm!("ud2", options(nostack, nomem)) };
        }
        "divide" => {
            // SAFETY: deliberate divide-by-zero in asm (not Rust arithmetic).
            unsafe {
                core::arch::asm!("xor edx, edx", "mov eax, 1", "xor ecx, ecx", "div ecx", out("eax") _, out("edx") _, out("ecx") _, options(nostack, nomem))
            };
        }
        "panic" => {
            #[allow(clippy::panic, reason = "deliberate panic-path test")]
            {
                panic!("deliberate test panic");
            }
        }
        _ => return Err("unknown test"),
    }
    Err("expected fault did not occur")
}

/// xorshift64* generator for self-tests (deterministic).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn heap_in_use() -> usize {
    heap::stats().map_or(0, |s| s.in_use)
}

/// Exercises the heap through `alloc` collections and checks that every
/// round returns the heap to its starting `in_use` (no leaks) with all
/// contents intact.
pub fn heap_suite() -> TestResult {
    let base = heap_in_use();
    let t0 = cpu::rdtsc();
    let mut ops = 0u64;
    for round in 0..3u64 {
        let mut v: Vec<u64> = Vec::new();
        for i in 0..50_000 {
            v.push(i ^ round);
        }
        ensure(v.iter().enumerate().all(|(i, &x)| x == i as u64 ^ round), "Vec contents corrupted")?;
        ops += 50_000;
        let mut boxes: Vec<Box<[u8]>> =
            (0..1500usize).map(|i| alloc::vec![i as u8; 1 + (i * 37) % 6000].into_boxed_slice()).collect();
        for (i, b) in boxes.iter().enumerate() {
            ensure(b.iter().all(|&x| x == i as u8), "boxed contents corrupted")?;
        }
        let mut i = 0;
        boxes.retain(|_| {
            i += 1;
            i % 2 == 0
        });
        boxes.extend((0..750usize).map(|i| alloc::vec![0xAB; 1 + i * 11].into_boxed_slice()));
        ops += 2250;
        let mut map = BTreeMap::new();
        let mut rng = Rng(0xC0C0 + round);
        for _ in 0..20_000 {
            let k = rng.next() % 4096;
            if rng.next().is_multiple_of(3) {
                map.remove(&k);
            } else {
                map.insert(k, alloc::vec![k; 1 + (k % 8) as usize]);
            }
        }
        ensure(map.iter().all(|(k, v)| v.iter().all(|x| x == k)), "map contents corrupted")?;
        ops += 20_000;
        drop((v, boxes, map));
        ensure(heap_in_use() == base, "heap in-use bytes did not return to baseline (leak)")?;
    }
    let cycles = cpu::rdtsc() - t0;
    let s = heap::stats().unwrap_or_default();
    kprintln!(
        "  heap: 3 churn rounds, ~{} ops, {} cyc/op; in use {} B (baseline {} B), peak {} KiB, mapped {} KiB",
        ops,
        cycles / ops.max(1),
        s.in_use,
        base,
        s.peak / 1024,
        s.mapped / 1024
    );
    Ok(())
}

/// Allocates 1 MiB blocks until the heap reports exhaustion (frames run
/// out), checks the failure is a clean `Err`, frees everything and checks
/// the heap is fully reusable.
pub fn heap_exhaustion() -> TestResult {
    let base = heap_in_use();
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    ensure(blocks.try_reserve(4096).is_ok(), "cannot reserve bookkeeping")?;
    loop {
        let mut b: Vec<u8> = Vec::new();
        if b.try_reserve_exact(1 << 20).is_err() {
            break;
        }
        b.push(blocks.len() as u8);
        if blocks.len() == blocks.capacity() {
            break; // bookkeeping exhausted before memory (very large RAM)
        }
        blocks.push(b);
    }
    let count = blocks.len();
    ensure(count > 0, "no allocation succeeded")?;
    ensure(blocks.iter().enumerate().all(|(i, b)| b.first() == Some(&(i as u8))), "block contents corrupted")?;
    drop(blocks);
    ensure(heap_in_use() == base, "heap did not recover after exhaustion")?;
    let mut again: Vec<u8> = Vec::new();
    ensure(again.try_reserve_exact(64 << 20).is_ok(), "freed memory not reusable")?;
    drop(again);
    let s = heap::stats().unwrap_or_default();
    kprintln!("  heap exhaustion: {} x 1 MiB before OOM, {} failed attempt(s), recovered", count, s.failures);
    Ok(())
}

/// Measures 1 s of timer ticks against the calibrated TSC.
pub fn timer_accuracy() -> TestResult {
    let hz = interrupts::tsc_hz();
    ensure(hz > 0, "TSC not calibrated")?;
    interrupts::sleep_ticks(1);
    let t0 = cpu::rdtsc();
    interrupts::sleep_ticks(interrupts::HZ);
    let ms = (cpu::rdtsc() - t0) * 1000 / hz;
    let (spurious, unexpected) = interrupts::anomaly_counts();
    kprintln!(
        "  timer: {} ticks took {} ms by TSC; spurious {}, unexpected {}",
        interrupts::HZ,
        ms,
        spurious,
        unexpected
    );
    ensure((900..=1100).contains(&ms), "tick rate off by more than 10%")?;
    ensure(unexpected == 0, "unexpected interrupt vectors")
}

/// The timer handler is made to allocate; the allocator must refuse,
/// which aborts through the allocation-error path (expected fatal).
pub fn irq_alloc() -> TestResult {
    interrupts::ALLOC_IN_IRQ_TEST.store(true, Ordering::Relaxed);
    interrupts::sleep_ticks(3);
    interrupts::ALLOC_IN_IRQ_TEST.store(false, Ordering::Relaxed);
    Err("allocation in interrupt context was not refused")
}

/// Boot / reboot / power-off lifecycle: counts boots in CMOS, reboots
/// until `cycles` boots happened, then powers off through ACPI S5.
pub fn lifecycle(acpi: Option<&coco_acpi::AcpiInfo>, cycles: u8, prior_failures: u32) -> TestResult {
    let n = power::bump_boot_counter();
    kprintln!("lifecycle: boot {} of {}", n, cycles);
    if prior_failures > 0 {
        power::clear_boot_counter();
        return Err("self-tests failed during lifecycle boot");
    }
    if n < cycles {
        power::reboot(acpi);
        return Err("reboot did not happen");
    }
    power::clear_boot_counter();
    kprintln!("lifecycle: {} boots complete, powering off", cycles);
    power::shutdown(acpi)?;
    Err("power-off did not happen")
}
