#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::vec::Vec;

use super::*;

/// Minimal DTB writer for tests.
#[derive(Default)]
struct Writer {
    structure: Vec<u8>,
    strings: Vec<u8>,
    rsv: Vec<(u64, u64)>,
}

impl Writer {
    fn u32(&mut self, v: u32) {
        self.structure.extend_from_slice(&v.to_be_bytes());
    }
    fn pad(&mut self) {
        while self.structure.len() % 4 != 0 {
            self.structure.push(0);
        }
    }
    fn begin(&mut self, name: &str) {
        self.u32(FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }
    fn end(&mut self) {
        self.u32(FDT_END_NODE);
    }
    fn prop(&mut self, name: &str, value: &[u8]) {
        let off = match self
            .strings
            .windows(name.len() + 1)
            .position(|w| &w[..name.len()] == name.as_bytes() && w[name.len()] == 0)
        {
            Some(o) => o,
            None => {
                let o = self.strings.len();
                self.strings.extend_from_slice(name.as_bytes());
                self.strings.push(0);
                o
            }
        };
        self.u32(FDT_PROP);
        self.u32(value.len() as u32);
        self.u32(off as u32);
        self.structure.extend_from_slice(value);
        self.pad();
    }
    fn finish(mut self) -> Vec<u8> {
        self.u32(FDT_END);
        let rsv_off = 40usize;
        let rsv_len = (self.rsv.len() + 1) * 16;
        let struct_off = rsv_off + rsv_len;
        let strings_off = struct_off + self.structure.len();
        let total = strings_off + self.strings.len();
        let mut out = Vec::new();
        for v in [
            MAGIC,
            total as u32,
            struct_off as u32,
            strings_off as u32,
            rsv_off as u32,
            17,
            16,
            0,
            self.strings.len() as u32,
            self.structure.len() as u32,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        for (b, s) in &self.rsv {
            out.extend_from_slice(&b.to_be_bytes());
            out.extend_from_slice(&s.to_be_bytes());
        }
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&self.structure);
        out.extend_from_slice(&self.strings);
        out
    }
}

fn cells2(vals: &[u64]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// A tree shaped like QEMU riscv `virt` + OpenSBI.
fn qemu_like() -> Vec<u8> {
    let mut w = Writer::default();
    w.rsv.push((0x8700_0000, 0x1000));
    w.begin("");
    w.prop("#address-cells", &2u32.to_be_bytes());
    w.prop("#size-cells", &2u32.to_be_bytes());
    w.prop("compatible", b"riscv-virtio\0");
    w.begin("memory@80000000");
    w.prop("device_type", b"memory\0");
    w.prop("reg", &cells2(&[0x8000_0000, 0x800_0000]));
    w.end();
    w.begin("reserved-memory");
    w.prop("#address-cells", &2u32.to_be_bytes());
    w.prop("#size-cells", &2u32.to_be_bytes());
    w.begin("mmode_resv0@80000000");
    w.prop("reg", &cells2(&[0x8000_0000, 0x4_0000]));
    w.end();
    w.end();
    w.begin("soc");
    w.prop("#address-cells", &2u32.to_be_bytes());
    w.prop("#size-cells", &2u32.to_be_bytes());
    w.begin("serial@10000000");
    w.prop("compatible", b"ns16550a\0");
    w.prop("reg", &cells2(&[0x1000_0000, 0x100]));
    w.end();
    w.end();
    w.end();
    w.finish()
}

#[test]
fn parses_qemu_like_tree() {
    let blob = qemu_like();
    let s = parse(&blob, b"ns16550a").unwrap();
    assert_eq!(s.memory.as_slice(), &[Range { base: 0x8000_0000, size: 0x800_0000 }]);
    assert_eq!(
        s.reserved.as_slice(),
        &[Range { base: 0x8700_0000, size: 0x1000 }, Range { base: 0x8000_0000, size: 0x4_0000 }]
    );
    assert_eq!(s.device, Some(0x1000_0000));
    assert_eq!(s.total_size as usize, blob.len());
    assert_eq!(parse(&blob, b"arm,pl011").unwrap().device, None);
}

#[test]
fn one_cell_addresses_and_multiple_banks() {
    let mut w = Writer::default();
    w.begin("");
    w.prop("#address-cells", &1u32.to_be_bytes());
    w.prop("#size-cells", &1u32.to_be_bytes());
    w.begin("memory");
    w.prop("device_type", b"memory\0");
    let reg: Vec<u8> = [0x0u32, 0x1000_0000, 0x4000_0000, 0x2000_0000].iter().flat_map(|v| v.to_be_bytes()).collect();
    w.prop("reg", &reg);
    w.end();
    w.end();
    let s = parse(&w.finish(), b"").unwrap();
    assert_eq!(
        s.memory.as_slice(),
        &[Range { base: 0, size: 0x1000_0000 }, Range { base: 0x4000_0000, size: 0x2000_0000 }]
    );
}

#[test]
fn rejects_malformed_headers() {
    let good = qemu_like();
    let mut b = good.clone();
    b[0] = 0;
    assert_eq!(parse(&b, b"").unwrap_err(), FdtError::BadMagic);
    let mut b = good.clone();
    b[23] = 3; // version 3
    assert_eq!(parse(&b, b"").unwrap_err(), FdtError::BadVersion);
    let mut b = good.clone();
    b[4..8].copy_from_slice(&(good.len() as u32 + 1).to_be_bytes());
    assert_eq!(parse(&b, b"").unwrap_err(), FdtError::Truncated);
    let mut b = good.clone();
    b[36..40].copy_from_slice(&u32::MAX.to_be_bytes()); // struct size
    assert_eq!(parse(&b, b"").unwrap_err(), FdtError::BadLayout);
}

#[test]
fn every_truncation_is_rejected() {
    let good = qemu_like();
    for len in 0..good.len() {
        assert!(parse(&good[..len], b"ns16550a").is_err(), "len {len}");
    }
}

#[test]
fn unbalanced_and_too_deep_trees_rejected() {
    let mut w = Writer::default();
    w.begin("");
    w.begin("a");
    w.end();
    // root never closed
    assert!(parse(&w.finish(), b"").is_err());
    let mut w = Writer::default();
    for i in 0..=MAX_DEPTH {
        w.begin(if i == 0 { "" } else { "n" });
    }
    for _ in 0..=MAX_DEPTH {
        w.end();
    }
    assert_eq!(parse(&w.finish(), b"").unwrap_err(), FdtError::BadStructure);
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

/// Random corruption never panics; accepted results stay bounded.
#[test]
fn mutation_never_panics() {
    let good = qemu_like();
    let mut rng = Rng(0xFD7);
    for _ in 0..if cfg!(miri) { 300 } else { 50_000 } {
        let mut b = good.clone();
        for _ in 0..1 + rng.next() % 4 {
            let i = (rng.next() % b.len() as u64) as usize;
            b[i] = rng.next() as u8;
        }
        if let Ok(s) = parse(&b, b"ns16550a") {
            assert!(s.memory.as_slice().len() <= MAX_RANGES && s.reserved.as_slice().len() <= MAX_RANGES);
        }
    }
}
