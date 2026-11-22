//! Fuzzes ACPI discovery: the input is firmware memory mapped at 0x10000
//! with the RSDP at its start. Parsing must never panic, read outside the
//! provided memory, or report structures exceeding their fixed bounds.
#![no_main]
use coco_acpi::{AcpiError, PhysMemory, MAX_CPUS, MAX_IOAPICS};
use libfuzzer_sys::fuzz_target;

const BASE: u64 = 0x10000;

struct Mem<'a>(&'a [u8], u32);

impl PhysMemory for Mem<'_> {
    fn read(&mut self, phys: u64, out: &mut [u8]) -> Result<(), AcpiError> {
        self.1 += 1;
        assert!(self.1 < 2_000_000, "unbounded number of reads");
        let off = phys.checked_sub(BASE).ok_or(AcpiError::Unreadable(phys))? as usize;
        let src = self.0.get(off..off.checked_add(out.len()).ok_or(AcpiError::Overflow)?).ok_or(AcpiError::Unreadable(phys))?;
        out.copy_from_slice(src);
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let mut mem = Mem(data, 0);
    if let Ok(info) = coco_acpi::parse(&mut mem, BASE) {
        if let Some(m) = info.madt {
            assert!(m.cpus().len() <= MAX_CPUS && m.ioapics().len() <= MAX_IOAPICS);
        }
        if let Some((a, b)) = info.s5 {
            assert!(a < 8 && b < 8);
        }
        assert!(info.rejected_tables <= info.table_count);
    }
});
