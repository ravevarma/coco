//! coco boot protocol (ABI between `coco-boot` and the coco kernel).
//!
//! Everything the loader hands to the kernel is described here as plain
//! `#[repr(C)]` data. Enumerations are transported as raw integers and decoded
//! with checked conversions, so a corrupt or malicious value can never produce
//! an invalid Rust enum. The kernel must call [`BootInfo::validate_header`] before
//! trusting any field.
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

pub mod cmdline;
pub mod layout;
pub mod memory;
pub mod syscall;

pub use memory::{MemoryKind, MemoryRegion, RegionError};

/// `b"cocoBOOT"` read as a little-endian `u64`.
pub const BOOT_INFO_MAGIC: u64 = u64::from_le_bytes(*b"cocoBOOT");
/// Bumped on every incompatible change to [`BootInfo`].
pub const BOOT_INFO_VERSION: u32 = 2;
/// Upper bound on memory regions accepted by the kernel.
pub const MAX_MEMORY_REGIONS: u64 = 1024;
/// Upper bound on the command-line length in bytes.
pub const MAX_CMDLINE_LEN: u64 = 1024;
/// Upper bound on the initial user program image.
pub const MAX_INIT_IMAGE: u64 = 16 << 20;

/// Pixel layouts the loader may report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    /// No linear framebuffer is available (serial-only boot).
    None = 0,
    /// 32-bit pixels, byte order R, G, B, reserved.
    Rgbx = 1,
    /// 32-bit pixels, byte order B, G, R, reserved.
    Bgrx = 2,
}

impl PixelFormat {
    /// Decodes a raw value; unknown values yield `None`.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::Rgbx),
            2 => Some(Self::Bgrx),
            _ => None,
        }
    }
}

/// Linear framebuffer description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FramebufferInfo {
    /// Physical base address.
    pub phys_base: u64,
    /// Kernel virtual address (inside the direct map).
    pub virt_base: u64,
    /// Size of the mapped framebuffer in bytes.
    pub byte_len: u64,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Pixels per scan line (>= width).
    pub stride: u32,
    /// Raw [`PixelFormat`].
    pub format: u32,
}

impl FramebufferInfo {
    /// A framebuffer descriptor meaning "no framebuffer".
    pub const NONE: Self = Self {
        phys_base: 0,
        virt_base: 0,
        byte_len: 0,
        width: 0,
        height: 0,
        stride: 0,
        format: PixelFormat::None as u32,
    };
}

/// Loader feature flags reported in [`BootInfo::flags`].
pub mod flags {
    /// EFER.NXE was enabled; non-executable mappings are enforced.
    pub const NX_ENABLED: u64 = 1 << 0;
    /// CR0.WP was enabled; read-only mappings are enforced in ring 0.
    pub const WRITE_PROTECT: u64 = 1 << 1;
    /// The lower half still contains the temporary identity map.
    pub const IDENTITY_MAPPED: u64 = 1 << 2;
}

/// Handoff block passed (by direct-map virtual address) in `rdi` to the kernel.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct BootInfo {
    /// Must equal [`BOOT_INFO_MAGIC`].
    pub magic: u64,
    /// Must equal [`BOOT_INFO_VERSION`].
    pub version: u32,
    /// `size_of::<BootInfo>()` as seen by the loader.
    pub header_size: u32,
    /// Virtual address of the [`MemoryRegion`] array.
    pub memory_regions: u64,
    /// Number of valid entries.
    pub memory_region_count: u64,
    /// Linear framebuffer, or [`FramebufferInfo::NONE`].
    pub framebuffer: FramebufferInfo,
    /// Virtual address of the ASCII command line (not NUL terminated).
    pub cmdline: u64,
    /// Length of the command line in bytes.
    pub cmdline_len: u64,
    /// Physical address of the ACPI 2.0+ RSDP, or 0 if not found.
    pub acpi_rsdp: u64,
    /// Lowest mapped address of the boot stack (a guard page lies below it).
    pub stack_bottom: u64,
    /// One past the highest address of the boot stack.
    pub stack_top: u64,
    /// Base of the physical-memory direct map.
    pub direct_map_base: u64,
    /// Bytes of physical address space covered by the direct map.
    pub direct_map_len: u64,
    /// TSC when the loader started.
    pub tsc_loader_start: u64,
    /// TSC immediately before jumping to the kernel.
    pub tsc_handoff: u64,
    /// See [`flags`].
    pub flags: u64,
    /// Virtual address (direct map) of the initial user program ELF image,
    /// or 0 if none was loaded.
    pub init_image: u64,
    /// Length of the initial user program image in bytes.
    pub init_image_len: u64,
}

/// Reasons a [`BootInfo`] is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootInfoError {
    /// Wrong magic number.
    BadMagic,
    /// Unsupported protocol version.
    BadVersion(u32),
    /// Header size does not match this kernel's structure.
    BadHeaderSize(u32),
    /// Region array pointer is null, misaligned or outside the direct map.
    BadRegionPointer,
    /// Too many regions.
    TooManyRegions(u64),
    /// A memory region failed validation.
    Region(RegionError),
    /// Framebuffer description is inconsistent.
    BadFramebuffer,
    /// Command line pointer/length invalid.
    BadCmdline,
    /// Command line contains a non-printable or non-ASCII byte.
    CmdlineNotAscii,
    /// Stack bounds inconsistent.
    BadStack,
    /// Direct map description inconsistent with the fixed layout.
    BadDirectMap,
    /// Initial program image pointer/length invalid.
    BadInitImage,
}

impl BootInfo {
    /// Validates the header fields that must be checked before any pointer
    /// in this structure may be dereferenced.
    ///
    /// # Errors
    /// Returns the first inconsistency found.
    pub fn validate_header(&self) -> Result<(), BootInfoError> {
        if self.magic != BOOT_INFO_MAGIC {
            return Err(BootInfoError::BadMagic);
        }
        if self.version != BOOT_INFO_VERSION {
            return Err(BootInfoError::BadVersion(self.version));
        }
        if self.header_size as usize != core::mem::size_of::<Self>() {
            return Err(BootInfoError::BadHeaderSize(self.header_size));
        }
        if self.direct_map_base != layout::DIRECT_MAP_BASE
            || self.direct_map_len == 0
            || self.direct_map_len > layout::DIRECT_MAP_MAX_LEN
            || !self.direct_map_len.is_multiple_of(layout::PAGE_SIZE)
        {
            return Err(BootInfoError::BadDirectMap);
        }
        if self.memory_region_count > MAX_MEMORY_REGIONS {
            return Err(BootInfoError::TooManyRegions(self.memory_region_count));
        }
        let region_bytes = self
            .memory_region_count
            .checked_mul(core::mem::size_of::<MemoryRegion>() as u64)
            .ok_or(BootInfoError::BadRegionPointer)?;
        if self.memory_region_count == 0
            || !self.memory_regions.is_multiple_of(core::mem::align_of::<MemoryRegion>() as u64)
            || !self.in_direct_map(self.memory_regions, region_bytes)
        {
            return Err(BootInfoError::BadRegionPointer);
        }
        if self.cmdline_len > MAX_CMDLINE_LEN
            || (self.cmdline_len > 0 && !self.in_direct_map(self.cmdline, self.cmdline_len))
        {
            return Err(BootInfoError::BadCmdline);
        }
        if self.init_image_len > MAX_INIT_IMAGE
            || (self.init_image_len > 0 && !self.in_direct_map(self.init_image, self.init_image_len))
        {
            return Err(BootInfoError::BadInitImage);
        }
        if self.stack_bottom >= self.stack_top
            || !self.stack_bottom.is_multiple_of(layout::PAGE_SIZE)
            || !self.stack_top.is_multiple_of(layout::PAGE_SIZE)
            || self.stack_bottom < layout::KERNEL_STACK_REGION
        {
            return Err(BootInfoError::BadStack);
        }
        self.validate_framebuffer()
    }

    /// Returns true when `[addr, addr + len)` lies inside the direct map.
    #[must_use]
    pub fn in_direct_map(&self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        let Some(map_end) = self.direct_map_base.checked_add(self.direct_map_len) else {
            return false;
        };
        addr >= self.direct_map_base && end <= map_end
    }

    fn validate_framebuffer(&self) -> Result<(), BootInfoError> {
        let fb = &self.framebuffer;
        match PixelFormat::from_raw(fb.format) {
            None => Err(BootInfoError::BadFramebuffer),
            Some(PixelFormat::None) => Ok(()),
            Some(PixelFormat::Rgbx | PixelFormat::Bgrx) => {
                let needed = u64::from(fb.stride)
                    .checked_mul(u64::from(fb.height))
                    .and_then(|p| p.checked_mul(4))
                    .ok_or(BootInfoError::BadFramebuffer)?;
                let expected_virt =
                    self.direct_map_base.checked_add(fb.phys_base).ok_or(BootInfoError::BadFramebuffer)?;
                if fb.width == 0
                    || fb.height == 0
                    || fb.stride < fb.width
                    || fb.byte_len < needed
                    || !fb.phys_base.is_multiple_of(4)
                    || fb.virt_base != expected_virt
                    || !self.in_direct_map(fb.virt_base, fb.byte_len)
                {
                    return Err(BootInfoError::BadFramebuffer);
                }
                Ok(())
            }
        }
    }

    /// Validates the command line bytes (printable ASCII only).
    ///
    /// # Errors
    /// [`BootInfoError::CmdlineNotAscii`] on any other byte.
    pub fn validate_cmdline(bytes: &[u8]) -> Result<&str, BootInfoError> {
        if bytes.len() as u64 > MAX_CMDLINE_LEN {
            return Err(BootInfoError::BadCmdline);
        }
        if !bytes.iter().all(|&b| b == b' ' || b.is_ascii_graphic()) {
            return Err(BootInfoError::CmdlineNotAscii);
        }
        core::str::from_utf8(bytes).map_err(|_| BootInfoError::CmdlineNotAscii)
    }

    /// Validates the memory region array (sorted, non-overlapping, known kinds,
    /// page aligned, inside the physical address space).
    ///
    /// # Errors
    /// [`BootInfoError::Region`] describing the first bad entry.
    pub fn validate_regions(regions: &[MemoryRegion]) -> Result<(), BootInfoError> {
        memory::validate_sorted(regions).map_err(BootInfoError::Region)
    }
}

#[cfg(test)]
mod tests;
