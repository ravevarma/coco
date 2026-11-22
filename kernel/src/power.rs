//! Reboot, ACPI soft-off (S5) and a CMOS-backed boot counter used by the
//! boot/reboot/shutdown lifecycle self-test.

use coco_acpi::{AcpiInfo, SPACE_IO, SPACE_MEMORY};
use coco_arch::cpu;

use crate::{kprintln, mm};

const SLP_EN: u16 = 1 << 13;
const SCI_EN: u16 = 1;

fn spin_ms(ms: u64) {
    let hz = crate::interrupts::tsc_hz().max(1_000_000_000);
    let start = cpu::rdtsc();
    while cpu::rdtsc().wrapping_sub(start) < hz / 1000 * ms {
        core::hint::spin_loop();
    }
}

/// Resets the machine: ACPI reset register, then port 0xCF9, then the
/// keyboard controller. Returns only if all methods failed.
pub fn reboot(acpi: Option<&AcpiInfo>) {
    cpu::disable_interrupts();
    if let Some((reg, value)) = acpi.and_then(|a| a.fadt).and_then(|f| f.reset) {
        kprintln!("power: reboot via ACPI reset register");
        match reg.space {
            SPACE_IO => {
                if let Ok(port) = u16::try_from(reg.address) {
                    // SAFETY: firmware-described reset register; writing the
                    // reset value is its documented purpose.
                    unsafe { cpu::outb(port, value) };
                }
            }
            SPACE_MEMORY => {
                if let Some(Ok(va)) = mm::with(|m| m.map_mmio(reg.address)) {
                    // SAFETY: uncached mapping of the firmware-described
                    // reset register.
                    unsafe { (va as *mut u8).write_volatile(value) };
                }
            }
            _ => {}
        }
        spin_ms(100);
    }
    kprintln!("power: reboot via port 0xCF9");
    // SAFETY: 0xCF9 is the chipset reset control register on PC platforms.
    unsafe { cpu::outb(0xCF9, 0x06) };
    spin_ms(100);
    kprintln!("power: reboot via keyboard controller");
    for _ in 0..100_000 {
        // SAFETY: legacy 8042 status port; reading has no side effects.
        if unsafe { cpu::inb(0x64) } & 2 == 0 {
            break;
        }
    }
    // SAFETY: 8042 command 0xFE pulses the CPU reset line.
    unsafe { cpu::outb(0x64, 0xFE) };
    spin_ms(100);
}

/// Powers off through ACPI S5. Returns an error if the platform lacks the
/// information or did not power off.
pub fn shutdown(acpi: Option<&AcpiInfo>) -> Result<(), &'static str> {
    cpu::disable_interrupts();
    let fadt = acpi.and_then(|a| a.fadt).ok_or("no FADT")?;
    let (slp_a, slp_b) = acpi.and_then(|a| a.s5).ok_or("no \\_S5 object in DSDT")?;
    let pm1a = fadt.pm1a_control.ok_or("no PM1a control block")?;
    // SAFETY: firmware-described PM1a control port; reading has no side effects.
    let acpi_mode = unsafe { cpu::inw(pm1a) } & SCI_EN != 0;
    if !acpi_mode && let Some(smi) = fadt.smi_command.filter(|_| fadt.acpi_enable != 0) {
        // SAFETY: writing ACPI_ENABLE to SMI_CMD is the ACPI-defined way to
        // switch the platform into ACPI mode.
        unsafe { cpu::outb(smi, fadt.acpi_enable) };
        for _ in 0..300 {
            // SAFETY: as above.
            if unsafe { cpu::inw(pm1a) } & SCI_EN != 0 {
                break;
            }
            spin_ms(1);
        }
    }
    kprintln!("power: entering ACPI S5 (SLP_TYP {slp_a}/{slp_b})");
    // SAFETY: SLP_TYP|SLP_EN on PM1a/PM1b requests the S5 state; nothing
    // runs afterwards on success.
    unsafe { cpu::outw(pm1a, (u16::from(slp_a) << 10) | SLP_EN) };
    if let Some(pm1b) = fadt.pm1b_control {
        // SAFETY: as above.
        unsafe { cpu::outw(pm1b, (u16::from(slp_b) << 10) | SLP_EN) };
    }
    spin_ms(500);
    Err("platform did not enter S5")
}

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;
/// CMOS bytes used by the lifecycle test (top of the 128-byte bank, not
/// used by OVMF/QEMU for configuration).
const CMOS_MAGIC_REG: u8 = 0x7E;
const CMOS_COUNT_REG: u8 = 0x7F;
const CMOS_MAGIC: u8 = 0xC0;

fn cmos_read(reg: u8) -> u8 {
    // SAFETY: RTC/CMOS index and data ports; the kernel owns them.
    unsafe { cpu::outb(CMOS_INDEX, reg) };
    // SAFETY: as above.
    unsafe { cpu::inb(CMOS_DATA) }
}

fn cmos_write(reg: u8, value: u8) {
    // SAFETY: as in `cmos_read`; only the two test bytes are written.
    unsafe { cpu::outb(CMOS_INDEX, reg) };
    // SAFETY: as above.
    unsafe { cpu::outb(CMOS_DATA, value) };
}

/// Increments and returns the persistent boot counter (1 on first boot).
pub fn bump_boot_counter() -> u8 {
    let count = if cmos_read(CMOS_MAGIC_REG) == CMOS_MAGIC { cmos_read(CMOS_COUNT_REG) } else { 0 };
    let next = count.saturating_add(1);
    cmos_write(CMOS_MAGIC_REG, CMOS_MAGIC);
    cmos_write(CMOS_COUNT_REG, next);
    next
}

/// Clears the persistent boot counter.
pub fn clear_boot_counter() {
    cmos_write(CMOS_MAGIC_REG, 0);
    cmos_write(CMOS_COUNT_REG, 0);
}
