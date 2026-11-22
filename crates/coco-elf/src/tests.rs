#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::vec::Vec;

use super::*;

const WIN: Policy =
    Policy { window_start: 0xFFFF_FFFF_8000_0000, window_end: 0xFFFF_FFFF_FFFF_F000, max_total_bytes: 16 << 20 };
const BASE: u64 = 0xFFFF_FFFF_8000_0000;

#[derive(Clone, Copy)]
struct Ph {
    p_type: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

fn load(flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64) -> Ph {
    Ph { p_type: 1, flags, offset, vaddr, filesz, memsz, align: 4096 }
}

/// Builds an ELF64 image with the given program headers and file length.
fn build(entry: u64, phs: &[Ph], file_len: usize) -> Vec<u8> {
    let mut d = std::vec![0u8; file_len.max(64 + 56 * phs.len())];
    d[0..4].copy_from_slice(b"\x7fELF");
    d[4..8].copy_from_slice(&[2, 1, 1, 0]);
    d[16..18].copy_from_slice(&2u16.to_le_bytes());
    d[18..20].copy_from_slice(&62u16.to_le_bytes());
    d[20..24].copy_from_slice(&1u32.to_le_bytes());
    d[24..32].copy_from_slice(&entry.to_le_bytes());
    d[32..40].copy_from_slice(&64u64.to_le_bytes());
    d[52..54].copy_from_slice(&64u16.to_le_bytes());
    d[54..56].copy_from_slice(&56u16.to_le_bytes());
    d[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
    for (i, p) in phs.iter().enumerate() {
        let b = 64 + i * 56;
        d[b..b + 4].copy_from_slice(&p.p_type.to_le_bytes());
        d[b + 4..b + 8].copy_from_slice(&p.flags.to_le_bytes());
        d[b + 8..b + 16].copy_from_slice(&p.offset.to_le_bytes());
        d[b + 16..b + 24].copy_from_slice(&p.vaddr.to_le_bytes());
        d[b + 24..b + 32].copy_from_slice(&p.vaddr.to_le_bytes());
        d[b + 32..b + 40].copy_from_slice(&p.filesz.to_le_bytes());
        d[b + 40..b + 48].copy_from_slice(&p.memsz.to_le_bytes());
        d[b + 48..b + 56].copy_from_slice(&p.align.to_le_bytes());
    }
    d
}

fn kernel_like() -> Vec<u8> {
    build(
        BASE + 0x10,
        &[
            load(PF_R | PF_X, 0x1000, BASE, 0x800, 0x800),
            load(PF_R, 0x2000, BASE + 0x1000, 0x100, 0x100),
            load(PF_R | PF_W, 0x3000, BASE + 0x2000, 0x10, 0x5000),
        ],
        0x3010,
    )
}

#[test]
fn accepts_valid_kernel() {
    let img = kernel_like();
    let exe = Executable::parse(&img, &WIN).unwrap();
    assert_eq!(exe.entry(), BASE + 0x10);
    assert_eq!(exe.segments().len(), 3);
    assert_eq!(exe.total_pages(), 1 + 1 + 5);
    let bss = exe.segments()[2];
    assert_eq!(exe.file_data(&bss).len(), 0x10);
    assert!(bss.writable() && !bss.executable());
}

#[test]
fn segment_page_math() {
    let s = Segment { vaddr: BASE + 0xFF0, mem_size: 0x20, file_offset: 0xFF0, file_size: 0x20, flags: PF_R };
    assert_eq!(s.page_start(), BASE);
    assert_eq!(s.page_offset(), 0xFF0);
    assert_eq!(s.page_count(), 2);
}

#[test]
fn rejects_header_corruption() {
    let good = kernel_like();
    let cases: &[(usize, u8, ElfError)] = &[
        (0, 0, ElfError::BadMagic),
        (4, 1, ElfError::BadIdent),       // 32-bit class
        (5, 2, ElfError::BadIdent),       // big endian
        (7, 3, ElfError::BadIdent),       // Linux OSABI
        (16, 3, ElfError::NotExecutable), // ET_DYN
        (18, 3, ElfError::WrongMachine),
        (20, 2, ElfError::BadIdent),
        (54, 32, ElfError::BadHeaderSize),
        (52, 32, ElfError::BadHeaderSize),
    ];
    for &(off, val, err) in cases {
        let mut img = good.clone();
        img[off] = val;
        assert_eq!(Executable::parse(&img, &WIN).unwrap_err(), err, "offset {off}");
    }
}

#[test]
fn rejects_truncation_at_every_length() {
    let good = kernel_like();
    for len in (0..good.len()).step_by(if cfg!(miri) { 97 } else { 1 }) {
        assert!(Executable::parse(&good[..len], &WIN).is_err(), "len {len}");
    }
}

#[test]
fn rejects_policy_violations() {
    let p = |phs: &[Ph], entry: u64| Executable::parse(&build(entry, phs, 0x4000), &WIN).unwrap_err();
    assert_eq!(p(&[load(PF_R | PF_W | PF_X, 0x1000, BASE, 0x10, 0x10)], BASE), ElfError::WriteExecute);
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, 0x40_0000, 0x10, 0x10)], 0x40_0000), ElfError::OutsideWindow);
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, BASE, 0x20, 0x10)], BASE), ElfError::BadSegmentSize);
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, BASE, 0, 0)], BASE), ElfError::BadSegmentSize);
    assert_eq!(p(&[load(PF_R | PF_X, 0x3F00, BASE + 0xF00, 0x200, 0x200)], BASE + 0xF00), ElfError::OutOfBounds);
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, BASE, 0x10, 0x10)], BASE + 0x10), ElfError::BadEntry);
    assert_eq!(p(&[load(PF_R, 0x1000, BASE, 0x10, 0x10)], BASE), ElfError::BadEntry);
    assert_eq!(
        p(&[load(PF_R | PF_X, 0x1000, BASE, 0x10, 0x10), load(PF_R | PF_W, 0x1800, BASE + 0x800, 0x10, 0x10)], BASE),
        ElfError::Overlap
    );
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, BASE, 0x10, 32 << 20)], BASE), ElfError::TooLarge);
    // vaddr + memsz wraps the address space (vaddr congruent with offset).
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, 0xFFFF_FFFF_FFFF_F000, 0x10, 0x2000)], BASE), ElfError::OutsideWindow);
    // Ends exactly past the window end.
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, 0xFFFF_FFFF_FFFF_E000, 0x10, 0x1001)], BASE), ElfError::OutsideWindow);
    // Incongruent vaddr/offset is rejected before anything else.
    assert_eq!(p(&[load(PF_R | PF_X, 0x1000, u64::MAX - 0x10, 0x10, 0x100)], BASE), ElfError::BadAlignment);
    assert_eq!(p(&[load(PF_R | PF_X, u64::MAX, BASE, 0x10, 0x10)], BASE), ElfError::OutOfBounds);
    let mut bad_align = load(PF_R | PF_X, 0x1001, BASE, 0x10, 0x10);
    assert_eq!(p(&[bad_align], BASE), ElfError::BadAlignment);
    bad_align.offset = 0x1000;
    bad_align.align = 3000;
    assert_eq!(p(&[bad_align], BASE), ElfError::BadAlignment);
    for t in [2u32, 3, 7] {
        let ph = Ph { p_type: t, flags: 0, offset: 0, vaddr: 0, filesz: 0, memsz: 0, align: 0 };
        assert_eq!(p(&[ph, load(PF_R | PF_X, 0x1000, BASE, 0x10, 0x10)], BASE), ElfError::Unsupported);
    }
    let note = Ph { p_type: 4, flags: 0, offset: 0, vaddr: 0, filesz: 0, memsz: 0, align: 0 };
    assert_eq!(p(&[note], BASE), ElfError::NoSegments);
}

#[test]
fn rejects_bad_phdr_tables() {
    let mut img = kernel_like();
    img[56..58].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(Executable::parse(&img, &WIN).unwrap_err(), ElfError::BadPhdrCount);
    img[56..58].copy_from_slice(&(MAX_PHDRS + 1).to_le_bytes());
    assert_eq!(Executable::parse(&img, &WIN).unwrap_err(), ElfError::BadPhdrCount);
    let mut img = kernel_like();
    img[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(Executable::parse(&img, &WIN).unwrap_err(), ElfError::OutOfBounds);
    let mut img = kernel_like();
    let near_end = img.len() as u64 - 10;
    img[32..40].copy_from_slice(&near_end.to_le_bytes());
    assert_eq!(Executable::parse(&img, &WIN).unwrap_err(), ElfError::OutOfBounds);
}

#[test]
fn rejects_too_many_segments() {
    let phs: Vec<Ph> =
        (0..(MAX_SEGMENTS as u64 + 1)).map(|i| load(PF_R | PF_X, 0x1000, BASE + i * 0x1000, 0x10, 0x10)).collect();
    let img = build(BASE, &phs, 0x2000);
    assert_eq!(Executable::parse(&img, &WIN).unwrap_err(), ElfError::TooManySegments);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Mutation fuzz: corrupt random bytes of a valid image. The parser must not
/// panic and every accepted image must satisfy the documented invariants.
#[test]
fn mutation_fuzz_invariants() {
    let good = kernel_like();
    let mut rng = Rng(0x0C0C_0E1F_0000_0001);
    let mut accepted = 0u32;
    for _ in 0..if cfg!(miri) { 400 } else { 200_000 } {
        let mut img = good.clone();
        let flips = 1 + rng.next() % 4;
        for _ in 0..flips {
            // Bias mutations towards the headers, where all the logic lives.
            let idx = (rng.next() % 300) as usize;
            img[idx] = rng.next() as u8;
        }
        if let Ok(exe) = Executable::parse(&img, &WIN) {
            accepted += 1;
            check_invariants(&exe, img.len());
        }
    }
    assert!(accepted > 0, "fuzzer never produced an accepted variant");
}

fn check_invariants(exe: &Executable<'_>, len: usize) {
    let segs = exe.segments();
    assert!(!segs.is_empty() && segs.len() <= MAX_SEGMENTS);
    for (i, s) in segs.iter().enumerate() {
        assert!(!(s.writable() && s.executable()));
        assert!(s.file_size <= s.mem_size && s.mem_size > 0);
        assert!(s.file_offset + s.file_size <= len as u64);
        assert!(s.vaddr >= WIN.window_start && s.vaddr + s.mem_size <= WIN.window_end);
        assert_eq!(exe.file_data(s).len() as u64, s.file_size);
        for t in &segs[i + 1..] {
            let a_end = s.page_start() + s.page_count() * 4096;
            let b_end = t.page_start() + t.page_count() * 4096;
            assert!(a_end <= t.page_start() || b_end <= s.page_start());
        }
    }
    assert!(exe.total_pages() * 4096 <= WIN.max_total_bytes);
    assert!(segs.iter().any(|s| s.executable() && exe.entry() >= s.vaddr && exe.entry() < s.vaddr + s.mem_size));
}
