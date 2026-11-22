//! ACPI discovery: feeds firmware memory to the host-tested `coco-acpi`
//! parser, mapping table pages read-only into the direct map on demand.

use coco_abi::layout::{DIRECT_MAP_BASE, PAGE_SIZE};
use coco_acpi::{AcpiError, AcpiInfo, PhysMemory};

use crate::mm;

/// Reads physical memory through the direct map.
struct KernelPhys;

impl PhysMemory for KernelPhys {
    fn read(&mut self, phys: u64, out: &mut [u8]) -> Result<(), AcpiError> {
        let len = out.len() as u64;
        let end = phys.checked_add(len).ok_or(AcpiError::Overflow)?;
        let mut page = phys & !(PAGE_SIZE - 1);
        while page < end {
            mm::with(|m| m.ensure_direct_mapped(page))
                .ok_or(AcpiError::Unreadable(phys))?
                .map_err(|_| AcpiError::Unreadable(page))?;
            page += PAGE_SIZE;
        }
        let src = DIRECT_MAP_BASE.wrapping_add(phys) as *const u8;
        // SAFETY: every page of `[phys, end)` was just ensured mapped
        // (read-only) in the direct map; `out` is a distinct Rust buffer.
        unsafe { core::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len()) };
        Ok(())
    }
}

/// Parses the ACPI tables from the RSDP the loader found.
///
/// # Errors
/// Parser errors (the kernel continues without ACPI-derived features).
pub fn init(rsdp: u64) -> Result<AcpiInfo, AcpiError> {
    coco_acpi::parse(&mut KernelPhys, rsdp)
}
