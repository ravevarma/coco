//! Privileged CPU instructions and control registers.

use core::arch::asm;

/// IA32_EFER MSR.
pub const MSR_EFER: u32 = 0xC000_0080;
/// EFER.NXE (no-execute enable).
pub const EFER_NXE: u64 = 1 << 11;
/// CR0.WP (supervisor write protection).
pub const CR0_WP: u64 = 1 << 16;
/// CR4.LA57 (5-level paging active).
pub const CR4_LA57: u64 = 1 << 12;

/// Result of a `cpuid` query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuidResult {
    /// EAX.
    pub eax: u32,
    /// EBX.
    pub ebx: u32,
    /// ECX.
    pub ecx: u32,
    /// EDX.
    pub edx: u32,
}

/// Executes `cpuid` for `leaf`/`subleaf`.
#[must_use]
pub fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    // SAFETY: cpuid is unprivileged, has no memory effects and is available
    // on every x86_64 CPU. rbx is reserved by LLVM, so it is saved in a
    // scratch register around the instruction.
    unsafe {
        asm!(
            "mov {tmp:r}, rbx",
            "cpuid",
            "xchg {tmp:r}, rbx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack, preserves_flags, nomem),
        );
    }
    CpuidResult { eax, ebx, ecx, edx }
}

/// True if the CPU supports the NX (execute-disable) page bit.
#[must_use]
pub fn has_nx() -> bool {
    cpuid(0x8000_0000, 0).eax >= 0x8000_0001 && cpuid(0x8000_0001, 0).edx & (1 << 20) != 0
}

/// True if the CPU has an invariant TSC.
#[must_use]
pub fn has_invariant_tsc() -> bool {
    cpuid(0x8000_0000, 0).eax >= 0x8000_0007 && cpuid(0x8000_0007, 0).edx & (1 << 8) != 0
}

/// Reads the time-stamp counter.
#[must_use]
pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: rdtsc is unprivileged unless CR4.TSD is set, which coco never
    // sets; it has no memory effects.
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nostack, nomem, preserves_flags)) };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Reads a model-specific register.
///
/// # Safety
/// Must run at CPL 0 and `msr` must exist on this CPU (otherwise #GP).
#[must_use]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: caller guarantees CPL 0 and a valid MSR.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nostack, nomem, preserves_flags)) };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Writes a model-specific register.
///
/// # Safety
/// Must run at CPL 0; `msr` must exist and `value` must not violate any
/// memory-safety assumption of the running code (e.g. disabling long mode).
pub unsafe fn wrmsr(msr: u32, value: u64) {
    // SAFETY: caller contract.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack, preserves_flags));
    }
}

/// Reads CR0.
#[must_use]
pub fn read_cr0() -> u64 {
    let v: u64;
    // SAFETY: reading CR0 has no side effects (CPL 0 is an invariant of all
    // coco code linking this crate: loader and kernel).
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nostack, nomem, preserves_flags)) };
    v
}

/// Writes CR0.
///
/// # Safety
/// The new value must keep paging and protected mode enabled and must not
/// break assumptions of running code.
pub unsafe fn write_cr0(v: u64) {
    // SAFETY: caller contract.
    unsafe { asm!("mov cr0, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Reads CR2 (last page-fault linear address).
#[must_use]
pub fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: side-effect-free read at CPL 0.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nostack, nomem, preserves_flags)) };
    v
}

/// Reads CR3 (page-table root).
#[must_use]
pub fn read_cr3() -> u64 {
    let v: u64;
    // SAFETY: side-effect-free read at CPL 0.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nostack, nomem, preserves_flags)) };
    v
}

/// Loads CR3, switching address space and flushing non-global TLB entries.
///
/// # Safety
/// `v` must reference a valid PML4 that maps the currently executing code,
/// the current stack and every object live Rust references point to.
pub unsafe fn write_cr3(v: u64) {
    // SAFETY: caller contract. Not `nomem`: acts as a compiler barrier so
    // page-table writes are not reordered past the switch.
    unsafe { asm!("mov cr3, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Reads CR4.
#[must_use]
pub fn read_cr4() -> u64 {
    let v: u64;
    // SAFETY: side-effect-free read at CPL 0.
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nostack, nomem, preserves_flags)) };
    v
}

/// Invalidates the TLB entry for `addr`.
pub fn invlpg(addr: u64) {
    // SAFETY: invlpg only drops a cached translation; it cannot violate
    // memory safety (a stale entry being dropped is always conservative).
    // Not `nomem`: orders preceding page-table writes.
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

/// Disables maskable interrupts.
pub fn disable_interrupts() {
    // SAFETY: clearing IF cannot break memory safety. Not `nomem` so the
    // compiler does not move memory accesses out of a critical section.
    unsafe { asm!("cli", options(nostack)) };
}

/// Halts until the next interrupt (forever if interrupts are disabled).
pub fn halt() {
    // SAFETY: hlt only idles the CPU.
    unsafe { asm!("hlt", options(nostack, nomem, preserves_flags)) };
}

/// Stops this CPU permanently.
pub fn halt_forever() -> ! {
    loop {
        disable_interrupts();
        halt();
    }
}

/// Raises a breakpoint exception (#BP, vector 3).
pub fn breakpoint() {
    // SAFETY: int3 transfers to the installed #BP handler, which returns.
    // Callers must have installed an IDT (kernel invariant after
    // `interrupts::init`); without one the CPU triple-faults, which is a
    // controlled reset, not memory unsafety.
    unsafe { asm!("int3", options(nostack)) };
}

/// Writes a byte to an I/O port.
///
/// # Safety
/// The port write must not violate memory safety (e.g. programming a DMA
/// engine) and the caller must own the device behind `port`.
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: caller contract.
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nostack, nomem, preserves_flags)) };
}

/// Writes a 32-bit value to an I/O port.
///
/// # Safety
/// As for [`outb`].
pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: caller contract.
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nostack, nomem, preserves_flags)) };
}

/// Reads a byte from an I/O port.
///
/// # Safety
/// The read must not have side effects that violate memory safety, and the
/// caller must own the device behind `port`.
#[must_use]
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: caller contract.
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nostack, nomem, preserves_flags)) };
    v
}

/// IA32_APIC_BASE MSR.
pub const MSR_APIC_BASE: u32 = 0x1B;
/// CR4.UMIP (user-mode instruction prevention).
pub const CR4_UMIP: u64 = 1 << 11;
/// CR4.SMEP (supervisor-mode execution prevention).
pub const CR4_SMEP: u64 = 1 << 20;
/// CR4.SMAP (supervisor-mode access prevention).
pub const CR4_SMAP: u64 = 1 << 21;
const RFLAGS_IF: u64 = 1 << 9;

/// Writes CR4.
///
/// # Safety
/// The new value must only enable features the CPU supports and must not
/// invalidate assumptions of running code (e.g. clearing PAE).
pub unsafe fn write_cr4(v: u64) {
    // SAFETY: caller contract.
    unsafe { asm!("mov cr4, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// CPU features coco checks at boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    /// Supervisor-mode execution prevention.
    pub smep: bool,
    /// Supervisor-mode access prevention.
    pub smap: bool,
    /// User-mode instruction prevention.
    pub umip: bool,
    /// x2APIC supported.
    pub x2apic: bool,
    /// Invariant TSC.
    pub invariant_tsc: bool,
}

/// Queries CPUID for [`Features`].
#[must_use]
pub fn features() -> Features {
    let max = cpuid(0, 0).eax;
    let leaf7 = if max >= 7 { cpuid(7, 0) } else { CpuidResult::default() };
    Features {
        smep: leaf7.ebx & (1 << 7) != 0,
        smap: leaf7.ebx & (1 << 20) != 0,
        umip: leaf7.ecx & (1 << 2) != 0,
        x2apic: cpuid(1, 0).ecx & (1 << 21) != 0,
        invariant_tsc: has_invariant_tsc(),
    }
}

/// True if maskable interrupts are enabled (RFLAGS.IF).
#[must_use]
pub fn interrupts_enabled() -> bool {
    let flags: u64;
    // SAFETY: reading RFLAGS has no side effects.
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags)) };
    flags & RFLAGS_IF != 0
}

/// Enables maskable interrupts.
///
/// # Safety
/// An IDT able to handle every unmasked interrupt source must be loaded,
/// and no lock that interrupt handlers take may be held.
pub unsafe fn enable_interrupts() {
    // SAFETY: caller contract. Not `nomem`: acts as a compiler barrier at
    // the end of a critical section.
    unsafe { asm!("sti", options(nostack)) };
}

/// Atomically enables interrupts and halts until the next one (`sti; hlt`
/// executes `hlt` in the interrupt shadow, so no wake-up is lost).
///
/// # Safety
/// As for [`enable_interrupts`].
pub unsafe fn enable_interrupts_and_halt() {
    // SAFETY: caller contract.
    unsafe { asm!("sti", "hlt", options(nostack)) };
}

/// Writes a 16-bit value to an I/O port.
///
/// # Safety
/// As for [`outb`].
pub unsafe fn outw(port: u16, value: u16) {
    // SAFETY: caller contract.
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nostack, nomem, preserves_flags)) };
}

/// Reads a 16-bit value from an I/O port.
///
/// # Safety
/// As for [`inb`].
#[must_use]
pub unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    // SAFETY: caller contract.
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") v, options(nostack, nomem, preserves_flags)) };
    v
}

/// Reads a 32-bit value from an I/O port.
///
/// # Safety
/// As for [`inb`].
#[must_use]
pub unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    // SAFETY: caller contract.
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") v, options(nostack, nomem, preserves_flags)) };
    v
}
