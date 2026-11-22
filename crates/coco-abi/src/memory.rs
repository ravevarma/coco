//! Physical memory map entries and normalization.

use crate::layout::{MAX_PHYS_ADDR, PAGE_SIZE};

/// Classification of a physical memory region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MemoryKind {
    /// Free RAM the kernel may allocate.
    Usable = 1,
    /// Firmware-reserved or unknown; never touch.
    Reserved = 2,
    /// ACPI tables; reclaimable once parsed.
    AcpiReclaimable = 3,
    /// ACPI non-volatile storage; never touch.
    AcpiNvs = 4,
    /// Memory-mapped I/O.
    Mmio = 5,
    /// UEFI runtime services code/data; must stay mapped if runtime services are used.
    FirmwareRuntime = 6,
    /// Loader code/data/pool; free once the kernel no longer uses loader memory.
    BootloaderReclaimable = 7,
    /// Kernel image segments.
    KernelImage = 8,
    /// Boot stack.
    KernelStack = 9,
    /// Page tables built by the loader (in use by CR3).
    PageTables = 10,
    /// Boot information block, memory map and command line.
    BootInfo = 11,
    /// RAM reported as faulty.
    Unusable = 12,
    /// Persistent memory.
    Persistent = 13,
}

impl MemoryKind {
    /// Decodes a raw value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::Usable,
            2 => Self::Reserved,
            3 => Self::AcpiReclaimable,
            4 => Self::AcpiNvs,
            5 => Self::Mmio,
            6 => Self::FirmwareRuntime,
            7 => Self::BootloaderReclaimable,
            8 => Self::KernelImage,
            9 => Self::KernelStack,
            10 => Self::PageTables,
            11 => Self::BootInfo,
            12 => Self::Unusable,
            13 => Self::Persistent,
            _ => return None,
        })
    }

    /// Short human-readable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Usable => "usable",
            Self::Reserved => "reserved",
            Self::AcpiReclaimable => "acpi-reclaim",
            Self::AcpiNvs => "acpi-nvs",
            Self::Mmio => "mmio",
            Self::FirmwareRuntime => "fw-runtime",
            Self::BootloaderReclaimable => "loader",
            Self::KernelImage => "kernel",
            Self::KernelStack => "kstack",
            Self::PageTables => "pagetable",
            Self::BootInfo => "bootinfo",
            Self::Unusable => "bad-ram",
            Self::Persistent => "pmem",
        }
    }

    /// True for RAM the kernel will hand to its frame allocator.
    #[must_use]
    pub const fn is_allocatable(self) -> bool {
        matches!(self, Self::Usable)
    }
}

/// UEFI memory types the loader uses for its own allocations
/// (UEFI reserves `0x8000_0000..=0xFFFF_FFFF` for OS loaders).
pub mod uefi_types {
    /// Kernel image segments.
    pub const KERNEL_IMAGE: u32 = 0x8000_C0C0;
    /// Kernel boot stack.
    pub const KERNEL_STACK: u32 = 0x8000_C0C1;
    /// Page tables.
    pub const PAGE_TABLES: u32 = 0x8000_C0C2;
    /// Boot info, memory map, command line.
    pub const BOOT_INFO: u32 = 0x8000_C0C3;
}

/// Maps a UEFI `EFI_MEMORY_TYPE` to a coco [`MemoryKind`].
///
/// Boot-services memory becomes usable after `ExitBootServices`. Unknown
/// types are treated as reserved (fail safe).
#[must_use]
pub const fn kind_from_uefi(uefi_type: u32) -> MemoryKind {
    match uefi_type {
        // EfiLoaderCode, EfiLoaderData
        1 | 2 => MemoryKind::BootloaderReclaimable,
        // EfiBootServicesCode, EfiBootServicesData, EfiConventionalMemory
        3 | 4 | 7 => MemoryKind::Usable,
        // EfiRuntimeServicesCode, EfiRuntimeServicesData
        5 | 6 => MemoryKind::FirmwareRuntime,
        8 => MemoryKind::Unusable,
        9 => MemoryKind::AcpiReclaimable,
        10 => MemoryKind::AcpiNvs,
        // EfiMemoryMappedIO, EfiMemoryMappedIOPortSpace
        11 | 12 => MemoryKind::Mmio,
        14 => MemoryKind::Persistent,
        uefi_types::KERNEL_IMAGE => MemoryKind::KernelImage,
        uefi_types::KERNEL_STACK => MemoryKind::KernelStack,
        uefi_types::PAGE_TABLES => MemoryKind::PageTables,
        uefi_types::BOOT_INFO => MemoryKind::BootInfo,
        // EfiReservedMemoryType (0), EfiPalCode (13), EfiUnacceptedMemoryType (15), others
        _ => MemoryKind::Reserved,
    }
}

/// One contiguous physical region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct MemoryRegion {
    /// Page-aligned physical base.
    pub base: u64,
    /// Length in 4 KiB pages (non-zero).
    pub pages: u64,
    /// Raw [`MemoryKind`].
    pub kind: u32,
    /// Must be zero.
    pub reserved: u32,
}

/// Reasons a region list is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError {
    /// Entry `index` has a base that is not page aligned.
    Misaligned(usize),
    /// Entry `index` has zero pages.
    Empty(usize),
    /// Entry `index` extends past [`MAX_PHYS_ADDR`] or overflows.
    OutOfRange(usize),
    /// Entry `index` has an unknown kind.
    UnknownKind(usize),
    /// Entry `index` is not sorted after its predecessor or overlaps it.
    Overlap(usize),
    /// Entry `index` has non-zero reserved bits.
    ReservedBits(usize),
    /// More regions than the destination can hold.
    Capacity,
}

impl MemoryRegion {
    /// Builds a region from a raw kind.
    #[must_use]
    pub const fn new(base: u64, pages: u64, kind: MemoryKind) -> Self {
        Self { base, pages, kind: kind as u32, reserved: 0 }
    }

    /// Decoded kind.
    #[must_use]
    pub const fn kind(&self) -> Option<MemoryKind> {
        MemoryKind::from_raw(self.kind)
    }

    /// Length in bytes, if it does not overflow.
    #[must_use]
    pub const fn byte_len(&self) -> Option<u64> {
        self.pages.checked_mul(PAGE_SIZE)
    }

    /// Exclusive end address, if it does not overflow.
    #[must_use]
    pub const fn end(&self) -> Option<u64> {
        match self.byte_len() {
            Some(len) => self.base.checked_add(len),
            None => None,
        }
    }

    fn check(&self, index: usize) -> Result<u64, RegionError> {
        if !self.base.is_multiple_of(PAGE_SIZE) {
            return Err(RegionError::Misaligned(index));
        }
        if self.pages == 0 {
            return Err(RegionError::Empty(index));
        }
        if self.reserved != 0 {
            return Err(RegionError::ReservedBits(index));
        }
        if self.kind().is_none() {
            return Err(RegionError::UnknownKind(index));
        }
        match self.end() {
            Some(end) if end <= MAX_PHYS_ADDR => Ok(end),
            _ => Err(RegionError::OutOfRange(index)),
        }
    }
}

/// Validates that `regions` are individually valid, sorted by base and
/// non-overlapping.
///
/// # Errors
/// The first violation found.
pub fn validate_sorted(regions: &[MemoryRegion]) -> Result<(), RegionError> {
    let mut prev_end = 0u64;
    for (i, r) in regions.iter().enumerate() {
        let end = r.check(i)?;
        if i > 0 && r.base < prev_end {
            return Err(RegionError::Overlap(i));
        }
        prev_end = end;
    }
    Ok(())
}

/// Sorts `regions` in place by base address, merges adjacent entries of the
/// same kind and verifies that no two entries overlap.
///
/// Returns the number of entries in the normalized prefix of `regions`.
/// Overlapping input is rejected rather than guessed at, because silently
/// choosing a kind for doubly-described memory could hand firmware-owned RAM
/// to the allocator.
///
/// # Errors
/// Any [`RegionError`] from validation of the normalized list.
pub fn normalize(regions: &mut [MemoryRegion]) -> Result<usize, RegionError> {
    for (i, r) in regions.iter().enumerate() {
        r.check(i)?;
    }
    // Insertion sort: no allocation, stable, and n is small (firmware maps
    // have at most a few hundred entries).
    for i in 1..regions.len() {
        let mut j = i;
        while let Some(prev_idx) = j.checked_sub(1) {
            let (Some(a), Some(b)) = (regions.get(prev_idx), regions.get(j)) else {
                break;
            };
            if a.base <= b.base {
                break;
            }
            regions.swap(prev_idx, j);
            j = prev_idx;
        }
    }
    let mut out = 0usize;
    for i in 0..regions.len() {
        let Some(&cur) = regions.get(i) else { break };
        if let Some(prev_idx) = out.checked_sub(1) {
            let Some(prev) = regions.get_mut(prev_idx) else {
                break;
            };
            let prev_end = prev.end().ok_or(RegionError::OutOfRange(prev_idx))?;
            if cur.base < prev_end {
                return Err(RegionError::Overlap(i));
            }
            if cur.base == prev_end && cur.kind == prev.kind {
                prev.pages = prev.pages.checked_add(cur.pages).ok_or(RegionError::OutOfRange(i))?;
                continue;
            }
        }
        let Some(slot) = regions.get_mut(out) else {
            return Err(RegionError::Capacity);
        };
        *slot = cur;
        out = out.saturating_add(1); // bounded by regions.len()
    }
    let normalized = regions.get(..out).ok_or(RegionError::Capacity)?;
    validate_sorted(normalized)?;
    Ok(out)
}
