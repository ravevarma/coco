//! 32-bit x86 backend: Multiboot v1 (QEMU `-kernel`, GRUB), protected mode
//! with a flat GDT, 8259 PIC + PIT at 100 Hz, COM1 console, TSC calibrated
//! against PIT channel 2, `isa-debug-exit` for QEMU termination.
//!
//! Every trap stub saves the general registers *and* the SSE state
//! (FXSAVE): compiled Rust code may use XMM registers, so a handler must
//! not clobber the interrupted code's vector state.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

use coco_fdt::Range;

use crate::{BREAKPOINTS, Platform, TICKS, println};

pub const NAME: &str = "x86 (32-bit protected mode, Multiboot)";

const COM1: u16 = 0x3F8;
const MULTIBOOT_MAGIC: u32 = 0x2BAD_B002;
const PIT_HZ: u64 = 1_193_182;
/// Number of IDT vectors with stubs: 32 exceptions + 16 PIC IRQs.
const VECTORS: usize = 48;
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

global_asm!(
    ".section .multiboot, \"a\"",
    ".balign 4",
    ".long 0x1BADB002",
    ".long 0x00000003", // page-align modules, provide memory map
    ".long -(0x1BADB002 + 0x00000003)",
    "",
    ".section .text.entry, \"ax\"",
    ".global _start",
    "_start:",
    "    cli",
    "    movl $__stack_top, %esp",
    "    movl %eax, %esi", // Multiboot magic
    "    cld",
    "    movl $__bss_start, %edi",
    "    movl $__bss_end, %ecx",
    "    subl %edi, %ecx",
    "    xorl %eax, %eax",
    "    rep stosb", // %ebx (info pointer) is preserved
    "    lgdt coco_gdt_ptr",
    "    ljmp $0x08, $2f",
    "2:  movw $0x10, %ax",
    "    movw %ax, %ds",
    "    movw %ax, %es",
    "    movw %ax, %fs",
    "    movw %ax, %gs",
    "    movw %ax, %ss",
    // SSE on: CR0.EM = 0, CR0.MP = 1; CR4.OSFXSR | CR4.OSXMMEXCPT.
    "    movl %cr0, %eax",
    "    andl $0xFFFFFFFB, %eax",
    "    orl $0x2, %eax",
    "    movl %eax, %cr0",
    "    movl %cr4, %eax",
    "    orl $0x600, %eax",
    "    movl %eax, %cr4",
    "    fninit",
    "    andl $0xFFFFFFF0, %esp",
    "    subl $8, %esp",
    "    pushl %ebx",
    "    pushl %esi",
    "    call coco_port_x86_main", // cdecl; esp 16-aligned at the call
    "3:  hlt",
    "    jmp 3b",
    "",
    ".section .text",
    ".macro COCO_ISR n",
    ".balign 16",
    ".if (\\n == 8) || ((\\n >= 10) && (\\n <= 14)) || (\\n == 17) || (\\n == 21) || (\\n == 29) || (\\n == 30)",
    ".else",
    "    pushl $0", // uniform frame: dummy error code
    ".endif",
    "    pushl $\\n",
    "    jmp coco_isr_common",
    ".endm",
    ".balign 16",
    ".global coco_isr_stubs",
    "coco_isr_stubs:",
    "COCO_ISR 0",
    "COCO_ISR 1",
    "COCO_ISR 2",
    "COCO_ISR 3",
    "COCO_ISR 4",
    "COCO_ISR 5",
    "COCO_ISR 6",
    "COCO_ISR 7",
    "COCO_ISR 8",
    "COCO_ISR 9",
    "COCO_ISR 10",
    "COCO_ISR 11",
    "COCO_ISR 12",
    "COCO_ISR 13",
    "COCO_ISR 14",
    "COCO_ISR 15",
    "COCO_ISR 16",
    "COCO_ISR 17",
    "COCO_ISR 18",
    "COCO_ISR 19",
    "COCO_ISR 20",
    "COCO_ISR 21",
    "COCO_ISR 22",
    "COCO_ISR 23",
    "COCO_ISR 24",
    "COCO_ISR 25",
    "COCO_ISR 26",
    "COCO_ISR 27",
    "COCO_ISR 28",
    "COCO_ISR 29",
    "COCO_ISR 30",
    "COCO_ISR 31",
    "COCO_ISR 32",
    "COCO_ISR 33",
    "COCO_ISR 34",
    "COCO_ISR 35",
    "COCO_ISR 36",
    "COCO_ISR 37",
    "COCO_ISR 38",
    "COCO_ISR 39",
    "COCO_ISR 40",
    "COCO_ISR 41",
    "COCO_ISR 42",
    "COCO_ISR 43",
    "COCO_ISR 44",
    "COCO_ISR 45",
    "COCO_ISR 46",
    "COCO_ISR 47",
    "coco_isr_common:",
    "    pushal",
    "    movl %esp, %ebp", // -> Frame
    "    subl $512, %esp",
    "    andl $0xFFFFFFF0, %esp",
    "    fxsave (%esp)",
    "    subl $12, %esp",
    "    pushl %ebp",
    "    call coco_port_x86_trap",
    "    addl $16, %esp",
    "    fxrstor (%esp)",
    "    movl %ebp, %esp",
    "    popal",
    "    addl $8, %esp", // vector + error code
    "    iretl",
    "",
    ".section .data",
    ".balign 8",
    "coco_gdt:",
    "    .quad 0",
    "    .quad 0x00CF9B000000FFFF", // ring-0 code, flat, accessed
    "    .quad 0x00CF93000000FFFF", // ring-0 data, flat, accessed
    "coco_gdt_ptr:",
    "    .word 23",
    "    .long coco_gdt",
    "",
    ".section .bss",
    ".balign 8",
    ".global coco_idt",
    "coco_idt:",
    "    .skip 8 * 48",
    options(att_syntax),
);

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static coco_isr_stubs: u8;
    static mut coco_idt: [u64; VECTORS];
}

/// Register state pushed by the stubs (`pushal` order, then vector and
/// error code, then the CPU's interrupt frame).
#[repr(C)]
struct Frame {
    edi: u32,
    esi: u32,
    ebp: u32,
    esp: u32,
    ebx: u32,
    edx: u32,
    ecx: u32,
    eax: u32,
    vector: u32,
    error: u32,
    eip: u32,
    cs: u32,
    eflags: u32,
}

fn outb(port: u16, v: u8) {
    // SAFETY: legacy I/O ports of the PC platform (PIC, PIT, COM1, debug
    // exit); writes only reconfigure those devices.
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) };
}

fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: as `outb`; reads of these ports have no memory effects.
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack, preserves_flags)) };
    v
}

fn serial_init() {
    outb(COM1 + 1, 0x00); // no UART interrupts
    outb(COM1 + 3, 0x80); // DLAB
    outb(COM1, 0x01); // 115200 baud
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x03); // 8N1
    outb(COM1 + 2, 0xC7); // FIFO on, cleared
    outb(COM1 + 4, 0x03); // DTR | RTS
}

pub fn putc(b: u8) {
    for _ in 0..100_000 {
        if inb(COM1 + 5) & 0x20 != 0 {
            break;
        }
    }
    outb(COM1, b);
}

pub fn counter() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: `rdtsc` reads the time-stamp counter (CR4.TSD is clear).
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags)) };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// TSC frequency measured against PIT channel 2 (set by `start_timer`).
pub fn counter_hz() -> Option<u64> {
    let hz = TSC_HZ.load(Ordering::Relaxed);
    (hz != 0).then_some(hz)
}

/// Measures the TSC over a 50 ms PIT channel-2 one-shot.
fn calibrate_tsc() -> u64 {
    const COUNT: u64 = PIT_HZ / 20;
    outb(0x61, (inb(0x61) & !0x02) | 0x01); // gate on, speaker off
    outb(0x43, 0xB0); // channel 2, lo/hi, mode 0
    outb(0x42, COUNT as u8);
    outb(0x42, (COUNT >> 8) as u8);
    let start = counter();
    let mut ok = false;
    for _ in 0..50_000_000u32 {
        if inb(0x61) & 0x20 != 0 {
            ok = true;
            break;
        }
    }
    let end = counter();
    outb(0x61, inb(0x61) & !0x01);
    if ok { end.wrapping_sub(start).saturating_mul(20) } else { 0 }
}

pub fn start_timer(hz: u32) {
    TSC_HZ.store(calibrate_tsc(), Ordering::Relaxed);
    // 8259 PICs: vectors 0x20/0x28, cascade on IRQ2, only IRQ0 unmasked.
    for (port, v) in [
        (0x20, 0x11),
        (0xA0, 0x11),
        (0x21, 0x20),
        (0xA1, 0x28),
        (0x21, 0x04),
        (0xA1, 0x02),
        (0x21, 0x01),
        (0xA1, 0x01),
        (0x21, 0xFE),
        (0xA1, 0xFF),
    ] {
        outb(port, v);
    }
    let divisor = (PIT_HZ / u64::from(hz.max(19))).min(0xFFFF) as u16;
    outb(0x43, 0x36); // channel 0, lo/hi, mode 3
    outb(0x40, divisor as u8);
    outb(0x40, (divisor >> 8) as u8);
    // SAFETY: the IDT is loaded (in `coco_port_x86_main`); enabling
    // interrupts only starts delivery to the installed stubs.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

pub fn wait_for_interrupt() {
    // SAFETY: `hlt` waits for the next interrupt (IF is set by
    // `start_timer` before any caller waits).
    unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
}

pub fn breakpoint() {
    // SAFETY: vector 3 is a trap: the handler returns past the `int3`.
    unsafe { asm!("int3", options(nostack)) };
}

pub fn kernel_image() -> (u64, u64) {
    ((&raw const __kernel_start) as u32 as u64, (&raw const __kernel_end) as u32 as u64)
}

/// QEMU `isa-debug-exit` at port 0xF4: status `(v << 1) | 1`, so 33 means
/// pass and 35 failure.
pub fn exit(success: bool) -> ! {
    outb(0xF4, if success { 0x10 } else { 0x11 });
    loop {
        // SAFETY: interrupts off and halt forever if no exit device exists.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

fn install_idt() {
    let stubs = (&raw const coco_isr_stubs) as u32;
    let idt = &raw mut coco_idt;
    for v in 0..VECTORS {
        let handler = u64::from(stubs + 16 * v as u32);
        // 32-bit interrupt gate, present, DPL 0, selector 0x08.
        let gate = (handler & 0xFFFF) | (0x08 << 16) | (0x8E << 40) | ((handler >> 16) << 48);
        // SAFETY: single-threaded boot with interrupts disabled; `coco_idt`
        // is only written here, before `lidt`; `v < VECTORS`.
        unsafe { (*idt)[v] = gate };
    }
    #[repr(C, packed)]
    struct Pointer {
        limit: u16,
        base: u32,
    }
    let ptr = Pointer { limit: (8 * VECTORS - 1) as u16, base: idt as u32 };
    // SAFETY: the table is fully initialised and lives for the kernel's
    // lifetime; every gate points at a stub in `.text`.
    unsafe { asm!("lidt [{}]", in(reg) &raw const ptr, options(readonly, nostack, preserves_flags)) };
}

#[unsafe(no_mangle)]
extern "C" fn coco_port_x86_trap(frame: &mut Frame) {
    match frame.vector {
        3 => {
            BREAKPOINTS.fetch_add(1, Ordering::Relaxed); // trap: EIP is already past int3
            return;
        }
        32 => {
            TICKS.fetch_add(1, Ordering::Relaxed);
            outb(0x20, 0x20);
            return;
        }
        // Spurious IRQ7/IRQ15 (in-service bit clear): no EOI for that PIC.
        39 => return,
        47 => {
            outb(0x20, 0x20);
            return;
        }
        33..=46 => {
            if frame.vector >= 40 {
                outb(0xA0, 0x20);
            }
            outb(0x20, 0x20);
            return;
        }
        _ => {}
    }
    let cr2: u32;
    // SAFETY: reading CR2 at ring 0 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    println!(
        "FATAL: exception {} (error {:#x}) at {:#x}:{:#x}, eflags {:#x}, cr2 {:#x}, eax {:#x} esp {:#x}",
        frame.vector, frame.error, frame.cs, frame.eip, frame.eflags, cr2, frame.eax, frame.esp
    );
    exit(false)
}

fn read_u32(addr: u32) -> u32 {
    // SAFETY: callers pass addresses inside the Multiboot information
    // structure or memory map, which the loader placed in RAM; paging is
    // off, so physical addresses are directly usable.
    unsafe { (addr as usize as *const u32).read_unaligned() }
}

#[unsafe(no_mangle)]
extern "C" fn coco_port_x86_main(magic: u32, info: u32) -> ! {
    serial_init();
    install_idt();
    if magic != MULTIBOOT_MAGIC || info == 0 {
        println!("FATAL: not started by a Multiboot loader (magic {:#x})", magic);
        exit(false);
    }
    let mut platform = Platform::new("Multiboot memory map");
    platform.add_reserved(Range { base: u64::from(info), size: 52 });
    let flags = read_u32(info);
    if flags & (1 << 6) != 0 {
        let len = read_u32(info + 44);
        let addr = read_u32(info + 48);
        platform.add_reserved(Range { base: u64::from(addr), size: u64::from(len) });
        let end = addr.saturating_add(len);
        let mut entry = addr;
        // Each entry: size (excluding itself), base u64, length u64, type.
        while entry.saturating_add(24) <= end {
            let size = read_u32(entry);
            let base = u64::from(read_u32(entry + 4)) | (u64::from(read_u32(entry + 8)) << 32);
            let length = u64::from(read_u32(entry + 12)) | (u64::from(read_u32(entry + 16)) << 32);
            if read_u32(entry + 20) == 1 {
                platform.add_ram(Range { base, size: length });
            }
            match entry.checked_add(size).and_then(|e| e.checked_add(4)) {
                Some(next) if size >= 20 => entry = next,
                _ => break, // malformed entry size: stop rather than loop
            }
        }
    } else if flags & 1 != 0 {
        platform.source = "Multiboot mem_upper";
        let upper_kib = read_u32(info + 8);
        platform.add_ram(Range { base: 1 << 20, size: u64::from(upper_kib) * 1024 });
    }
    crate::kernel_main(&platform)
}

// ----------------------------------------------------------- memory routines
//
// The hosted i686 target's `compiler_builtins` expects libc to provide these.
// String instructions are used so the compiler cannot lower the bodies back
// into calls to the same functions. The direction flag is clear on entry
// (System V ABI) and restored to clear after a backward copy.

/// # Safety
/// Standard C `memcpy` contract: valid, non-overlapping regions of `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    // SAFETY: caller contract; `rep movsb` copies exactly `n` bytes forward.
    // LLVM reserves `esi`, so it is swapped in and restored around the copy.
    unsafe {
        asm!("xchg esi, {s}", "rep movsb", "xchg esi, {s}", s = inout(reg) src => _, inout("ecx") n => _, inout("edi") dst => _, options(nostack, preserves_flags));
    }
    dst
}

/// # Safety
/// Standard C `memmove` contract: valid regions of `n` bytes (may overlap).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dst as usize) <= (src as usize) || (dst as usize) >= (src as usize).wrapping_add(n) {
        // SAFETY: a forward copy is correct when dst precedes src or the
        // regions are disjoint.
        return unsafe { memcpy(dst, src, n) };
    }
    if n == 0 {
        return dst;
    }
    // SAFETY: overlapping with dst above src: copy backwards from the last
    // byte; the flag is cleared again before returning.
    unsafe {
        asm!("xchg esi, {s}", "std", "rep movsb", "cld", "xchg esi, {s}", s = inout(reg) src.wrapping_add(n - 1) => _, inout("ecx") n => _, inout("edi") dst.wrapping_add(n - 1) => _, options(nostack));
    }
    dst
}

/// # Safety
/// Standard C `memset` contract: a valid region of `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    // SAFETY: caller contract; `rep stosb` writes exactly `n` bytes.
    unsafe {
        asm!("rep stosb", inout("ecx") n => _, inout("edi") dst => _, in("al") c as u8, options(nostack, preserves_flags))
    };
    dst
}

/// # Safety
/// Standard C `memcmp` contract: valid regions of `n` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    // SAFETY: caller contract: both regions are valid for `n` bytes.
    let a = unsafe { core::slice::from_raw_parts(a, n) };
    // SAFETY: as above.
    let b = unsafe { core::slice::from_raw_parts(b, n) };
    for (&x, &y) in a.iter().zip(b) {
        if x != y {
            return i32::from(x) - i32::from(y);
        }
    }
    0
}

/// # Safety
/// As [`memcmp`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    // SAFETY: same contract.
    unsafe { memcmp(a, b, n) }
}
