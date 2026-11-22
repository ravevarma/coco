//! The coco kernel.
//!
//! Entered from `coco-boot` with the boot-info pointer in `rdi`, interrupts
//! disabled, on the guard-paged boot stack, in the loader-built address
//! space (kernel image, direct map, temporary identity alias).
#![no_std]
#![no_main]

mod console;
mod cpu;
mod fb;
mod font;
mod mm;
mod selftest;

use core::sync::atomic::{AtomicBool, Ordering};

use coco_abi::cmdline::Cmdline;
use coco_abi::layout::DIRECT_MAP_BASE;
use coco_abi::{BootInfo, BootInfoError, MemoryKind, MemoryRegion, flags};
use coco_arch::cpu as arch;

/// Whether `qemu-exit` was requested (isa-debug-exit device present).
static QEMU_EXIT: AtomicBool = AtomicBool::new(false);
const QEMU_EXIT_PORT: u16 = 0xF4;
/// QEMU exits with status `(code << 1) | 1`: 0x10 -> 33, 0x11 -> 35.
const QEMU_SUCCESS: u32 = 0x10;
const QEMU_FAILURE: u32 = 0x11;
const BOOT_STAGES: usize = 5;

/// Validated, kernel-owned view of the boot information.
struct BootView {
    info: BootInfo,
    regions: &'static [MemoryRegion],
    cmdline: &'static str,
}

fn validate_boot_info(ptr: u64) -> Result<BootView, BootInfoError> {
    if ptr < DIRECT_MAP_BASE || !ptr.is_multiple_of(core::mem::align_of::<BootInfo>() as u64) {
        return Err(BootInfoError::BadRegionPointer);
    }
    // SAFETY: loader contract — `ptr` addresses a BootInfo in BOOT_INFO
    // memory inside the direct map (alignment checked above). It is copied
    // out by value; every field is then validated before use.
    let info = unsafe { core::ptr::read(ptr as *const BootInfo) };
    info.validate_header()?;
    // SAFETY: `validate_header` checked that the region array is non-empty,
    // aligned and inside the direct map; it lives in BOOT_INFO memory that
    // the kernel never frees or reuses ('static) and nothing mutates it.
    let regions = unsafe {
        core::slice::from_raw_parts(info.memory_regions as *const MemoryRegion, info.memory_region_count as usize)
    };
    BootInfo::validate_regions(regions)?;
    let bytes: &'static [u8] = if info.cmdline_len == 0 {
        &[]
    } else {
        // SAFETY: as for `regions`: bounds checked by `validate_header`,
        // BOOT_INFO memory is immutable and never reclaimed.
        unsafe { core::slice::from_raw_parts(info.cmdline as *const u8, info.cmdline_len as usize) }
    };
    let cmdline = BootInfo::validate_cmdline(bytes)?;
    Ok(BootView { info, regions, cmdline })
}

/// Stops the machine after a fatal error.
pub fn fatal_stop() -> ! {
    if QEMU_EXIT.load(Ordering::Relaxed) {
        // SAFETY: `qemu-exit` asserts the isa-debug-exit device exists at
        // port 0xF4; writing it terminates the VM.
        unsafe { arch::outl(QEMU_EXIT_PORT, QEMU_FAILURE) };
    }
    arch::halt_forever()
}

fn success_stop() -> ! {
    if QEMU_EXIT.load(Ordering::Relaxed) {
        // SAFETY: as in `fatal_stop`.
        unsafe { arch::outl(QEMU_EXIT_PORT, QEMU_SUCCESS) };
    }
    arch::halt_forever()
}

/// Kernel entry point (System V ABI, called by the loader).
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "sysv64" fn _start(boot_info: u64) -> ! {
    kernel_main(boot_info)
}

fn kernel_main(boot_info: u64) -> ! {
    let t_entry = arch::rdtsc();
    let serial = console::init_serial();
    kprintln!("coco kernel {} (x86_64) - serial {}", env!("CARGO_PKG_VERSION"), if serial { "ok" } else { "absent" });

    let boot = match validate_boot_info(boot_info) {
        Ok(b) => b,
        Err(e) => {
            kprintln!("FATAL: invalid boot information: {:?}", e);
            fatal_stop();
        }
    };
    let cmd = Cmdline::new(boot.cmdline);
    QEMU_EXIT.store(cmd.flag("qemu-exit"), Ordering::Relaxed);
    kprintln!(
        "boot: info v{} ok, {} memory regions, cmdline \"{}\"",
        boot.info.version,
        boot.regions.len(),
        boot.cmdline
    );

    if let Err(e) = cpu::init() {
        kprintln!("FATAL: cpu init: {:?}", e);
        fatal_stop();
    }
    kprintln!("cpu: GDT, TSS (IST stacks for #DF/NMI/#MC) and IDT loaded");
    mm::drop_identity_map(&boot.info);
    kprintln!("mm: loader identity map removed");
    let t_cpu = arch::rdtsc();

    // SAFETY: framebuffer description validated by `validate_header`; this
    // is the only Framebuffer handle ever created.
    if let Some(fb) = unsafe { fb::Framebuffer::new(&boot.info.framebuffer) } {
        let (w, h) = (fb.width(), fb.height());
        console::attach_framebuffer(fb);
        kprintln!("framebuffer: {}x{} ready", w, h);
    } else {
        kprintln!("framebuffer: none (serial-only)");
    }
    console::progress(2, BOOT_STAGES);
    let t_fb = arch::rdtsc();

    let mut mem = match mm::init(&boot.info, boot.regions) {
        Ok(m) => m,
        Err(e) => {
            kprintln!("FATAL: memory init: {:?}", e);
            fatal_stop();
        }
    };
    let t_mm = arch::rdtsc();
    console::progress(3, BOOT_STAGES);
    report_memory(&mem, &boot);

    let nx = boot.info.flags & flags::NX_ENABLED != 0;
    kprintln!(
        "security: NX {}, kernel W^X enforced, CR0.WP {}, stack guard page at {:#x}",
        if nx { "on" } else { "UNAVAILABLE" },
        if arch::read_cr0() & arch::CR0_WP != 0 { "on" } else { "off" },
        boot.info.stack_bottom - 4096
    );

    let mut failures = 0u32;
    match cmd.get("test") {
        None => {}
        Some("memory" | "all") => {
            let (pass, fail) = selftest::memory_suite(&mut mem, nx);
            let bp = selftest::breakpoint();
            kprintln!("selftest breakpoint: {}", if bp.is_ok() { "PASS" } else { "FAIL" });
            let fail = fail + u32::from(bp.is_err());
            kprintln!("selftest summary: {} passed, {} failed", pass + u32::from(bp.is_ok()), fail);
            failures += fail;
        }
        Some("breakpoint") => {
            let r = selftest::breakpoint();
            kprintln!("selftest breakpoint: {}", if r.is_ok() { "PASS" } else { "FAIL" });
            failures += u32::from(r.is_err());
        }
        Some(name) => {
            if let Err(e) = selftest::fatal_test(name) {
                kprintln!("fault-test {name}: FAIL: {e}");
                failures += 1;
            }
        }
    }
    console::progress(BOOT_STAGES, BOOT_STAGES);

    let t_done = arch::rdtsc();
    kprintln!(
        "timing (TSC cycles): loader {} | handoff->entry {} | cpu {} | fb {} | mm {} | total kernel {}",
        boot.info.tsc_handoff.wrapping_sub(boot.info.tsc_loader_start),
        t_entry.wrapping_sub(boot.info.tsc_handoff),
        t_cpu - t_entry,
        t_fb - t_cpu,
        t_mm - t_fb,
        t_done - t_entry
    );
    if failures > 0 {
        kprintln!("coco: boot complete with {} self-test FAILURE(S)", failures);
        fatal_stop();
    }
    kprintln!("coco: boot complete");
    success_stop()
}

fn report_memory(mem: &mm::KernelMemory, boot: &BootView) {
    let kib = |k: MemoryKind| mem.stats.bytes_by_kind.get(k as usize).copied().unwrap_or(0) >> 10;
    kprintln!(
        "memory: usable {} MiB + loader-reclaimed {} KiB; free frames {} ({} MiB)",
        kib(MemoryKind::Usable) >> 10,
        kib(MemoryKind::BootloaderReclaimable),
        mem.frames.free_frames(),
        mem.frames.free_frames() * 4 / 1024
    );
    kprintln!(
        "memory: kernel image {} KiB, boot stack {} KiB, page tables {} KiB, boot info {} KiB, frame bitmaps {} KiB",
        kib(MemoryKind::KernelImage),
        kib(MemoryKind::KernelStack),
        kib(MemoryKind::PageTables),
        kib(MemoryKind::BootInfo),
        mem.stats.bitmap_frames * 4
    );
    kprintln!(
        "memory: firmware runtime {} KiB, ACPI {} KiB, reserved/mmio {} KiB, direct map {} MiB",
        kib(MemoryKind::FirmwareRuntime),
        kib(MemoryKind::AcpiReclaimable) + kib(MemoryKind::AcpiNvs),
        kib(MemoryKind::Reserved) + kib(MemoryKind::Mmio),
        boot.info.direct_map_len >> 20
    );
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    arch::disable_interrupts();
    console::emergency_banner("coco: kernel panic - system halted");
    kprintln_emergency!("KERNEL PANIC: {}", info);
    fatal_stop()
}
