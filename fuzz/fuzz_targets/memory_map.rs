//! Fuzzes memory-map normalization with arbitrary firmware-like entries.
#![no_main]
use coco_abi::MemoryRegion;
use coco_abi::memory::{normalize, validate_sorted};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut regions: Vec<MemoryRegion> = data
        .chunks_exact(24)
        .take(256)
        .map(|c| MemoryRegion {
            base: u64::from_le_bytes(c[0..8].try_into().unwrap()),
            pages: u64::from_le_bytes(c[8..16].try_into().unwrap()),
            kind: u32::from_le_bytes(c[16..20].try_into().unwrap()),
            reserved: u32::from_le_bytes(c[20..24].try_into().unwrap()),
        })
        .collect();
    let total_before: u128 = regions.iter().map(|r| u128::from(r.pages)).sum();
    if let Ok(n) = normalize(&mut regions) {
        let out = &regions[..n];
        validate_sorted(out).unwrap();
        // Merging never creates or loses pages.
        let total_after: u128 = out.iter().map(|r| u128::from(r.pages)).sum();
        assert_eq!(total_before, total_after);
    }
});
