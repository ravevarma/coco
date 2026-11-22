//! Interrupt controllers and the system tick.
//!
//! * The legacy 8259 PICs are remapped to vectors 0x20-0x2F and fully
//!   masked (only their spurious vectors can still arrive).
//! * The boot CPU's local APIC is used in whichever mode the firmware left
//!   it (xAPIC via uncached MMIO, or x2APIC via MSRs).
//! * The TSC and the APIC timer are calibrated against the ACPI PM timer
//!   (fallback: HPET) and the APIC timer then runs periodically at [`HZ`].
//!
//! Handlers are allocation-free and lock-free (they touch atomics and APIC
//! registers only).

use core::hint::black_box;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use coco_acpi::AcpiInfo;
use coco_arch::cpu;

use crate::{kprintln_emergency, mm};

/// System tick frequency.
pub const HZ: u64 = 100;
/// Timer interrupt vector.
pub const TIMER_VECTOR: u8 = 0x20;
/// Local APIC error vector.
pub const ERROR_VECTOR: u8 = 0xFE;
/// Local APIC spurious vector.
pub const SPURIOUS_VECTOR: u8 = 0xFF;
const PIC_SPURIOUS: [u8; 2] = [0x27, 0x2F];

const LAPIC_TPR: u32 = 0x80;
const LAPIC_EOI: u32 = 0xB0;
const LAPIC_SVR: u32 = 0xF0;
const LAPIC_ESR: u32 = 0x280;
const LAPIC_LVT_TIMER: u32 = 0x320;
const LAPIC_LVT_LINT0: u32 = 0x350;
const LAPIC_LVT_ERROR: u32 = 0x370;
const LAPIC_TIMER_INIT: u32 = 0x380;
const LAPIC_TIMER_CURRENT: u32 = 0x390;
const LAPIC_TIMER_DIVIDE: u32 = 0x3E0;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const PM_TIMER_HZ: u64 = 3_579_545;

static LAPIC_VIRT: AtomicU64 = AtomicU64::new(0);
static X2APIC: AtomicBool = AtomicBool::new(false);
static TICKS: AtomicU64 = AtomicU64::new(0);
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static SPURIOUS: AtomicU64 = AtomicU64::new(0);
static UNEXPECTED: AtomicU64 = AtomicU64::new(0);
/// Self-test hook: make the timer handler attempt a heap allocation.
pub static ALLOC_IN_IRQ_TEST: AtomicBool = AtomicBool::new(false);

/// Interrupt-controller initialization errors.
#[derive(Debug)]
#[allow(dead_code, reason = "payloads are read through the Debug impl")]
pub enum IrqError {
    /// The local APIC is disabled or absent.
    NoLapic,
    /// The APIC MMIO page could not be mapped.
    Map(coco_memory::paging::MapError),
    /// No reference clock (PM timer or HPET) to calibrate against.
    NoReferenceClock,
    /// The reference clock did not advance.
    ReferenceClockStuck,
}

fn lapic_read(reg: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: x2APIC mode is active (checked in `init`), so MSR
        // 0x800 + reg/16 exists; reads have no side effects.
        unsafe { cpu::rdmsr(0x800 + (reg >> 4)) as u32 }
    } else {
        let base = LAPIC_VIRT.load(Ordering::Relaxed);
        // SAFETY: `base` is the uncached mapping of the local APIC page made
        // in `init` (non-zero once set); `reg` is a register offset < 4096.
        unsafe { ((base + u64::from(reg)) as *const u32).read_volatile() }
    }
}

fn lapic_write(reg: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: as in `lapic_read`; APIC register writes affect only
        // interrupt delivery, never memory.
        unsafe { cpu::wrmsr(0x800 + (reg >> 4), u64::from(value)) };
    } else {
        let base = LAPIC_VIRT.load(Ordering::Relaxed);
        // SAFETY: as in `lapic_read`.
        unsafe { ((base + u64::from(reg)) as *mut u32).write_volatile(value) };
    }
}

fn lapic_ready() -> bool {
    X2APIC.load(Ordering::Relaxed) || LAPIC_VIRT.load(Ordering::Relaxed) != 0
}

/// Signals end-of-interrupt to the local APIC.
fn eoi() {
    if lapic_ready() {
        lapic_write(LAPIC_EOI, 0);
    }
}

/// Remaps the 8259 PICs away from the exception vectors and masks them.
fn disable_pic() {
    let out = |port: u16, v: u8| {
        // SAFETY: the kernel owns the legacy PIC ports after
        // ExitBootServices; programming/masking the PIC affects only
        // interrupt delivery.
        unsafe { cpu::outb(port, v) };
        // SAFETY: port 0x80 is the POST diagnostic port, used as an I/O delay.
        unsafe { cpu::outb(0x80, 0) };
    };
    out(0x20, 0x11);
    out(0xA0, 0x11);
    out(0x21, 0x20); // master base vector
    out(0xA1, 0x28); // slave base vector
    out(0x21, 0x04);
    out(0xA1, 0x02);
    out(0x21, 0x01);
    out(0xA1, 0x01);
    out(0x21, 0xFF); // mask everything
    out(0xA1, 0xFF);
}

/// A reference clock with a known frequency.
#[derive(Clone, Copy, Debug)]
enum RefClock {
    PmTimer { port: u16, mask: u64 },
    Hpet { counter: u64, hz: u64 },
}

impl RefClock {
    fn hz(self) -> u64 {
        match self {
            Self::PmTimer { .. } => PM_TIMER_HZ,
            Self::Hpet { hz, .. } => hz,
        }
    }

    fn read(self) -> u64 {
        match self {
            // SAFETY: the port comes from the validated FADT; reading the PM
            // timer has no side effects.
            Self::PmTimer { port, mask } => u64::from(unsafe { cpu::inl(port) }) & mask,
            // SAFETY: `counter` is the uncached mapping of the HPET main
            // counter register (HPET base + 0xF0), mapped in `init`.
            Self::Hpet { counter, .. } => unsafe { (counter as *const u64).read_volatile() },
        }
    }

    fn mask(self) -> u64 {
        match self {
            Self::PmTimer { mask, .. } => mask,
            Self::Hpet { .. } => u64::MAX,
        }
    }
}

fn reference_clock(acpi: Option<&AcpiInfo>) -> Result<RefClock, IrqError> {
    let fadt = acpi.and_then(|a| a.fadt);
    if let Some(port) = fadt.and_then(|f| f.pm_timer_port) {
        let mask = if fadt.is_some_and(|f| f.pm_timer_32bit) { 0xFFFF_FFFF } else { 0x00FF_FFFF };
        return Ok(RefClock::PmTimer { port, mask });
    }
    let hpet = acpi.and_then(|a| a.hpet).ok_or(IrqError::NoReferenceClock)?;
    let base = mm::with(|m| m.map_mmio(hpet)).ok_or(IrqError::NoReferenceClock)?.map_err(IrqError::Map)?;
    // SAFETY: `base` maps the HPET register page uncached; offset 0 is the
    // capabilities register (upper 32 bits = period in femtoseconds).
    let caps = unsafe { (base as *const u64).read_volatile() };
    let period_fs = caps >> 32;
    if period_fs == 0 || period_fs > 100_000_000 {
        return Err(IrqError::NoReferenceClock);
    }
    let config = (base + 0x10) as *mut u64;
    // SAFETY: HPET general configuration register (uncached mapping);
    // reading it has no side effects.
    let current = unsafe { config.read_volatile() };
    // SAFETY: setting ENABLE_CNF starts the main counter and affects
    // nothing else.
    unsafe { config.write_volatile(current | 1) };
    Ok(RefClock::Hpet { counter: base + 0xF0, hz: 1_000_000_000_000_000 / period_fs })
}

/// Busy-waits for `ms` milliseconds of the reference clock, returning
/// `(tsc_delta, apic_timer_ticks)`.
fn measure(clock: RefClock, ms: u64) -> Result<(u64, u64), IrqError> {
    let target = clock.hz() * ms / 1000;
    lapic_write(LAPIC_TIMER_DIVIDE, 0x3); // divide by 16
    lapic_write(LAPIC_LVT_TIMER, LVT_MASKED | u32::from(TIMER_VECTOR));
    lapic_write(LAPIC_TIMER_INIT, u32::MAX);
    let tsc0 = cpu::rdtsc();
    let mut last = clock.read();
    let mut elapsed = 0u64;
    while elapsed < target {
        let now = clock.read();
        elapsed += now.wrapping_sub(last) & clock.mask();
        last = now;
        // A healthy clock advances well within 2^34 TSC cycles (> 1 s even
        // at 16 GHz); a stuck one must not hang the boot.
        if cpu::rdtsc().wrapping_sub(tsc0) > (1 << 34) && elapsed == 0 {
            return Err(IrqError::ReferenceClockStuck);
        }
        core::hint::spin_loop();
    }
    let tsc = cpu::rdtsc().wrapping_sub(tsc0);
    let apic = u64::from(u32::MAX - lapic_read(LAPIC_TIMER_CURRENT));
    lapic_write(LAPIC_TIMER_INIT, 0);
    Ok((tsc, apic))
}

/// Result of interrupt-controller initialization.
#[derive(Debug, Clone, Copy)]
pub struct TimerInfo {
    /// Calibrated TSC frequency.
    pub tsc_hz: u64,
    /// Calibrated APIC timer input frequency (after the /16 divider).
    pub apic_timer_hz: u64,
    /// Local APIC in x2APIC mode.
    pub x2apic: bool,
    /// Calibration reference.
    pub reference: &'static str,
}

/// Disables the PIC, enables the local APIC, calibrates and starts the
/// periodic tick. Interrupts stay disabled; the caller enables them.
///
/// # Errors
/// See [`IrqError`].
pub fn init(acpi: Option<&AcpiInfo>, force_x2apic: bool) -> Result<TimerInfo, IrqError> {
    if acpi.and_then(|a| a.madt).is_none_or(|m| m.pcat_compat) {
        disable_pic();
    }
    // SAFETY: IA32_APIC_BASE exists on every x86_64 CPU; CPL 0.
    let mut apic_base = unsafe { cpu::rdmsr(cpu::MSR_APIC_BASE) };
    if force_x2apic && apic_base & (1 << 11) != 0 && apic_base & (1 << 10) == 0 && cpu::features().x2apic {
        apic_base |= 1 << 10;
        // SAFETY: xAPIC -> x2APIC (EN=1, EXTD 0->1) is an architecturally
        // permitted transition on CPUs reporting x2APIC; interrupts are
        // disabled and the APIC is not yet in use.
        unsafe { cpu::wrmsr(cpu::MSR_APIC_BASE, apic_base) };
    }
    if apic_base & (1 << 11) == 0 {
        return Err(IrqError::NoLapic);
    }
    if apic_base & (1 << 10) != 0 {
        X2APIC.store(true, Ordering::Relaxed);
    } else {
        let phys = acpi.and_then(|a| a.madt).map_or(apic_base & 0x000F_FFFF_FFFF_F000, |m| m.lapic_address);
        let virt = mm::with(|m| m.map_mmio(phys)).ok_or(IrqError::NoLapic)?.map_err(IrqError::Map)?;
        LAPIC_VIRT.store(virt, Ordering::Relaxed);
    }
    lapic_write(LAPIC_TPR, 0);
    lapic_write(LAPIC_LVT_LINT0, LVT_MASKED);
    lapic_write(LAPIC_LVT_ERROR, u32::from(ERROR_VECTOR));
    lapic_write(LAPIC_SVR, 0x100 | u32::from(SPURIOUS_VECTOR));

    let clock = reference_clock(acpi)?;
    let (tsc, apic) = measure(clock, 50)?;
    let tsc_hz = tsc * 20;
    let apic_hz = apic * 20;
    TSC_HZ.store(tsc_hz, Ordering::Relaxed);
    let initial = u32::try_from((apic_hz / HZ).max(1)).unwrap_or(u32::MAX);
    lapic_write(LAPIC_TIMER_DIVIDE, 0x3);
    lapic_write(LAPIC_LVT_TIMER, LVT_PERIODIC | u32::from(TIMER_VECTOR));
    lapic_write(LAPIC_TIMER_INIT, initial);
    Ok(TimerInfo {
        tsc_hz,
        apic_timer_hz: apic_hz,
        x2apic: X2APIC.load(Ordering::Relaxed),
        reference: match clock {
            RefClock::PmTimer { .. } => "ACPI PM timer",
            RefClock::Hpet { .. } => "HPET",
        },
    })
}

/// Hardware-interrupt dispatcher (called by `cpu::trap_dispatch`).
pub fn handle(vector: u8) {
    match vector {
        TIMER_VECTOR => {
            TICKS.fetch_add(1, Ordering::Relaxed);
            if ALLOC_IN_IRQ_TEST.load(Ordering::Relaxed) {
                // Deliberate rule violation for the self-test: must be refused.
                let b = alloc::boxed::Box::new(0xC0C0_u64);
                black_box(b);
            }
            eoi();
        }
        SPURIOUS_VECTOR => {
            SPURIOUS.fetch_add(1, Ordering::Relaxed); // no EOI for spurious
        }
        v if PIC_SPURIOUS.contains(&v) => {
            SPURIOUS.fetch_add(1, Ordering::Relaxed);
        }
        ERROR_VECTOR => {
            lapic_write(LAPIC_ESR, 0);
            let esr = lapic_read(LAPIC_ESR);
            kprintln_emergency!("apic: error interrupt, ESR {esr:#x}");
            eoi();
        }
        _ => {
            UNEXPECTED.fetch_add(1, Ordering::Relaxed);
            eoi();
        }
    }
}

/// Timer ticks since the timer started.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Calibrated TSC frequency (0 before `init`).
pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// `(spurious, unexpected)` interrupt counters.
pub fn anomaly_counts() -> (u64, u64) {
    (SPURIOUS.load(Ordering::Relaxed), UNEXPECTED.load(Ordering::Relaxed))
}

/// Sleeps (halting the CPU) until `n` more ticks have elapsed.
/// Must be called with interrupts enabled and no locks held.
pub fn sleep_ticks(n: u64) {
    let target = ticks() + n;
    while ticks() < target {
        // SAFETY: the IDT handles every vector, the timer is running and the
        // caller holds no locks (documented precondition).
        unsafe { cpu::enable_interrupts_and_halt() };
    }
}
