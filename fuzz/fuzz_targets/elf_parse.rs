//! Fuzzes the kernel-image parser: arbitrary bytes must never panic, and any
//! accepted image must satisfy the loader's safety invariants.
#![no_main]
use coco_elf::{Executable, Policy};
use libfuzzer_sys::fuzz_target;

const POLICY: Policy =
    Policy { window_start: 0xFFFF_FFFF_8000_0000, window_end: 0xFFFF_FFFF_FFFF_F000, max_total_bytes: 64 << 20 };

fuzz_target!(|data: &[u8]| {
    if let Ok(exe) = Executable::parse(data, &POLICY) {
        let segs = exe.segments();
        assert!(!segs.is_empty());
        for (i, s) in segs.iter().enumerate() {
            assert!(!(s.writable() && s.executable()), "W^X violated");
            assert!(s.file_size <= s.mem_size);
            assert!(s.file_offset.checked_add(s.file_size).is_some_and(|e| e <= data.len() as u64));
            assert!(s.vaddr >= POLICY.window_start);
            assert!(s.vaddr.checked_add(s.mem_size).is_some_and(|e| e <= POLICY.window_end));
            assert_eq!(exe.file_data(s).len() as u64, s.file_size);
            for t in &segs[i + 1..] {
                let a_end = s.page_start() + s.page_count() * 4096;
                let b_end = t.page_start() + t.page_count() * 4096;
                assert!(a_end <= t.page_start() || b_end <= s.page_start(), "segments share a page");
            }
        }
        assert!(exe.total_pages() * 4096 <= POLICY.max_total_bytes);
    }
});
