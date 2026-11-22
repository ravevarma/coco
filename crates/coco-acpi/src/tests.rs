#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::vec::Vec;

use super::*;

/// Sparse fake physical memory made of byte regions.
#[derive(Default, Clone)]
struct Mem(Vec<(u64, Vec<u8>)>);

impl PhysMemory for Mem {
    fn read(&mut self, phys: u64, out: &mut [u8]) -> Result<(), AcpiError> {
        for (base, data) in &self.0 {
            if phys >= *base && phys + out.len() as u64 <= base + data.len() as u64 {
                let o = (phys - base) as usize;
                out.copy_from_slice(&data[o..o + out.len()]);
                return Ok(());
            }
        }
        Err(AcpiError::Unreadable(phys))
    }
}

fn fix_checksum(t: &mut [u8], at: usize, len: usize) {
    t[at] = 0;
    let s = t[..len].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    t[at] = 0u8.wrapping_sub(s);
}

fn sdt(sig: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut t = std::vec![0u8; 36];
    t[0..4].copy_from_slice(sig);
    t[8] = 1;
    t[10..16].copy_from_slice(b"COCOTS");
    t.extend_from_slice(body);
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    let l = t.len();
    fix_checksum(&mut t, 9, l);
    t
}

fn rsdp_v2(xsdt: u64) -> Vec<u8> {
    let mut r = std::vec![0u8; 36];
    r[0..8].copy_from_slice(b"RSD PTR ");
    r[9..15].copy_from_slice(b"COCOOS");
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    r[24..32].copy_from_slice(&xsdt.to_le_bytes());
    fix_checksum(&mut r, 8, 20);
    fix_checksum(&mut r, 32, 36);
    r
}

fn madt_body(cpus: &[(u8, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes()); // PCAT_COMPAT
    for &(id, flags) in cpus {
        b.extend_from_slice(&[0, 8, id, id]);
        b.extend_from_slice(&flags.to_le_bytes());
    }
    b.extend_from_slice(&[1, 12, 0, 0]);
    b.extend_from_slice(&0xFEC0_0000u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&[2, 10, 0, 0]);
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&[9, 16, 0, 0]);
    b.extend_from_slice(&300u32.to_le_bytes()); // x2APIC id
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&77u32.to_le_bytes());
    b
}

fn fadt_body(dsdt: u32) -> Vec<u8> {
    // Full ACPI 6 FADT is 276 bytes; body = 240.
    let mut b = std::vec![0u8; 240];
    let put = |b: &mut Vec<u8>, off: usize, v: &[u8]| b[off - 36..off - 36 + v.len()].copy_from_slice(v);
    put(&mut b, 40, &dsdt.to_le_bytes());
    put(&mut b, 48, &0xB2u32.to_le_bytes());
    put(&mut b, 52, &[0xF1]);
    put(&mut b, 64, &0x604u32.to_le_bytes());
    put(&mut b, 76, &0x608u32.to_le_bytes());
    put(&mut b, 112, &((1u32 << 8) | (1 << 10)).to_le_bytes());
    put(&mut b, 116, &[SPACE_IO, 8, 0, 1]);
    put(&mut b, 120, &0xCF9u64.to_le_bytes());
    put(&mut b, 128, &[0x0F]);
    b
}

fn dsdt_with(aml: &[u8]) -> Vec<u8> {
    let mut body = std::vec![0x10u8; 300]; // filler that is not NameOp
    body.extend_from_slice(aml);
    body.extend_from_slice(&[0x5B; 50]);
    sdt(b"DSDT", &body)
}

const S5_AML: &[u8] = &[0x08, b'_', b'S', b'5', b'_', 0x12, 0x08, 0x04, 0x0A, 0x05, 0x0A, 0x05, 0x00, 0x00];

fn platform(s5: &[u8]) -> Mem {
    let dsdt = dsdt_with(s5);
    let madt = sdt(b"APIC", &madt_body(&[(0, 1), (1, 1), (2, 0), (3, 2)]));
    let fadt = sdt(b"FACP", &fadt_body(0x30_0000));
    let mut hpet_body = std::vec![0u8; 20];
    hpet_body[4..8].copy_from_slice(&[SPACE_MEMORY, 64, 0, 0]);
    hpet_body[8..16].copy_from_slice(&0xFED0_0000u64.to_le_bytes());
    let hpet = sdt(b"HPET", &hpet_body);
    let mut xsdt_body = Vec::new();
    for a in [0x10_0000u64, 0x20_0000, 0x40_0000] {
        xsdt_body.extend_from_slice(&a.to_le_bytes());
    }
    let xsdt = sdt(b"XSDT", &xsdt_body);
    Mem(std::vec![
        (0xE0000, rsdp_v2(0x8000)),
        (0x8000, xsdt),
        (0x10_0000, madt),
        (0x20_0000, fadt),
        (0x30_0000, dsdt),
        (0x40_0000, hpet),
    ])
}

#[test]
fn parses_complete_platform() {
    let mut m = platform(S5_AML);
    let info = parse(&mut m, 0xE0000).unwrap();
    assert_eq!(info.revision, 2);
    assert_eq!(&info.oem_id, b"COCOOS");
    assert_eq!((info.table_count, info.rejected_tables), (3, 0));
    let madt = info.madt.unwrap();
    assert_eq!(madt.lapic_address, 0xFEE0_0000);
    assert!(madt.pcat_compat);
    assert_eq!(madt.cpus().len(), 5);
    assert_eq!(madt.enabled_cpus(), 3);
    assert_eq!(madt.cpus()[3], Cpu { apic_id: 3, uid: 3, enabled: false, online_capable: true });
    assert_eq!(madt.cpus()[4], Cpu { apic_id: 300, uid: 77, enabled: true, online_capable: false });
    assert_eq!(madt.ioapics(), &[IoApic { id: 0, address: 0xFEC0_0000, gsi_base: 0 }]);
    assert_eq!(madt.overrides(), &[Override { source: 0, gsi: 2, flags: 0 }]);
    let fadt = info.fadt.unwrap();
    assert_eq!(fadt.pm_timer_port, Some(0x608));
    assert!(fadt.pm_timer_32bit);
    assert_eq!(fadt.pm1a_control, Some(0x604));
    assert_eq!((fadt.smi_command, fadt.acpi_enable), (Some(0xB2), 0xF1));
    assert_eq!(fadt.reset, Some((GenericAddress { space: SPACE_IO, bit_width: 8, address: 0xCF9 }, 0x0F)));
    assert_eq!(info.hpet, Some(0xFED0_0000));
    assert_eq!(info.s5, Some((5, 5)));
}

#[test]
fn s5_encodings() {
    type Case<'a> = (&'a [u8], Option<(u8, u8)>);
    let cases: &[Case<'_>] = &[
        (&[0x08, b'\\', b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x00, 0x00, 0x00, 0x00], Some((0, 0))),
        (&[0x08, b'_', b'S', b'5', b'_', 0x12, 0x0A, 0x04, 0x01, 0x0A, 0x07, 0x00, 0x00], Some((1, 7))),
        // Two-byte PkgLength (lead byte 0x40 | low nibble).
        (&[0x08, b'_', b'S', b'5', b'_', 0x12, 0x41, 0x00, 0x02, 0x0A, 0x03, 0x0A, 0x04], Some((3, 4))),
        // Not preceded by NameOp: ignored.
        (&[0x77, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x01, 0x01], None),
        // Package with one element: unusable.
        (&[0x08, b'_', b'S', b'5', b'_', 0x12, 0x03, 0x01, 0x01], None),
    ];
    for (aml, want) in cases {
        let mut m = platform(aml);
        assert_eq!(parse(&mut m, 0xE0000).unwrap().s5, *want, "aml {aml:02x?}");
    }
}

#[test]
fn s5_found_across_chunk_boundary() {
    // Place the pattern at every offset around the 512-byte chunk edge.
    for pad in 160..200 {
        let mut aml = std::vec![0x10u8; pad];
        aml.extend_from_slice(S5_AML);
        let mut m = platform(&aml);
        assert_eq!(parse(&mut m, 0xE0000).unwrap().s5, Some((5, 5)), "pad {pad}");
    }
}

#[test]
fn rejects_bad_rsdp() {
    let mut m = platform(S5_AML);
    assert_eq!(parse(&mut m, 0).unwrap_err(), AcpiError::NoRsdp);
    assert_eq!(parse(&mut m, 0x1234_0000).unwrap_err(), AcpiError::Unreadable(0x1234_0000));
    m.0[0].1[3] = b'X';
    assert_eq!(parse(&mut m, 0xE0000).unwrap_err(), AcpiError::BadSignature(0xE0000));
    let mut m = platform(S5_AML);
    m.0[0].1[12] ^= 1;
    assert_eq!(parse(&mut m, 0xE0000).unwrap_err(), AcpiError::BadChecksum(0xE0000));
}

#[test]
fn bad_table_is_rejected_not_trusted() {
    let mut m = platform(S5_AML);
    m.0[2].1[50] ^= 0xFF; // corrupt MADT body -> checksum fails
    let info = parse(&mut m, 0xE0000).unwrap();
    assert!(info.madt.is_none());
    assert_eq!(info.rejected_tables, 1);
    assert!(info.fadt.is_some());
}

#[test]
fn madt_entry_overrunning_table_rejected() {
    let mut body = madt_body(&[(0, 1)]);
    body[8 + 1] = 200; // first entry claims 200 bytes
    let mut m = platform(S5_AML);
    m.0[2].1 = sdt(b"APIC", &body);
    let info = parse(&mut m, 0xE0000).unwrap();
    assert!(info.madt.is_none());
    assert_eq!(info.rejected_tables, 1);
}

#[test]
fn madt_zero_length_entry_rejected() {
    let mut body = madt_body(&[(0, 1)]);
    body[8 + 1] = 0;
    let mut m = platform(S5_AML);
    m.0[2].1 = sdt(b"APIC", &body);
    assert!(parse(&mut m, 0xE0000).unwrap().madt.is_none());
}

#[test]
fn many_cpus_are_bounded() {
    let cpus: Vec<(u8, u32)> = (0..=255u8).chain(0..50).map(|i| (i, 1)).collect();
    let mut m = platform(S5_AML);
    m.0[2].1 = sdt(b"APIC", &madt_body(&cpus));
    let madt = parse(&mut m, 0xE0000).unwrap().madt.unwrap();
    assert_eq!(madt.cpus().len(), MAX_CPUS);
    assert!(madt.dropped_entries >= 50);
}

#[test]
fn acpi1_rsdt_path() {
    let mut m = platform(S5_AML);
    let mut rsdt_body = Vec::new();
    for a in [0x10_0000u32, 0x20_0000] {
        rsdt_body.extend_from_slice(&a.to_le_bytes());
    }
    m.0.push((0x9000, sdt(b"RSDT", &rsdt_body)));
    let mut r = std::vec![0u8; 20];
    r[0..8].copy_from_slice(b"RSD PTR ");
    r[15] = 0;
    r[16..20].copy_from_slice(&0x9000u32.to_le_bytes());
    fix_checksum(&mut r, 8, 20);
    m.0[0].1 = r;
    let info = parse(&mut m, 0xE0000).unwrap();
    assert_eq!((info.revision, info.table_count), (0, 2));
    assert_eq!(info.madt.unwrap().enabled_cpus(), 3);
    assert_eq!(info.hpet, None);
}

#[test]
fn oversized_and_undersized_lengths() {
    let mut m = platform(S5_AML);
    m.0[1].1[4..8].copy_from_slice(&(MAX_TABLE_LEN + 1).to_le_bytes());
    assert_eq!(parse(&mut m, 0xE0000).unwrap_err(), AcpiError::BadLength(0x8000));
    let mut m = platform(S5_AML);
    m.0[1].1[4..8].copy_from_slice(&10u32.to_le_bytes());
    assert_eq!(parse(&mut m, 0xE0000).unwrap_err(), AcpiError::BadLength(0x8000));
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

/// Random corruption of every table: never panics; any MADT that is
/// accepted stays within its fixed bounds.
#[test]
fn corruption_never_panics() {
    let base = platform(S5_AML);
    let mut rng = Rng(0xAC01);
    for _ in 0..if cfg!(miri) { 60 } else { 20_000 } {
        let mut m = base.clone();
        for _ in 0..1 + rng.next() % 6 {
            let t = (rng.next() % m.0.len() as u64) as usize;
            let len = m.0[t].1.len();
            let i = (rng.next() % len as u64) as usize;
            m.0[t].1[i] = rng.next() as u8;
            if rng.next().is_multiple_of(2) {
                // Re-fix checksums so corruption reaches the parsers.
                if t == 0 {
                    fix_checksum(&mut m.0[0].1, 8, 20);
                    fix_checksum(&mut m.0[0].1, 32, 36);
                } else {
                    let l = m.0[t].1.len();
                    fix_checksum(&mut m.0[t].1, 9, l);
                }
            }
        }
        if let Ok(info) = parse(&mut m, 0xE0000) {
            if let Some(madt) = info.madt {
                assert!(madt.cpus().len() <= MAX_CPUS && madt.ioapics().len() <= MAX_IOAPICS);
            }
            if let Some((a, b)) = info.s5 {
                assert!(a < 8 && b < 8);
            }
        }
    }
}
