#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::collections::{BTreeMap, BTreeSet};
use std::vec::Vec;

use crate::PAGE_SIZE;
use crate::frame::*;
use crate::paging::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn alloc_with(frames: u64) -> (Vec<u64>, Vec<u64>) {
    (std::vec![0; words_for(frames).unwrap()], std::vec![0; words_for(frames).unwrap()])
}

// ---------------------------------------------------------------- frames

#[test]
fn frame_basic_alloc_free() {
    let (mut m, mut f) = alloc_with(1024);
    let mut a = FrameAllocator::new(&mut m, &mut f, 1024).unwrap();
    assert_eq!(a.allocate(), Err(FrameError::OutOfMemory));
    a.add_free_range(0x10000, 4).unwrap();
    assert_eq!(a.free_frames(), 4);
    let f1 = a.allocate().unwrap();
    assert_eq!(f1.addr(), 0x10000);
    assert!(!a.is_free(0x10000));
    assert_eq!(a.allocated_frames(), 1);
    a.free(f1).unwrap();
    assert_eq!(a.free_frames(), 4);
    assert!(a.check_invariants());
}

/// Regression (32-bit audit): sizes must never be truncated. On a 32-bit
/// target 2^40 frames need 2^34 bitmap words, which does not fit in usize.
#[test]
fn frame_counts_never_truncate() {
    let huge = 1u64 << 40;
    #[cfg(target_pointer_width = "64")]
    assert_eq!(words_for(huge), Some((huge / 64) as usize), "64-bit: exact");
    #[cfg(target_pointer_width = "32")]
    assert_eq!(words_for(huge), None, "32-bit: refused, never truncated");
    let mut a = [0u64; 4];
    let mut b = [0u64; 4];
    let r = FrameAllocator::new(&mut a, &mut b, huge);
    assert!(matches!(r, Err(FrameError::StorageTooSmall | FrameError::OutOfRange)), "tiny storage accepted");
    assert_eq!(words_for(0), Some(0));
    assert_eq!(words_for(65), Some(2));
}

#[test]
fn frame_storage_and_range_errors() {
    let mut small = [0u64; 1];
    let mut small2 = [0u64; 1];
    assert_eq!(FrameAllocator::new(&mut small, &mut small2, 65).unwrap_err(), FrameError::StorageTooSmall);
    let (mut m, mut f) = alloc_with(128);
    let mut a = FrameAllocator::new(&mut m, &mut f, 128).unwrap();
    assert_eq!(a.add_free_range(0x1001, 1), Err(FrameError::Misaligned));
    assert_eq!(a.add_free_range(127 * PAGE_SIZE, 2), Err(FrameError::OutOfRange));
    assert_eq!(a.add_free_range(u64::MAX - 4095, 2), Err(FrameError::OutOfRange));
    assert_eq!(a.add_free_range(0, u64::MAX), Err(FrameError::OutOfRange));
    assert_eq!(a.add_free_range(0, 0), Ok(()));
    a.add_free_range(0, 10).unwrap();
    // Overlap is rejected atomically: the new frames 10..12 must not be added.
    assert_eq!(a.add_free_range(8 * PAGE_SIZE, 4), Err(FrameError::AlreadyManaged));
    assert_eq!(a.managed_frames(), 10);
    assert!(!a.is_free(10 * PAGE_SIZE));
    assert!(a.check_invariants());
}

#[test]
fn frame_double_free_and_forgery_detected() {
    let (mut m, mut f) = alloc_with(64);
    let mut a = FrameAllocator::new(&mut m, &mut f, 64).unwrap();
    a.add_free_range(0, 4).unwrap();
    let fr = a.allocate().unwrap();
    let raw = fr.into_raw();
    // SAFETY: test re-wraps the frame it just took ownership of.
    a.free(unsafe { PhysFrame::from_raw(raw) }).unwrap();
    // SAFETY: deliberately forged to exercise runtime double-free detection.
    assert_eq!(a.free(unsafe { PhysFrame::from_raw(raw) }), Err(FrameError::DoubleFree));
    // SAFETY: deliberately forged token for an unmanaged frame.
    assert_eq!(a.free(unsafe { PhysFrame::from_raw(40 * PAGE_SIZE) }), Err(FrameError::NotManaged));
    // SAFETY: deliberately forged misaligned token.
    assert_eq!(a.free(unsafe { PhysFrame::from_raw(3) }), Err(FrameError::Misaligned));
    // SAFETY: deliberately forged out-of-range token.
    assert_eq!(a.free(unsafe { PhysFrame::from_raw(1 << 40) }), Err(FrameError::OutOfRange));
    assert_eq!(a.free_frames(), 4);
    assert!(a.check_invariants());
}

/// Exhaust memory, verify OOM is reported (not a panic), then free everything
/// and verify the allocator returns to its exact initial state (leak check).
#[test]
fn frame_exhaustion_and_recovery() {
    let frames = 4096u64;
    let (mut m, mut f) = alloc_with(frames);
    let mut a = FrameAllocator::new(&mut m, &mut f, frames).unwrap();
    a.add_free_range(0x1000, 1000).unwrap();
    a.add_free_range(0x80_0000, 1000).unwrap(); // frames 2048..3048 (bitmap covers 4096)
    for round in 0..3 {
        let mut held = Vec::new();
        let mut seen = BTreeSet::new();
        loop {
            match a.allocate() {
                Ok(fr) => {
                    assert!(seen.insert(fr.addr()), "frame handed out twice");
                    held.push(fr);
                }
                Err(e) => {
                    assert_eq!(e, FrameError::OutOfMemory);
                    break;
                }
            }
        }
        assert_eq!(held.len(), 2000, "round {round}");
        assert_eq!(a.free_frames(), 0);
        assert_eq!(a.allocate(), Err(FrameError::OutOfMemory));
        for fr in held {
            a.free(fr).unwrap();
        }
        assert_eq!(a.free_frames(), 2000);
        assert!(a.check_invariants());
    }
}

/// Property test against a model: random interleavings of allocate/free.
#[test]
fn frame_random_ops_match_model() {
    let frames = 2048u64;
    let (mut m, mut f) = alloc_with(frames);
    let mut a = FrameAllocator::new(&mut m, &mut f, frames).unwrap();
    let mut rng = Rng(42);
    let mut usable = BTreeSet::new();
    let mut base = 0u64;
    while base < frames {
        let len = 1 + rng.below(50);
        if rng.below(2) == 0 && base + len <= frames {
            a.add_free_range(base * PAGE_SIZE, len).unwrap();
            usable.extend(base..base + len);
        }
        base += len + rng.below(20);
    }
    let mut held: Vec<PhysFrame> = Vec::new();
    let mut held_set = BTreeSet::new();
    for _ in 0..if cfg!(miri) { 3000 } else { 100_000 } {
        if rng.below(3) != 0 {
            match a.allocate() {
                Ok(fr) => {
                    let n = fr.addr() / PAGE_SIZE;
                    assert!(usable.contains(&n), "allocated unmanaged frame");
                    assert!(held_set.insert(n), "duplicate allocation");
                    held.push(fr);
                }
                Err(e) => {
                    assert_eq!(e, FrameError::OutOfMemory);
                    assert_eq!(held.len() as u64, a.managed_frames());
                }
            }
        } else if !held.is_empty() {
            let i = rng.below(held.len() as u64) as usize;
            let fr = held.swap_remove(i);
            held_set.remove(&(fr.addr() / PAGE_SIZE));
            a.free(fr).unwrap();
        }
        assert_eq!(a.allocated_frames(), held.len() as u64);
    }
    assert!(a.check_invariants());
    for fr in held {
        a.free(fr).unwrap();
    }
    assert_eq!(a.free_frames(), usable.len() as u64);
}

// ---------------------------------------------------------------- paging

/// Simulated physical memory: `frames` 4 KiB frames at physical 0.
/// The arena pointer is derived once so raw accesses stay valid under Miri.
struct Sim {
    _storage: Vec<u64>,
    base: *mut u64,
    frames: u64,
}

impl Sim {
    fn new(frames: u64) -> Self {
        let mut storage = std::vec![0u64; (frames * 512) as usize];
        let base = storage.as_mut_ptr();
        Self { _storage: storage, base, frames }
    }
}

#[derive(Clone, Copy)]
struct SimAccess(*mut u64, u64);

// SAFETY: every table address the mapper uses comes from `SimFrames`, which
// only returns frames inside the arena; the arena outlives the mapper and no
// references into it are held while the mapper runs.
unsafe impl PhysAccess for SimAccess {
    fn table(&self, phys: u64) -> *mut u64 {
        assert!(phys.is_multiple_of(PAGE_SIZE) && phys / PAGE_SIZE < self.1, "table outside arena: {phys:#x}");
        self.0.wrapping_add((phys / 8) as usize)
    }
}

struct SimFrames {
    next: u64,
    limit: u64,
    handed: u64,
}

impl TableFrameSource for SimFrames {
    fn allocate_table(&mut self) -> Option<u64> {
        if self.next >= self.limit {
            return None;
        }
        let f = self.next * PAGE_SIZE;
        self.next += 1;
        self.handed += 1;
        Some(f)
    }
}

fn mapper(sim: &Sim, table_frames: u64) -> (Mapper<SimAccess>, SimFrames) {
    // Frame 0 is the root; tables come from frames 1..=table_frames.
    // SAFETY: frame 0 of a zeroed arena is a valid empty PML4 and the arena
    // is only reached through this mapper.
    let m = unsafe { Mapper::new(0, SimAccess(sim.base, sim.frames)) };
    (m, SimFrames { next: 1, limit: 1 + table_frames, handed: 0 })
}

const KBASE: u64 = 0xFFFF_FFFF_8000_0000;

#[test]
fn map_translate_unmap_4k() {
    let sim = Sim::new(16);
    let (mut m, mut fs) = mapper(&sim, 15);
    let flags = PageFlags::WRITABLE | PageFlags::NO_EXECUTE;
    m.map(KBASE, 0x1234_5000, PageSize::Size4K, flags, &mut fs).unwrap().ignore();
    assert_eq!(fs.handed, 3); // PDPT, PD, PT
    let t = m.translate(KBASE + 0x123).unwrap();
    assert_eq!(t.phys, 0x1234_5123);
    assert_eq!(t.size, PageSize::Size4K);
    assert!(t.flags.contains(PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE));
    assert!(!t.flags.contains(PageFlags::USER));
    assert_eq!(m.translate(KBASE + 0x1000), None);
    // Second page in the same table needs no new tables.
    m.map(KBASE + 0x1000, 0x5000, PageSize::Size4K, PageFlags::EMPTY, &mut fs).unwrap().ignore();
    assert_eq!(fs.handed, 3);
    assert_eq!(m.map(KBASE, 0x6000, PageSize::Size4K, flags, &mut fs).unwrap_err(), MapError::AlreadyMapped);
    let (phys, size, flush) = m.unmap(KBASE + 0x10).unwrap();
    assert_eq!((phys, size, flush.addr()), (0x1234_5000, PageSize::Size4K, KBASE));
    assert_eq!(m.translate(KBASE), None);
    assert_eq!(m.unmap(KBASE).unwrap_err(), MapError::NotMapped);
    // Remap after unmap works.
    m.map(KBASE, 0x7000, PageSize::Size4K, flags, &mut fs).unwrap().ignore();
    assert_eq!(m.translate(KBASE).unwrap().phys, 0x7000);
}

#[test]
fn map_2m_and_conflicts() {
    let sim = Sim::new(16);
    let (mut m, mut fs) = mapper(&sim, 15);
    let v = 0xFFFF_8000_0020_0000;
    m.map(v, 0x20_0000, PageSize::Size2M, PageFlags::WRITABLE, &mut fs).unwrap().ignore();
    let t = m.translate(v + 0x1_2345).unwrap();
    assert_eq!((t.phys, t.size), (0x21_2345, PageSize::Size2M));
    assert!(t.flags.contains(PageFlags::HUGE));
    // 4K inside a 2M leaf is a size conflict, not a silent split.
    assert_eq!(m.map(v + 0x1000, 0, PageSize::Size4K, PageFlags::EMPTY, &mut fs).unwrap_err(), MapError::SizeConflict);
    // 2M over an existing 4K table is a size conflict.
    m.map(v + 0x20_0000, 0, PageSize::Size4K, PageFlags::EMPTY, &mut fs).unwrap().ignore();
    assert_eq!(
        m.map(v + 0x20_0000, 0, PageSize::Size2M, PageFlags::EMPTY, &mut fs).unwrap_err(),
        MapError::SizeConflict
    );
    let (phys, size, _f) = m.unmap(v + 0x1000).unwrap();
    assert_eq!((phys, size), (0x20_0000, PageSize::Size2M));
}

#[test]
fn map_rejects_invalid_requests() {
    let sim = Sim::new(8);
    let (mut m, mut fs) = mapper(&sim, 7);
    let e = |m: &mut Mapper<SimAccess>, fs: &mut SimFrames, v, p, s, f| m.map(v, p, s, f, fs).unwrap_err();
    assert_eq!(
        e(&mut m, &mut fs, 0x0000_8000_0000_0000, 0, PageSize::Size4K, PageFlags::EMPTY),
        MapError::NonCanonical
    );
    assert_eq!(e(&mut m, &mut fs, KBASE + 1, 0, PageSize::Size4K, PageFlags::EMPTY), MapError::Misaligned);
    assert_eq!(e(&mut m, &mut fs, KBASE, 0x1000, PageSize::Size2M, PageFlags::EMPTY), MapError::Misaligned);
    assert_eq!(e(&mut m, &mut fs, KBASE, 1 << 52, PageSize::Size4K, PageFlags::EMPTY), MapError::PhysOutOfRange);
    assert_eq!(e(&mut m, &mut fs, KBASE, 0, PageSize::Size4K, PageFlags::HUGE), MapError::InvalidFlags);
    assert_eq!(e(&mut m, &mut fs, KBASE, 0, PageSize::Size4K, PageFlags::from_bits(1 << 52)), MapError::InvalidFlags);
    assert_eq!(m.unmap(0x0000_8000_0000_0000).unwrap_err(), MapError::NonCanonical);
    assert_eq!(m.translate(0x0000_8000_0000_0000), None);
}

#[test]
fn map_reports_table_exhaustion() {
    let sim = Sim::new(4);
    let (mut m, mut fs) = mapper(&sim, 2); // needs 3 for a 4K mapping
    assert_eq!(m.map(KBASE, 0, PageSize::Size4K, PageFlags::EMPTY, &mut fs).unwrap_err(), MapError::OutOfFrames);
    assert_eq!(m.translate(KBASE), None);
}

#[test]
fn user_bit_propagates_to_intermediates() {
    let sim = Sim::new(8);
    let (mut m, mut fs) = mapper(&sim, 7);
    m.map(0x40_0000, 0x1000, PageSize::Size4K, PageFlags::USER, &mut fs).unwrap().ignore();
    assert!(m.root_entry(0) & PageFlags::USER.bits() != 0);
    m.map(KBASE, 0x2000, PageSize::Size4K, PageFlags::EMPTY, &mut fs).unwrap().ignore();
    assert!(m.root_entry(511) & PageFlags::USER.bits() == 0, "kernel tables must not be user-accessible");
}

/// Model-based property test: random map/unmap of 4K and 2M pages across
/// several PML4 slots, checked against a BTreeMap after each step.
#[test]
fn paging_random_ops_match_model() {
    let sim = Sim::new(600);
    let (mut m, mut fs) = mapper(&sim, 599);
    let mut rng = Rng(7);
    let mut model: BTreeMap<u64, (u64, PageSize)> = BTreeMap::new();
    // Level-2 slots that own a 4K page table. The mapper never frees
    // intermediate tables, so a 2M mapping there is a size conflict even
    // after every 4K page was unmapped (documented behaviour).
    let mut tabled: BTreeSet<u64> = BTreeSet::new();
    let regions = [0x0000_0000_0000_0000u64, 0xFFFF_8000_0000_0000, KBASE, 0x0000_7F00_0000_0000];
    for step in 0..if cfg!(miri) { 1500 } else { 20_000 } {
        let size = if rng.below(4) == 0 { PageSize::Size2M } else { PageSize::Size4K };
        let region = regions[rng.below(regions.len() as u64) as usize];
        let slot = rng.below(64);
        let v = region + slot * LARGE_PAGE_SIZE + if size == PageSize::Size4K { rng.below(512) * PAGE_SIZE } else { 0 };
        let large_base = v & !(LARGE_PAGE_SIZE - 1);
        if rng.below(2) == 0 {
            let p = (rng.below(1 << 20) * size.bytes()) & 0x000F_FFFF_FFFF_F000;
            let result = m.map(v, p, size, PageFlags::WRITABLE, &mut fs);
            let conflict = match size {
                PageSize::Size2M => {
                    tabled.contains(&large_base)
                        || model.range(large_base..large_base + LARGE_PAGE_SIZE).next().is_some()
                }
                PageSize::Size4K => {
                    model.contains_key(&v) || model.get(&large_base).is_some_and(|e| e.1 == PageSize::Size2M)
                }
            };
            assert_eq!(result.is_ok(), !conflict, "step {step} map {v:#x} {size:?}");
            if let Ok(f) = result {
                f.ignore();
                model.insert(v, (p, size));
                if size == PageSize::Size4K {
                    tabled.insert(large_base);
                }
            }
        } else {
            let result = m.unmap(v);
            let key = if model.get(&large_base).is_some_and(|e| e.1 == PageSize::Size2M) { large_base } else { v };
            match model.remove(&key) {
                Some((p, s)) => {
                    let (phys, size, _f) = result.unwrap();
                    assert_eq!((phys, size), (p, s));
                }
                None => assert_eq!(result.unwrap_err(), MapError::NotMapped),
            }
        }
        if step % 512 == 0 {
            for (&v, &(p, s)) in &model {
                let t = m.translate(v + 8).unwrap();
                assert_eq!((t.phys, t.size), (p + 8, s));
            }
        }
    }
}

#[test]
fn canonical_addresses() {
    assert!(is_canonical(0));
    assert!(is_canonical(0x0000_7FFF_FFFF_FFFF));
    assert!(!is_canonical(0x0000_8000_0000_0000));
    assert!(!is_canonical(0xFFFF_7FFF_FFFF_FFFF));
    assert!(is_canonical(0xFFFF_8000_0000_0000));
    assert!(is_canonical(u64::MAX));
}

// ---------------------------------------------------------------- heap

use crate::heap::*;

/// Arena backend: grows within a pre-allocated buffer, with an optional
/// byte budget to exercise OOM.
struct ArenaBackend {
    budget: usize,
    grown: usize,
    calls: u32,
}

impl HeapBackend for ArenaBackend {
    fn grow(&mut self, _start: *mut u8, len: usize) -> bool {
        self.calls += 1;
        if self.grown + len > self.budget {
            return false;
        }
        self.grown += len;
        true
    }
}

/// Page-aligned arena that outlives the heap built on it.
struct Arena {
    _buf: Vec<u8>,
    base: *mut u8,
    len: usize,
}

impl Arena {
    fn new(len: usize) -> Self {
        let mut buf = std::vec![0u8; len + HEAP_PAGE];
        let raw = buf.as_mut_ptr();
        let pad = raw.align_offset(HEAP_PAGE);
        Self { _buf: buf, base: raw.wrapping_add(pad), len }
    }
}

fn heap(arena: &Arena, budget: usize, poison: bool) -> Heap<ArenaBackend> {
    // SAFETY: the arena is reserved for this heap and outlives it; the
    // backend only accounts, the memory is always valid.
    unsafe { Heap::new(arena.base, arena.len, HeapConfig { poison }, ArenaBackend { budget, grown: 0, calls: 0 }) }
}

#[test]
fn heap_class_math() {
    assert_eq!(class_of(1, 1), Some(0));
    assert_eq!(class_of(16, 8), Some(0));
    assert_eq!(class_of(17, 8), Some(1));
    assert_eq!(class_of(8, 64), Some(2));
    assert_eq!(class_of(2048, 1), Some(7));
    assert_eq!(class_of(2049, 1), None);
    assert_eq!(class_of(1, 4096), None);
}

#[test]
fn heap_small_and_large_roundtrip() {
    let arena = Arena::new(1 << 20);
    let mut h = heap(&arena, usize::MAX, true);
    let a = h.allocate(24, 8).unwrap();
    let b = h.allocate(24, 8).unwrap();
    assert_ne!(a, b);
    assert_eq!(a as usize % 32, 0);
    let big = h.allocate(10_000, 4096).unwrap();
    assert_eq!(big as usize % 4096, 0);
    assert_eq!(h.stats().in_use, 32 + 32 + 3 * 4096);
    h.free(a, 24, 8).unwrap();
    h.free(big, 10_000, 4096).unwrap();
    h.free(b, 24, 8).unwrap();
    assert_eq!(h.stats().in_use, 0);
    assert!(h.check_runs());
    // Freed large run is reused first-fit.
    assert_eq!(h.allocate(8192, 8).unwrap(), big);
}

#[test]
fn heap_detects_double_free_and_bad_pointers() {
    let arena = Arena::new(1 << 20);
    let mut h = heap(&arena, usize::MAX, true);
    let a = h.allocate(100, 8).unwrap();
    h.free(a, 100, 8).unwrap();
    assert_eq!(h.free(a, 100, 8), Err(HeapError::DoubleFree(a as usize)));
    let big = h.allocate(5000, 8).unwrap();
    h.free(big, 5000, 8).unwrap();
    assert_eq!(h.free(big, 5000, 8), Err(HeapError::DoubleFree(big as usize)));
    let c = h.allocate(100, 8).unwrap();
    assert_eq!(h.free(c.wrapping_add(8), 100, 8), Err(HeapError::InvalidPointer(c as usize + 8)));
    let outside = arena.base.wrapping_add(arena.len - 16);
    assert!(matches!(h.free(outside, 16, 8), Err(HeapError::InvalidPointer(_))));
    assert_eq!(h.allocate(0, 8), Err(HeapError::Unsupported));
    assert_eq!(h.allocate(8, 8192), Err(HeapError::Unsupported));
    assert_eq!(h.allocate(8, 3), Err(HeapError::Unsupported));
    h.free(c, 100, 8).unwrap();
    assert_eq!(h.stats().in_use, 0);
}

#[test]
fn heap_detects_use_after_free_write() {
    let arena = Arena::new(1 << 20);
    let mut h = heap(&arena, usize::MAX, true);
    let a = h.allocate(64, 8).unwrap();
    h.free(a, 64, 8).unwrap();
    // SAFETY: deliberately writes into the freed (but still owned by the
    // arena) block to simulate a use-after-free bug.
    unsafe { a.wrapping_add(40).write(0x41) };
    assert_eq!(h.allocate(64, 8), Err(HeapError::Corruption(a as usize)));
}

#[test]
fn heap_out_of_memory_and_recovery() {
    let arena = Arena::new(1 << 20);
    let mut h = heap(&arena, 256 * 1024, false);
    let mut live = Vec::new();
    loop {
        match h.allocate(4096 * 3, 8) {
            Ok(p) => live.push(p),
            Err(e) => {
                assert_eq!(e, HeapError::OutOfMemory);
                break;
            }
        }
    }
    assert!(!live.is_empty() && h.stats().failures == 1);
    for p in live.drain(..) {
        h.free(p, 4096 * 3, 8).unwrap();
    }
    assert_eq!(h.stats().in_use, 0);
    assert!(h.check_runs());
    // Everything is reusable after recovery without growing further.
    let calls = h.backend_mut().calls;
    let p = h.allocate(4096 * 3, 8).unwrap();
    assert_eq!(h.backend_mut().calls, calls);
    h.free(p, 4096 * 3, 8).unwrap();
}

/// Model-based property test: random sizes/alignments, contents written
/// and verified, no overlaps, exact accounting, full recovery.
#[test]
fn heap_random_ops_match_model() {
    let arena = Arena::new(32 << 20);
    let mut h = heap(&arena, usize::MAX, true);
    let mut rng = Rng(0x4EA9);
    let mut live: Vec<(*mut u8, usize, usize, u8)> = Vec::new();
    let mut ranges: BTreeMap<usize, usize> = BTreeMap::new();
    let steps = if cfg!(miri) { 400 } else { 40_000 };
    for step in 0..steps {
        // Bounded live set (<= 1000 blocks, <= ~20 MiB worst case) so this
        // test checks correctness; exhaustion has its own test.
        if live.is_empty() || (live.len() < 1000 && rng.below(2) == 0) {
            let size = match rng.below(10) {
                0..=6 => 1 + rng.below(200) as usize,
                7 | 8 => 1 + rng.below(2048) as usize,
                _ => 1 + rng.below(20_000) as usize,
            };
            let align = 1usize << rng.below(13);
            let p = match h.allocate(size, align) {
                Ok(p) => p,
                Err(e) => {
                    panic!("step {step}: {e:?} size {size} align {align} live {} stats {:?}", live.len(), h.stats())
                }
            };
            assert_eq!(p as usize % align, 0, "step {step}: misaligned");
            let start = p as usize;
            if let Some((&s, &e)) = ranges.range(..start + size).next_back() {
                assert!(e <= start || s >= start + size, "step {step}: overlap");
            }
            ranges.insert(start, start + size);
            let tag = step as u8;
            // SAFETY: freshly allocated block of `size` bytes.
            unsafe { core::ptr::write_bytes(p, tag, size) };
            live.push((p, size, align, tag));
        } else {
            let i = rng.below(live.len() as u64) as usize;
            let (p, size, align, tag) = live.swap_remove(i);
            // SAFETY: live block of `size` bytes owned by the test.
            let bytes = unsafe { core::slice::from_raw_parts(p, size) };
            assert!(bytes.iter().all(|&b| b == tag), "step {step}: contents corrupted");
            ranges.remove(&(p as usize));
            h.free(p, size, align).unwrap();
        }
    }
    assert!(h.check_runs());
    for (p, size, align, _) in live.drain(..) {
        h.free(p, size, align).unwrap();
    }
    assert_eq!(h.stats().in_use, 0);
    assert!(h.check_runs());
    assert_eq!(h.stats().allocations, h.stats().frees);
}

/// Teardown returns exactly the mapped leaves and every table frame the
/// mapper allocated for those slots, and leaves other slots intact.
#[test]
fn teardown_returns_every_frame() {
    let sim = Sim::new(800);
    let (mut m, mut fs) = mapper(&sim, 799);
    let mut rng = Rng(99);
    let mut mapped = BTreeSet::new();
    // Kernel-half mapping that must survive.
    m.map(KBASE, 0x9_9000, PageSize::Size4K, PageFlags::EMPTY, &mut fs).unwrap().ignore();
    let kernel_tables = fs.handed;
    for _ in 0..if cfg!(miri) { 60 } else { 600 } {
        let v = (rng.below(4) << 39) | (rng.below(8) << 30) | (rng.below(64) << 21) | (rng.below(512) << 12);
        let p = rng.below(1 << 30) << 12;
        if m.map(v, p, PageSize::Size4K, PageFlags::USER | PageFlags::WRITABLE, &mut fs).is_ok() {
            mapped.insert(p);
        }
    }
    let v2 = 5u64 << 39;
    m.map(v2, 0x4000_0000, PageSize::Size2M, PageFlags::USER, &mut fs).unwrap().ignore();
    let mut leaves = BTreeSet::new();
    let mut tables = 0u64;
    // SAFETY: slots 0..256 belong only to this simulated user space.
    unsafe {
        m.teardown_slots(
            0..256,
            &mut |p, s| {
                assert!(leaves.insert((p, s)), "leaf reported twice");
            },
            &mut |_t| tables += 1,
        );
    }
    let small: BTreeSet<u64> = leaves.iter().filter(|(_, s)| *s == PageSize::Size4K).map(|(p, _)| *p).collect();
    assert_eq!(small, mapped);
    assert!(leaves.contains(&(0x4000_0000, PageSize::Size2M)));
    assert_eq!(tables, fs.handed - kernel_tables, "every user table frame returned");
    for slot in 0..256 {
        assert_eq!(m.root_entry(slot), 0);
    }
    assert_eq!(m.translate(KBASE).unwrap().phys, 0x9_9000, "kernel half survives");
}
