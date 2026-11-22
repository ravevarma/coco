//! Fuzzes the flattened device-tree parser used by the RISC-V and AArch64
//! ports. Parsing must never panic or read outside the blob, results must
//! respect the fixed bounds, and a parsed blob's declared size must fit.
#![no_main]
use coco_fdt::MAX_RANGES;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for compatible in [&b"ns16550a"[..], b"arm,pl011", b""] {
        if let Ok(s) = coco_fdt::parse(data, compatible) {
            assert!(s.total_size as usize <= data.len());
            assert!(s.memory.as_slice().len() <= MAX_RANGES);
            assert!(s.reserved.as_slice().len() <= MAX_RANGES);
            for r in s.memory.as_slice().iter().chain(s.reserved.as_slice()) {
                assert!(r.size > 0, "empty ranges must be dropped");
            }
        }
    }
});
