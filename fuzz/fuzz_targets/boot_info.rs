//! Fuzzes boot-info header and command-line validation.
#![no_main]
use coco_abi::cmdline::Cmdline;
use coco_abi::{BootInfo, FramebufferInfo};
use libfuzzer_sys::fuzz_target;

fn word(d: &[u8], i: usize) -> u64 {
    d.get(i * 8..i * 8 + 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
}

fuzz_target!(|data: &[u8]| {
    let info = BootInfo {
        magic: coco_abi::BOOT_INFO_MAGIC,
        version: coco_abi::BOOT_INFO_VERSION,
        header_size: core::mem::size_of::<BootInfo>() as u32,
        memory_regions: word(data, 0),
        memory_region_count: word(data, 1),
        framebuffer: FramebufferInfo {
            phys_base: word(data, 2),
            virt_base: word(data, 3),
            byte_len: word(data, 4),
            width: word(data, 5) as u32,
            height: (word(data, 5) >> 32) as u32,
            stride: word(data, 6) as u32,
            format: (word(data, 6) >> 32) as u32,
        },
        cmdline: word(data, 7),
        cmdline_len: word(data, 8),
        acpi_rsdp: 0,
        stack_bottom: word(data, 9),
        stack_top: word(data, 10),
        direct_map_base: coco_abi::layout::DIRECT_MAP_BASE,
        direct_map_len: word(data, 11),
        tsc_loader_start: 0,
        tsc_handoff: 0,
        flags: 0,
        init_image: word(data, 12),
        init_image_len: word(data, 13),
    };
    if info.validate_header().is_ok() {
        // Accepted pointers must lie inside the direct map.
        let bytes = info.memory_region_count * 24;
        assert!(info.in_direct_map(info.memory_regions, bytes));
        assert!(info.cmdline_len == 0 || info.in_direct_map(info.cmdline, info.cmdline_len));
        assert!(info.init_image_len <= coco_abi::MAX_INIT_IMAGE);
        assert!(info.init_image_len == 0 || info.in_direct_map(info.init_image, info.init_image_len));
    }
    let tail = data.get(112..).unwrap_or(&[]);
    if let Ok(text) = BootInfo::validate_cmdline(tail) {
        let c = Cmdline::new(text);
        let _ = c.get("test");
        let _ = c.flag("qemu-exit");
        assert!(c.options().count() <= text.len());
    }
});
