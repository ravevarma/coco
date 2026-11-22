#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::vec::Vec;

use super::*;
use crate::cmdline::Cmdline;
use crate::layout::*;
use crate::memory::{kind_from_uefi, normalize, uefi_types, validate_sorted};

/// Deterministic xorshift64* generator for property tests.
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

fn good_info() -> BootInfo {
    BootInfo {
        magic: BOOT_INFO_MAGIC,
        version: BOOT_INFO_VERSION,
        header_size: core::mem::size_of::<BootInfo>() as u32,
        memory_regions: DIRECT_MAP_BASE + 0x10_0000,
        memory_region_count: 4,
        framebuffer: FramebufferInfo {
            phys_base: 0x8000_0000,
            virt_base: DIRECT_MAP_BASE + 0x8000_0000,
            byte_len: 1280 * 800 * 4,
            width: 1280,
            height: 800,
            stride: 1280,
            format: PixelFormat::Bgrx as u32,
        },
        cmdline: DIRECT_MAP_BASE + 0x20_0000,
        cmdline_len: 8,
        acpi_rsdp: 0,
        stack_bottom: BOOT_STACK_BOTTOM,
        stack_top: BOOT_STACK_TOP,
        direct_map_base: DIRECT_MAP_BASE,
        direct_map_len: 0x1_0000_0000,
        tsc_loader_start: 0,
        tsc_handoff: 0,
        flags: 0,
        init_image: DIRECT_MAP_BASE + 0x30_0000,
        init_image_len: 4096,
    }
}

#[test]
fn header_accepts_well_formed() {
    assert_eq!(good_info().validate_header(), Ok(()));
}

#[test]
fn header_rejects_each_corruption() {
    type Case = (fn(&mut BootInfo), BootInfoError);
    let cases: [Case; 12] = [
        (|b| b.magic ^= 1, BootInfoError::BadMagic),
        (|b| b.version = 99, BootInfoError::BadVersion(99)),
        (|b| b.header_size = 8, BootInfoError::BadHeaderSize(8)),
        (|b| b.memory_region_count = 0, BootInfoError::BadRegionPointer),
        (|b| b.memory_region_count = MAX_MEMORY_REGIONS + 1, BootInfoError::TooManyRegions(MAX_MEMORY_REGIONS + 1)),
        (|b| b.memory_regions = 0x1000, BootInfoError::BadRegionPointer),
        (|b| b.memory_regions += 1, BootInfoError::BadRegionPointer),
        (|b| b.cmdline_len = MAX_CMDLINE_LEN + 1, BootInfoError::BadCmdline),
        (|b| b.stack_bottom = b.stack_top, BootInfoError::BadStack),
        (|b| b.direct_map_len = 0, BootInfoError::BadDirectMap),
        (|b| b.framebuffer.stride = 10, BootInfoError::BadFramebuffer),
        (|b| b.framebuffer.format = 7, BootInfoError::BadFramebuffer),
    ];
    for (mutate, expected) in cases {
        let mut info = good_info();
        mutate(&mut info);
        assert_eq!(info.validate_header(), Err(expected));
    }
}

#[test]
fn framebuffer_size_overflow_rejected() {
    let mut info = good_info();
    info.framebuffer.stride = u32::MAX;
    info.framebuffer.height = u32::MAX;
    assert_eq!(info.validate_header(), Err(BootInfoError::BadFramebuffer));
    let mut info = good_info();
    info.framebuffer.byte_len -= 1;
    assert_eq!(info.validate_header(), Err(BootInfoError::BadFramebuffer));
    let mut info = good_info();
    info.framebuffer.virt_base += 4096;
    assert_eq!(info.validate_header(), Err(BootInfoError::BadFramebuffer));
}

#[test]
fn init_image_validation() {
    let mut info = good_info();
    info.init_image_len = 0;
    info.init_image = 0;
    assert_eq!(info.validate_header(), Ok(()));
    let mut info = good_info();
    info.init_image = 0x1000;
    assert_eq!(info.validate_header(), Err(BootInfoError::BadInitImage));
    let mut info = good_info();
    info.init_image_len = MAX_INIT_IMAGE + 1;
    assert_eq!(info.validate_header(), Err(BootInfoError::BadInitImage));
    let mut info = good_info();
    info.init_image = u64::MAX - 10;
    assert_eq!(info.validate_header(), Err(BootInfoError::BadInitImage));
}

#[test]
fn no_framebuffer_is_valid() {
    let mut info = good_info();
    info.framebuffer = FramebufferInfo::NONE;
    assert_eq!(info.validate_header(), Ok(()));
}

#[test]
fn in_direct_map_overflow_safe() {
    let info = good_info();
    assert!(!info.in_direct_map(u64::MAX, 2));
    assert!(!info.in_direct_map(DIRECT_MAP_BASE - 1, 1));
    assert!(info.in_direct_map(DIRECT_MAP_BASE, 0x1_0000_0000));
    assert!(!info.in_direct_map(DIRECT_MAP_BASE, 0x1_0000_0001));
}

#[test]
fn cmdline_validation() {
    assert_eq!(BootInfo::validate_cmdline(b"test=all qemu-exit"), Ok("test=all qemu-exit"));
    assert_eq!(BootInfo::validate_cmdline(b"a\nb"), Err(BootInfoError::CmdlineNotAscii));
    assert_eq!(BootInfo::validate_cmdline(&[0xC3, 0xA9]), Err(BootInfoError::CmdlineNotAscii));
    assert_eq!(BootInfo::validate_cmdline(&[b'a'; 2000]), Err(BootInfoError::BadCmdline));
    assert_eq!(BootInfo::validate_cmdline(b""), Ok(""));
}

#[test]
fn cmdline_parsing() {
    let c = Cmdline::new("  test=breakpoint qemu-exit  log=off quiet=no test=pagefault ");
    assert_eq!(c.get("test"), Some("pagefault"));
    assert!(c.flag("qemu-exit"));
    assert!(!c.flag("quiet"));
    assert!(!c.flag("missing"));
    assert_eq!(c.get("qemu-exit"), None);
    assert_eq!(c.options().count(), 5);
    assert_eq!(Cmdline::new("").options().count(), 0);
}

#[test]
fn direct_map_conversions() {
    assert_eq!(phys_to_virt(0x1000), Some(DIRECT_MAP_BASE + 0x1000));
    assert_eq!(phys_to_virt(DIRECT_MAP_MAX_LEN), None);
    assert_eq!(virt_to_phys(DIRECT_MAP_BASE + 5), Some(5));
    assert_eq!(virt_to_phys(5), None);
    assert_eq!(virt_to_phys(u64::MAX), None);
    for p in [0u64, 4096, 1 << 40, DIRECT_MAP_MAX_LEN - 1] {
        assert_eq!(virt_to_phys(phys_to_virt(p).unwrap()), Some(p));
    }
}

#[test]
fn layout_is_consistent() {
    const _: () = assert!(BOOT_STACK_GUARD < BOOT_STACK_BOTTOM);
    const _: () = assert!(DIRECT_MAP_BASE + DIRECT_MAP_MAX_LEN <= KERNEL_STACK_REGION);
    const _: () = assert!(BOOT_STACK_TOP < KERNEL_IMAGE_BASE);
    const _: () = assert!(BOOT_STACK_BOTTOM - BOOT_STACK_GUARD == PAGE_SIZE);
    const _: () = assert!(BOOT_STACK_TOP.is_multiple_of(16));
}

#[test]
fn abi_layout_is_stable() {
    // Changing these sizes requires bumping BOOT_INFO_VERSION.
    assert_eq!(core::mem::size_of::<MemoryRegion>(), 24);
    assert_eq!(core::mem::size_of::<FramebufferInfo>(), 40);
    assert_eq!(core::mem::size_of::<BootInfo>(), 168);
}

#[test]
fn uefi_type_mapping() {
    assert_eq!(kind_from_uefi(7), MemoryKind::Usable);
    assert_eq!(kind_from_uefi(3), MemoryKind::Usable);
    assert_eq!(kind_from_uefi(4), MemoryKind::Usable);
    assert_eq!(kind_from_uefi(1), MemoryKind::BootloaderReclaimable);
    assert_eq!(kind_from_uefi(5), MemoryKind::FirmwareRuntime);
    assert_eq!(kind_from_uefi(10), MemoryKind::AcpiNvs);
    assert_eq!(kind_from_uefi(uefi_types::PAGE_TABLES), MemoryKind::PageTables);
    assert_eq!(kind_from_uefi(0x7000_0000), MemoryKind::Reserved);
    assert_eq!(kind_from_uefi(0xFFFF_FFFF), MemoryKind::Reserved);
    for raw in 0..20u32 {
        let k = MemoryKind::from_raw(raw);
        if let Some(k) = k {
            assert_eq!(k as u32, raw);
            assert!(!k.name().is_empty());
        }
    }
}

fn r(base: u64, pages: u64, kind: MemoryKind) -> MemoryRegion {
    MemoryRegion::new(base, pages, kind)
}

#[test]
fn normalize_sorts_and_merges() {
    let mut v = [
        r(0x5000, 1, MemoryKind::Usable),
        r(0x1000, 2, MemoryKind::Usable),
        r(0x3000, 2, MemoryKind::Usable),
        r(0x10000, 1, MemoryKind::Reserved),
        r(0x6000, 1, MemoryKind::KernelImage),
    ];
    let n = normalize(&mut v).unwrap();
    assert_eq!(
        &v[..n],
        &[r(0x1000, 5, MemoryKind::Usable), r(0x6000, 1, MemoryKind::KernelImage), r(0x10000, 1, MemoryKind::Reserved)]
    );
}

#[test]
fn normalize_rejects_bad_entries() {
    let mut overlap = [r(0x1000, 2, MemoryKind::Usable), r(0x2000, 1, MemoryKind::Reserved)];
    assert_eq!(normalize(&mut overlap), Err(RegionError::Overlap(1)));
    let mut unaligned = [r(0x1001, 1, MemoryKind::Usable)];
    assert_eq!(normalize(&mut unaligned), Err(RegionError::Misaligned(0)));
    let mut empty = [r(0x1000, 0, MemoryKind::Usable)];
    assert_eq!(normalize(&mut empty), Err(RegionError::Empty(0)));
    let mut huge = [r(0x1000, u64::MAX / 4096, MemoryKind::Usable)];
    assert_eq!(normalize(&mut huge), Err(RegionError::OutOfRange(0)));
    let mut overflow = [r(0x1000, u64::MAX, MemoryKind::Usable)];
    assert_eq!(normalize(&mut overflow), Err(RegionError::OutOfRange(0)));
    let mut unknown = [MemoryRegion { base: 0, pages: 1, kind: 77, reserved: 0 }];
    assert_eq!(normalize(&mut unknown), Err(RegionError::UnknownKind(0)));
    let mut resv = [MemoryRegion { base: 0, pages: 1, kind: 1, reserved: 1 }];
    assert_eq!(normalize(&mut resv), Err(RegionError::ReservedBits(0)));
    let mut none: [MemoryRegion; 0] = [];
    assert_eq!(normalize(&mut none), Ok(0));
}

/// Property: for random disjoint region sets in random order, normalize
/// produces a sorted, non-overlapping list that covers exactly the same
/// pages with the same kinds.
#[test]
fn normalize_property_preserves_coverage() {
    let kinds = [MemoryKind::Usable, MemoryKind::Reserved, MemoryKind::Mmio, MemoryKind::KernelImage];
    let mut rng = Rng(0x00C0_C0C0_5EED_0001);
    for _ in 0..if cfg!(miri) { 40 } else { 2000 } {
        let n = rng.below(40) as usize;
        let mut v = Vec::new();
        let mut cursor = 0u64;
        for _ in 0..n {
            cursor += rng.below(3) * PAGE_SIZE; // optional gap
            let pages = 1 + rng.below(8);
            v.push(r(cursor, pages, kinds[rng.below(kinds.len() as u64) as usize]));
            cursor += pages * PAGE_SIZE;
        }
        // Expected per-page kind map.
        let total = (cursor / PAGE_SIZE) as usize;
        let mut expect = std::vec![0u32; total];
        for reg in &v {
            for p in 0..reg.pages {
                expect[(reg.base / PAGE_SIZE + p) as usize] = reg.kind;
            }
        }
        // Shuffle.
        for i in (1..v.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
        let count = normalize(&mut v).unwrap();
        let out = &v[..count];
        validate_sorted(out).unwrap();
        let mut got = std::vec![0u32; total];
        for reg in out {
            for p in 0..reg.pages {
                got[(reg.base / PAGE_SIZE + p) as usize] = reg.kind;
            }
        }
        assert_eq!(got, expect);
        // Maximal merging: no two adjacent entries are contiguous with equal kind.
        for w in out.windows(2) {
            assert!(!(w[0].end().unwrap() == w[1].base && w[0].kind == w[1].kind));
        }
    }
}

/// Property: random garbage never panics and is either accepted as a valid
/// sorted list or rejected.
#[test]
fn normalize_property_garbage_never_panics() {
    let mut rng = Rng(0xDEAD_BEEF_0BAD_F00D);
    for _ in 0..if cfg!(miri) { 200 } else { 5000 } {
        let n = rng.below(16) as usize;
        let mut v: Vec<MemoryRegion> = (0..n)
            .map(|_| MemoryRegion {
                base: if rng.below(2) == 0 { rng.next() } else { rng.below(64) * PAGE_SIZE },
                pages: if rng.below(4) == 0 { rng.next() } else { rng.below(5) },
                kind: rng.below(16) as u32,
                reserved: (rng.below(8) == 0) as u32,
            })
            .collect();
        if let Ok(count) = normalize(&mut v) {
            validate_sorted(&v[..count]).unwrap();
        }
    }
}
