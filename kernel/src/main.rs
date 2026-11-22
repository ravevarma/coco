//! The coco kernel.
//!
//! Entered from `coco-boot` with the boot-info pointer in `rdi`, interrupts
//! disabled, on the guard-paged boot stack, in the loader-built address
//! space (kernel image, direct map, temporary identity alias).
#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod console;
mod cpu;
mod fb;
mod font;
mod heap;
mod interrupts;
mod mm;
mod power;
mod selftest;
mod sync;

use alloc::boxed::Box;

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
const BOOT_STAGES: usize = 6;

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

    let hardening = match cpu::init() {
        Ok(h) => h,
        Err(e) => {
            kprintln!("FATAL: cpu init: {:?}", e);
            fatal_stop();
        }
    };
    kprintln!("cpu: GDT, TSS (IST stacks for #DF/NMI/#MC), IDT (256 vectors) loaded");
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
    let ist_guards = cpu::arm_ist_guards(&mut mem);
    mm::install(mem);
    heap::init();
    kprintln!("heap: ready, window {:#x} + 1 GiB, grown on demand, poisoning on", heap::HEAP_BASE);

    let acpi: Option<&'static coco_acpi::AcpiInfo> = match acpi::init(boot.info.acpi_rsdp) {
        Ok(info) => {
            let info: &'static coco_acpi::AcpiInfo = Box::leak(Box::new(info));
            report_acpi(info);
            Some(info)
        }
        Err(e) => {
            kprintln!("acpi: unavailable ({:?}); continuing without ACPI", e);
            None
        }
    };
    let timer = match interrupts::init(acpi, cmd.flag("x2apic")) {
        Ok(t) => t,
        Err(e) => {
            kprintln!("FATAL: interrupt controller / timer init: {:?}", e);
            fatal_stop();
        }
    };
    // SAFETY: the IDT covers all 256 vectors, the PIC is masked, the local
    // APIC and timer are configured, and no lock is held here.
    unsafe { arch::enable_interrupts() };
    let t_irq = arch::rdtsc();
    kprintln!(
        "timer: {} Hz tick, TSC {} MHz, APIC timer {} kHz (calibrated against {}), {} mode; interrupts on",
        interrupts::HZ,
        timer.tsc_hz / 1_000_000,
        timer.apic_timer_hz / 1000,
        timer.reference,
        if timer.x2apic { "x2APIC" } else { "xAPIC" }
    );
    if cmd.flag("x2apic") && !timer.x2apic {
        kprintln!("apic: x2APIC requested but the CPU does not report it; staying in xAPIC mode");
    }
    console::progress(4, BOOT_STAGES);

    let nx = boot.info.flags & flags::NX_ENABLED != 0;
    let on = |b: bool| if b { "on" } else { "off" };
    kprintln!(
        "security: NX {}, W^X, CR0.WP {}, SMEP {}, SMAP {}, UMIP {}, stack guard {:#x}, IST guards {}",
        if nx { "on" } else { "UNAVAILABLE" },
        on(arch::read_cr0() & arch::CR0_WP != 0),
        on(hardening.smep),
        on(hardening.smap),
        on(hardening.umip),
        boot.info.stack_bottom - 4096,
        if ist_guards { "on" } else { "FAILED" }
    );

    let mut failures = 0u32;
    let mut record = |name: &str, r: selftest::TestResult| match r {
        Ok(()) => kprintln!("selftest {name}: PASS"),
        Err(e) => {
            kprintln!("selftest {name}: FAIL: {e}");
            failures += 1;
        }
    };
    match cmd.get("test") {
        None => {}
        Some("memory" | "all") => {
            let (pass, fail) = mm::with(|m| selftest::memory_suite(m, nx, &hardening)).unwrap_or((0, 1));
            record("breakpoint", selftest::breakpoint());
            record("heap", selftest::heap_suite());
            record("timer", selftest::timer_accuracy());
            kprintln!("selftest memory suite: {} passed, {} failed", pass, fail);
            if fail > 0 {
                record("memory-suite", Err("failures above"));
            }
        }
        Some("breakpoint") => record("breakpoint", selftest::breakpoint()),
        Some("heap") => {
            record("heap", selftest::heap_suite());
            record("heap-exhaustion", selftest::heap_exhaustion());
        }
        Some("timer") => record("timer", selftest::timer_accuracy()),
        Some("irq-alloc") => record("irq-alloc", selftest::irq_alloc()),
        Some("lifecycle") => {
            let cycles = cmd.get("cycles").and_then(|c| c.parse::<u8>().ok()).unwrap_or(3);
            let (pass, fail) = mm::with(|m| selftest::memory_suite(m, nx, &hardening)).unwrap_or((0, 1));
            kprintln!("selftest memory suite: {} passed, {} failed", pass, fail);
            record("lifecycle", selftest::lifecycle(acpi, cycles, fail));
        }
        Some("shutdown") => record("shutdown", power::shutdown(acpi)),
        Some(name) => record(name, selftest::fatal_test(name)),
    }
    console::progress(BOOT_STAGES, BOOT_STAGES);

    let t_done = arch::rdtsc();
    kprintln!(
        "timing (TSC cycles): loader {} | handoff->entry {} | cpu {} | fb {} | mm {} | heap+acpi+timer {} | total kernel {}",
        boot.info.tsc_handoff.wrapping_sub(boot.info.tsc_loader_start),
        t_entry.wrapping_sub(boot.info.tsc_handoff),
        t_cpu - t_entry,
        t_fb - t_cpu,
        t_mm - t_fb,
        t_irq - t_mm,
        t_done - t_entry
    );
    if let Some(h) = heap::stats() {
        kprintln!(
            "heap: in use {} B, peak {} B, mapped {} KiB, {} allocs / {} frees",
            h.in_use,
            h.peak,
            h.mapped / 1024,
            h.allocations,
            h.frees
        );
    }
    if failures > 0 {
        kprintln!("coco: boot complete with {} self-test FAILURE(S)", failures);
        fatal_stop();
    }
    kprintln!("coco: boot complete");
    if QEMU_EXIT.load(Ordering::Relaxed) {
        success_stop();
    }
    idle()
}

/// Idles with interrupts enabled, halting between timer ticks.
fn idle() -> ! {
    loop {
        // SAFETY: interrupts are fully configured and no lock is held.
        unsafe { arch::enable_interrupts_and_halt() };
    }
}

fn report_acpi(a: &coco_acpi::AcpiInfo) {
    let oem = core::str::from_utf8(&a.oem_id).unwrap_or("?").trim_end();
    kprintln!(
        "acpi: revision {}, OEM \"{}\", {} tables ({} rejected)",
        a.revision,
        oem,
        a.table_count,
        a.rejected_tables
    );
    if let Some(m) = a.madt {
        kprintln!(
            "acpi: MADT {} CPUs ({} enabled), {} I/O APIC(s), {} overrides, LAPIC {:#x}",
            m.cpus().len(),
            m.enabled_cpus(),
            m.ioapics().len(),
            m.overrides().len(),
            m.lapic_address
        );
    }
    if let Some(f) = a.fadt {
        kprintln!(
            "acpi: FADT PM timer {:?} ({}-bit), PM1a {:?}, reset {}, S5 {:?}, HPET {:?}",
            f.pm_timer_port,
            if f.pm_timer_32bit { 32 } else { 24 },
            f.pm1a_control,
            if f.reset.is_some() { "yes" } else { "no" },
            a.s5,
            a.hpet
        );
    }
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
