//! AArch64 backend: EL1 on QEMU `virt` (GICv2, PL011 UART, ARM generic
//! timer, PSCI over HVC for power-off). The device tree comes from `x0`
//! (Linux boot protocol) or, for a bare ELF loaded by QEMU, from the start
//! of RAM where QEMU places it.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use coco_fdt::Range;

use crate::{BREAKPOINTS, Platform, TICKS, println};

pub const NAME: &str = "aarch64 (EL1)";

const UART_DEFAULT: usize = 0x0900_0000;
const GICD: usize = 0x0800_0000;
const GICC: usize = 0x0801_0000;
/// Non-secure EL1 physical timer PPI (INTID 30).
const TIMER_INTID: u32 = 30;
const RAM_START: usize = 0x4000_0000;
static UART: AtomicUsize = AtomicUsize::new(UART_DEFAULT);
static INTERVAL: AtomicU64 = AtomicU64::new(0);

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    // Park every core but core 0 (affinity level 0 of MPIDR).
    "    mrs x9, mpidr_el1",
    "    and x9, x9, #0xff",
    "    cbnz x9, 9f",
    "    mov x19, x0",
    "    ldr x9, =__stack_top",
    "    mov sp, x9",
    "    ldr x9, =__bss_start",
    "    ldr x10, =__bss_end",
    "1:  cmp x9, x10",
    "    b.hs 2f",
    "    str xzr, [x9], #8",
    "    b 1b",
    "2:  ldr x9, =coco_port_vectors",
    "    msr vbar_el1, x9",
    "    isb",
    "    mov x0, x19",
    "    bl coco_port_aarch64_main",
    "9:  wfe",
    "    b 9b",
    "",
    // Exception vectors: 16 entries of 128 bytes, table 2 KiB aligned. Every
    // entry saves the full register state and passes its index.
    ".section .text",
    ".macro COCO_VECTOR idx",
    ".balign 128",
    "    sub sp, sp, #272",
    "    stp x0, x1, [sp, #0]",
    "    mov x0, #\\idx",
    "    b coco_port_vector_common",
    ".endm",
    ".balign 2048",
    ".global coco_port_vectors",
    "coco_port_vectors:",
    "    COCO_VECTOR 0",
    "    COCO_VECTOR 1",
    "    COCO_VECTOR 2",
    "    COCO_VECTOR 3",
    "    COCO_VECTOR 4",
    "    COCO_VECTOR 5",
    "    COCO_VECTOR 6",
    "    COCO_VECTOR 7",
    "    COCO_VECTOR 8",
    "    COCO_VECTOR 9",
    "    COCO_VECTOR 10",
    "    COCO_VECTOR 11",
    "    COCO_VECTOR 12",
    "    COCO_VECTOR 13",
    "    COCO_VECTOR 14",
    "    COCO_VECTOR 15",
    "coco_port_vector_common:",
    "    stp x2, x3, [sp, #16]",
    "    stp x4, x5, [sp, #32]",
    "    stp x6, x7, [sp, #48]",
    "    stp x8, x9, [sp, #64]",
    "    stp x10, x11, [sp, #80]",
    "    stp x12, x13, [sp, #96]",
    "    stp x14, x15, [sp, #112]",
    "    stp x16, x17, [sp, #128]",
    "    stp x18, x19, [sp, #144]",
    "    stp x20, x21, [sp, #160]",
    "    stp x22, x23, [sp, #176]",
    "    stp x24, x25, [sp, #192]",
    "    stp x26, x27, [sp, #208]",
    "    stp x28, x29, [sp, #224]",
    "    mrs x9, elr_el1",
    "    mrs x10, spsr_el1",
    "    stp x30, x9, [sp, #240]",
    "    str x10, [sp, #256]",
    // x0 = vector index; the original x0 sits in the frame. Frame pointer
    // to Rust in x1.
    "    mov x1, sp",
    "    bl coco_port_aarch64_trap",
    "    ldp x30, x9, [sp, #240]",
    "    ldr x10, [sp, #256]",
    "    msr elr_el1, x9",
    "    msr spsr_el1, x10",
    "    ldp x0, x1, [sp, #0]",
    "    ldp x2, x3, [sp, #16]",
    "    ldp x4, x5, [sp, #32]",
    "    ldp x6, x7, [sp, #48]",
    "    ldp x8, x9, [sp, #64]",
    "    ldp x10, x11, [sp, #80]",
    "    ldp x12, x13, [sp, #96]",
    "    ldp x14, x15, [sp, #112]",
    "    ldp x16, x17, [sp, #128]",
    "    ldp x18, x19, [sp, #144]",
    "    ldp x20, x21, [sp, #160]",
    "    ldp x22, x23, [sp, #176]",
    "    ldp x24, x25, [sp, #192]",
    "    ldp x26, x27, [sp, #208]",
    "    ldp x28, x29, [sp, #224]",
    "    add sp, sp, #272",
    "    eret",
);

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Saved register frame (layout fixed by the vector assembly).
#[repr(C)]
struct Frame {
    x: [u64; 31],
    elr: u64,
    spsr: u64,
    _pad: u64,
}

fn mmio_write(addr: usize, v: u32) {
    // SAFETY: callers pass GIC/UART register addresses of QEMU virt, which
    // are device memory (MMU off: all accesses are Device-nGnRnE).
    unsafe { (addr as *mut u32).write_volatile(v) };
}

fn mmio_read(addr: usize) -> u32 {
    // SAFETY: as `mmio_write`.
    unsafe { (addr as *const u32).read_volatile() }
}

pub fn putc(b: u8) {
    let base = UART.load(Ordering::Relaxed);
    for _ in 0..100_000 {
        if mmio_read(base + 0x18) & (1 << 5) == 0 {
            break; // FR.TXFF clear: room in the FIFO
        }
    }
    mmio_write(base, u32::from(b));
}

pub fn counter() -> u64 {
    let v: u64;
    // SAFETY: CNTVCT_EL0 is readable at EL1; the isb orders the read.
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

pub fn counter_hz() -> Option<u64> {
    let f: u64;
    // SAFETY: CNTFRQ_EL0 is readable at EL1 (set by firmware/QEMU).
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
    (f != 0).then_some(f)
}

fn arm_timer() {
    let interval = INTERVAL.load(Ordering::Relaxed);
    // SAFETY: programs the EL1 physical timer: tval = interval, enabled and
    // unmasked. Only affects timer interrupt delivery.
    unsafe {
        asm!("msr cntp_tval_el0, {}", "mov {t}, #1", "msr cntp_ctl_el0, {t}", "isb", in(reg) interval, t = out(reg) _, options(nostack))
    };
}

pub fn start_timer(hz: u32) {
    let freq = counter_hz().unwrap_or(62_500_000);
    INTERVAL.store(freq / u64::from(hz.max(1)), Ordering::Relaxed);
    // GICv2 distributor: enable the timer PPI with a high priority.
    mmio_write(GICD, 1); // GICD_CTLR.EnableGrp0/1
    let prio = GICD + 0x400 + TIMER_INTID as usize; // GICD_IPRIORITYR (byte-accessible)
    // SAFETY: GICD_IPRIORITYR is byte-accessible MMIO.
    unsafe { (prio as *mut u8).write_volatile(0x80) };
    mmio_write(GICD + 0x100, 1 << TIMER_INTID); // GICD_ISENABLER0
    mmio_write(GICC + 0x04, 0xFF); // GICC_PMR: allow all priorities
    mmio_write(GICC, 1); // GICC_CTLR
    arm_timer();
    // SAFETY: unmasks IRQs at EL1; the vector table is installed.
    unsafe { asm!("msr daifclr, #2", options(nomem, nostack)) };
}

pub fn wait_for_interrupt() {
    // SAFETY: `wfi` only stalls until an interrupt is pending.
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

pub fn breakpoint() {
    // SAFETY: traps to our synchronous vector, which skips the instruction.
    unsafe { asm!("brk #0", options(nostack)) };
}

pub fn kernel_image() -> (u64, u64) {
    ((&raw const __kernel_start) as u64, (&raw const __kernel_end) as u64)
}

fn psci(function: u64) {
    // SAFETY: PSCI call to the hypervisor/firmware (QEMU implements PSCI
    // over HVC for virt at EL1); it does not return for SYSTEM_OFF/RESET.
    unsafe { asm!("hvc #0", inlateout("x0") function => _, options(nostack)) };
}

/// Powers off (success) or resets (failure; run QEMU with `-no-reboot` so
/// the reset terminates it). The test harness also checks console output.
pub fn exit(success: bool) -> ! {
    psci(if success { 0x8400_0008 } else { 0x8400_0009 });
    loop {
        wait_for_interrupt();
    }
}

#[unsafe(no_mangle)]
extern "C" fn coco_port_aarch64_trap(vector: u64, frame: &mut Frame) {
    // Vectors 4..=7: current EL with SP_ELx (sync, IRQ, FIQ, SError).
    match vector {
        4 => {
            let esr: u64;
            // SAFETY: reading ESR_EL1 at EL1 has no side effects.
            unsafe { asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack)) };
            if esr >> 26 == 0x3C {
                BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
                frame.elr += 4; // BRK is always 4 bytes
                return;
            }
            let far: u64;
            // SAFETY: as above, FAR_EL1.
            unsafe { asm!("mrs {}, far_el1", out(reg) far, options(nomem, nostack)) };
            println!("FATAL: synchronous exception ESR {:#x} at {:#x}, FAR {:#x}", esr, frame.elr, far);
        }
        5 => {
            let iar = mmio_read(GICC + 0x0C);
            if iar & 0x3FF == TIMER_INTID {
                TICKS.fetch_add(1, Ordering::Relaxed);
                arm_timer();
            }
            if iar & 0x3FF < 1020 {
                mmio_write(GICC + 0x10, iar); // EOI (1020..1023 are spurious)
            }
            return;
        }
        _ => println!("FATAL: unexpected exception vector {} at {:#x}", vector, frame.elr),
    }
    exit(false)
}

#[unsafe(no_mangle)]
extern "C" fn coco_port_aarch64_main(x0: usize) -> ! {
    let mut platform = Platform::new("device tree");
    // SAFETY: the address is x0 from the boot protocol or the start of RAM,
    // where QEMU places the DTB for bare ELF images; `dtb_slice` validates
    // the header before trusting the size.
    let found = [x0, RAM_START].into_iter().find_map(|a| unsafe { dtb_slice(a) }.map(|b| (a, b)));
    match found.map(|(a, b)| (a, coco_fdt::parse(b, b"arm,pl011"))) {
        Some((addr, Ok(s))) => {
            if let Some(uart) = s.device {
                UART.store(uart as usize, Ordering::Relaxed);
            }
            for r in s.memory.as_slice() {
                platform.add_ram(*r);
            }
            for r in s.reserved.as_slice() {
                platform.add_reserved(*r);
            }
            platform.add_reserved(Range { base: addr as u64, size: u64::from(s.total_size) });
        }
        other => {
            println!("FATAL: no valid device tree (x0 = {:#x}): {:?}", x0, other.map(|(_, r)| r.err()));
            exit(false);
        }
    }
    crate::kernel_main(&platform)
}

/// Returns the device-tree blob at `addr` with its declared size.
///
/// # Safety
/// `addr` must be a readable physical address (RAM).
unsafe fn dtb_slice(addr: usize) -> Option<&'static [u8]> {
    if addr < RAM_START || addr % 8 != 0 {
        return None;
    }
    // SAFETY: caller contract; magic and size are read before anything else.
    let magic = u32::from_be(unsafe { (addr as *const u32).read_volatile() });
    if magic != 0xD00D_FEED {
        return None;
    }
    // SAFETY: as above.
    let size = u32::from_be(unsafe { ((addr + 4) as *const u32).read_volatile() }) as usize;
    if !(40..=(2 << 20)).contains(&size) {
        return None;
    }
    // SAFETY: caller contract plus the bounded size; the blob is reserved
    // from the allocator and never written.
    Some(unsafe { core::slice::from_raw_parts(addr as *const u8, size) })
}
