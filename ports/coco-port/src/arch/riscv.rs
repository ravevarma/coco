//! RISC-V backend (RV64 in S-mode under OpenSBI; RV32 in M-mode without
//! firmware, because no RV32 SBI firmware is available in the build
//! environment). Platform: QEMU `virt` — ns16550a UART, CLINT/SBI timer at
//! 10 MHz, `sifive,test` exit device, device tree passed in `a1`.

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use coco_fdt::Range;

use crate::{BREAKPOINTS, Platform, TICKS, println};

#[cfg(target_arch = "riscv64")]
pub const NAME: &str = "riscv64 (S-mode, OpenSBI)";
#[cfg(target_arch = "riscv32")]
pub const NAME: &str = "riscv32 (M-mode)";

const UART_DEFAULT: usize = 0x1000_0000;
const TEST_DEVICE: usize = 0x10_0000;
const TIMEBASE_HZ: u64 = 10_000_000;
static UART: AtomicUsize = AtomicUsize::new(UART_DEFAULT);
static INTERVAL: AtomicUsize = AtomicUsize::new(0);

/// Emits the entry and trap-vector assembly for one register width and
/// privilege mode.
macro_rules! riscv_asm {
    ($store:literal, $load:literal, $xlen:literal, $mode:literal, $ret:literal) => {
        global_asm!(concat!(
            ".section .text.entry\n",
            ".global _start\n",
            "_start:\n",
            "    la sp, __stack_top\n",
            "    la t0, __bss_start\n",
            "    la t1, __bss_end\n",
            "1:  bgeu t0, t1, 2f\n",
            "    ",
            $store,
            " zero, 0(t0)\n",
            "    addi t0, t0, ",
            $xlen,
            "\n",
            "    j 1b\n",
            "2:  call coco_port_riscv_main\n",
            "3:  wfi\n",
            "    j 3b\n",
            ".section .text\n",
            ".balign 4\n",
            ".global coco_port_trap_entry\n",
            "coco_port_trap_entry:\n",
            "    addi sp, sp, -32*",
            $xlen,
            "\n",
            "    ",
            $store,
            " x1, 1*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x3, 3*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x4, 4*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x5, 5*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x6, 6*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x7, 7*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x8, 8*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x9, 9*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x10, 10*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x11, 11*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x12, 12*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x13, 13*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x14, 14*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x15, 15*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x16, 16*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x17, 17*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x18, 18*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x19, 19*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x20, 20*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x21, 21*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x22, 22*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x23, 23*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x24, 24*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x25, 25*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x26, 26*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x27, 27*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x28, 28*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x29, 29*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x30, 30*",
            $xlen,
            "(sp)\n",
            "    ",
            $store,
            " x31, 31*",
            $xlen,
            "(sp)\n",
            "    csrr t0, ",
            $mode,
            "epc\n",
            "    ",
            $store,
            " t0, 0(sp)\n",
            "    addi t1, sp, 32*",
            $xlen,
            "\n",
            "    ",
            $store,
            " t1, 2*",
            $xlen,
            "(sp)\n",
            "    mv a0, sp\n",
            "    call coco_port_riscv_trap\n",
            "    ",
            $load,
            " t0, 0(sp)\n",
            "    csrw ",
            $mode,
            "epc, t0\n",
            "    ",
            $load,
            " x1, 1*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x3, 3*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x4, 4*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x5, 5*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x6, 6*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x7, 7*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x8, 8*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x9, 9*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x10, 10*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x11, 11*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x12, 12*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x13, 13*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x14, 14*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x15, 15*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x16, 16*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x17, 17*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x18, 18*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x19, 19*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x20, 20*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x21, 21*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x22, 22*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x23, 23*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x24, 24*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x25, 25*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x26, 26*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x27, 27*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x28, 28*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x29, 29*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x30, 30*",
            $xlen,
            "(sp)\n",
            "    ",
            $load,
            " x31, 31*",
            $xlen,
            "(sp)\n",
            "    addi sp, sp, 32*",
            $xlen,
            "\n",
            "    ",
            $ret,
            "\n",
        ));
    };
}

#[cfg(target_arch = "riscv64")]
riscv_asm!("sd", "ld", "8", "s", "sret");
#[cfg(target_arch = "riscv32")]
riscv_asm!("sw", "lw", "4", "m", "mret");

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static coco_port_trap_entry: u8;
}

// CSR accessors for the privilege mode in use.
#[cfg(target_arch = "riscv64")]
macro_rules! csr {
    (read $name:literal) => {{
        let v: usize;
        // SAFETY: reading a supervisor CSR in S-mode has no side effects.
        unsafe { core::arch::asm!(concat!("csrr {}, s", $name), out(reg) v, options(nomem, nostack)) };
        v
    }};
    (set $name:literal, $bits:expr) => {{
        // SAFETY: setting interrupt-enable bits only affects interrupt
        // delivery; the trap vector is installed before they are set.
        unsafe { core::arch::asm!(concat!("csrs s", $name, ", {}"), in(reg) $bits, options(nostack)) };
    }};
    (write $name:literal, $v:expr) => {{
        // SAFETY: as above (only stvec is written, with a valid vector).
        unsafe { core::arch::asm!(concat!("csrw s", $name, ", {}"), in(reg) $v, options(nostack)) };
    }};
}
#[cfg(target_arch = "riscv32")]
macro_rules! csr {
    (read $name:literal) => {{
        let v: usize;
        // SAFETY: reading a machine CSR in M-mode has no side effects.
        unsafe { core::arch::asm!(concat!("csrr {}, m", $name), out(reg) v, options(nomem, nostack)) };
        v
    }};
    (set $name:literal, $bits:expr) => {{
        // SAFETY: see the RV64 variant.
        unsafe { core::arch::asm!(concat!("csrs m", $name, ", {}"), in(reg) $bits, options(nostack)) };
    }};
    (write $name:literal, $v:expr) => {{
        // SAFETY: see the RV64 variant.
        unsafe { core::arch::asm!(concat!("csrw m", $name, ", {}"), in(reg) $v, options(nostack)) };
    }};
}

#[cfg(target_arch = "riscv64")]
const TIMER_IRQ: usize = 5; // supervisor timer
#[cfg(target_arch = "riscv32")]
const TIMER_IRQ: usize = 7; // machine timer
#[cfg(target_arch = "riscv64")]
const STATUS_IE: usize = 1 << 1; // sstatus.SIE
#[cfg(target_arch = "riscv32")]
const STATUS_IE: usize = 1 << 3; // mstatus.MIE

pub fn putc(b: u8) {
    let base = UART.load(Ordering::Relaxed);
    for _ in 0..100_000 {
        // SAFETY: `base` is the ns16550a register block (QEMU default or the
        // device-tree `reg`); LSR (offset 5) reads have no side effects.
        if unsafe { ((base + 5) as *const u8).read_volatile() } & 0x20 != 0 {
            break;
        }
    }
    // SAFETY: THR (offset 0) write transmits one byte.
    unsafe { (base as *mut u8).write_volatile(b) };
}

/// The CPU's 10 MHz time counter.
pub fn counter() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let t: u64;
        // SAFETY: `rdtime` is readable in S-mode (enabled by OpenSBI).
        unsafe { core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack)) };
        t
    }
    #[cfg(target_arch = "riscv32")]
    {
        const MTIME: usize = 0x0200_BFF8;
        loop {
            // SAFETY: CLINT mtime (M-mode MMIO, read-only use). Hi/lo/hi
            // re-read avoids a torn 64-bit value on RV32.
            let hi = unsafe { ((MTIME + 4) as *const u32).read_volatile() };
            // SAFETY: as above.
            let lo = unsafe { (MTIME as *const u32).read_volatile() };
            // SAFETY: as above.
            if unsafe { ((MTIME + 4) as *const u32).read_volatile() } == hi {
                return (u64::from(hi) << 32) | u64::from(lo);
            }
        }
    }
}

pub fn counter_hz() -> Option<u64> {
    Some(TIMEBASE_HZ)
}

fn arm_timer(at: u64) {
    #[cfg(target_arch = "riscv64")]
    {
        // SBI TIME extension (EID "TIME"), set_timer(stime_value).
        // SAFETY: an SBI call to firmware; it only programs the timer and
        // clobbers a0/a1.
        unsafe {
            core::arch::asm!("ecall", inlateout("a0") at as usize => _, lateout("a1") _, in("a6") 0usize, in("a7") 0x5449_4D45usize, options(nostack));
        }
    }
    #[cfg(target_arch = "riscv32")]
    {
        const MTIMECMP: usize = 0x0200_4000;
        // SAFETY: CLINT mtimecmp for hart 0. Writing hi = all-ones first
        // prevents a spurious interrupt while the halves are inconsistent.
        unsafe { ((MTIMECMP + 4) as *mut u32).write_volatile(u32::MAX) };
        // SAFETY: as above.
        unsafe { (MTIMECMP as *mut u32).write_volatile(at as u32) };
        // SAFETY: as above.
        unsafe { ((MTIMECMP + 4) as *mut u32).write_volatile((at >> 32) as u32) };
    }
}

pub fn start_timer(hz: u32) {
    let interval = TIMEBASE_HZ / u64::from(hz.max(1));
    INTERVAL.store(interval as usize, Ordering::Relaxed);
    arm_timer(counter() + interval);
    csr!(set "ie", 1usize << TIMER_IRQ);
    csr!(set "status", STATUS_IE);
}

pub fn wait_for_interrupt() {
    // SAFETY: `wfi` only stalls until an interrupt is pending.
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
}

pub fn breakpoint() {
    // SAFETY: traps to our vector, which advances the PC past it.
    unsafe { core::arch::asm!("ebreak", options(nostack)) };
}

pub fn kernel_image() -> (u64, u64) {
    ((&raw const __kernel_start) as usize as u64, (&raw const __kernel_end) as usize as u64)
}

/// Exits QEMU through the `sifive,test` device (0x5555 pass, 0x3333 fail).
pub fn exit(success: bool) -> ! {
    let code: u32 = if success { 0x5555 } else { (1 << 16) | 0x3333 };
    // SAFETY: QEMU virt's test device; writing it terminates the machine.
    unsafe { (TEST_DEVICE as *mut u32).write_volatile(code) };
    loop {
        wait_for_interrupt();
    }
}

#[unsafe(no_mangle)]
extern "C" fn coco_port_riscv_trap(frame: &mut [usize; 32]) {
    let cause = csr!(read "cause");
    let interrupt = cause & (1 << (usize::BITS - 1)) != 0;
    let code = cause & !(1 << (usize::BITS - 1));
    if interrupt && code == TIMER_IRQ {
        TICKS.fetch_add(1, Ordering::Relaxed);
        arm_timer(counter() + INTERVAL.load(Ordering::Relaxed) as u64);
        return;
    }
    if !interrupt && code == 3 {
        BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
        let epc = frame[0];
        // SAFETY: epc points at the ebreak just executed (kernel text).
        let insn = unsafe { (epc as *const u16).read_volatile() };
        frame[0] = epc + if insn & 3 == 3 { 4 } else { 2 }; // compressed c.ebreak is 2 bytes
        return;
    }
    let tval = csr!(read "tval");
    println!("FATAL: trap cause {:#x} at pc {:#x}, tval {:#x}", cause, frame[0], tval);
    exit(false)
}

#[unsafe(no_mangle)]
extern "C" fn coco_port_riscv_main(_hart: usize, dtb: usize) -> ! {
    csr!(write "tvec", (&raw const coco_port_trap_entry) as usize);
    // Keep the FPU off (status.FS = Off): the trap handler does not save FP
    // registers, so any FP instruction must trap visibly instead.
    #[cfg(target_arch = "riscv64")]
    // SAFETY: clearing sstatus.FS only disables FP instructions.
    unsafe {
        core::arch::asm!("csrc sstatus, {}", in(reg) 3usize << 13, options(nomem, nostack));
    }
    let mut platform = Platform::new("device tree");
    // SAFETY: firmware/QEMU passes the device-tree address in a1. Only the
    // fixed header is read before the declared size is checked by the
    // parser; the blob lies in RAM below the kernel's use.
    let blob = unsafe { dtb_slice(dtb) };
    match blob.map(|b| coco_fdt::parse(b, b"ns16550a")) {
        Some(Ok(s)) => {
            if let Some(uart) = s.device {
                UART.store(uart as usize, Ordering::Relaxed);
            }
            for r in s.memory.as_slice() {
                platform.add_ram(*r);
            }
            for r in s.reserved.as_slice() {
                platform.add_reserved(*r);
            }
            platform.add_reserved(Range { base: dtb as u64, size: u64::from(s.total_size) });
        }
        other => {
            println!("FATAL: no valid device tree at {:#x}: {:?}", dtb, other.map(|r| r.err()));
            exit(false);
        }
    }
    // RV32 runs without firmware: nothing else occupies RAM, and the kernel
    // image itself is excluded by `kernel_main`.
    crate::kernel_main(&platform)
}

/// Returns the device-tree blob at `addr` with its declared size.
///
/// # Safety
/// `addr` must be a readable physical address holding an FDT header.
unsafe fn dtb_slice(addr: usize) -> Option<&'static [u8]> {
    if addr == 0 || !addr.is_multiple_of(4) {
        return None;
    }
    // SAFETY: caller contract; the magic and size fields are read first.
    let magic = u32::from_be(unsafe { (addr as *const u32).read_volatile() });
    if magic != 0xD00D_FEED {
        return None;
    }
    // SAFETY: as above.
    let size = u32::from_be(unsafe { ((addr + 4) as *const u32).read_volatile() }) as usize;
    if !(40..=(2 << 20)).contains(&size) {
        return None;
    }
    // SAFETY: caller contract plus the bounded size check above; the blob is
    // immutable for the kernel's lifetime (reserved from the allocator).
    Some(unsafe { core::slice::from_raw_parts(addr as *const u8, size) })
}
