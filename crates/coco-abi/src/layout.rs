//! Fixed kernel virtual-address layout (x86_64, 4-level paging).
//!
//! | Range start                | PML4 slot | Purpose                          |
//! |----------------------------|-----------|----------------------------------|
//! | `0x0000_0000_0000_0000`    | 0..256    | user space (identity map at boot) |
//! | `0xFFFF_8000_0000_0000`    | 256..320  | direct map of physical memory     |
//! | `0xFFFF_FE00_0000_0000`    | 508       | kernel stacks (guard-page spaced) |
//! | `0xFFFF_FFFF_8000_0000`    | 511       | kernel image (`-mcmodel=kernel`)  |

/// Base page size.
pub const PAGE_SIZE: u64 = 4096;
/// Large page size used by the direct map.
pub const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;
/// Highest physical address width supported by 4-level paging (52 bits).
pub const MAX_PHYS_ADDR: u64 = 1 << 52;
/// Start of the direct map: `virt = DIRECT_MAP_BASE + phys`.
pub const DIRECT_MAP_BASE: u64 = 0xFFFF_8000_0000_0000;
/// Maximum span of the direct map (64 PML4 slots = 32 TiB).
pub const DIRECT_MAP_MAX_LEN: u64 = 64 * (1 << 39);
/// Region reserved for kernel stacks.
pub const KERNEL_STACK_REGION: u64 = 0xFFFF_FE00_0000_0000;
/// Boot stack size (bytes, excluding the guard page).
pub const BOOT_STACK_SIZE: u64 = 64 * 1024;
/// Address of the boot stack's guard page (left unmapped).
pub const BOOT_STACK_GUARD: u64 = KERNEL_STACK_REGION;
/// Lowest mapped boot-stack address.
pub const BOOT_STACK_BOTTOM: u64 = KERNEL_STACK_REGION + PAGE_SIZE;
/// One past the top of the boot stack.
pub const BOOT_STACK_TOP: u64 = BOOT_STACK_BOTTOM + BOOT_STACK_SIZE;
/// Lowest address the kernel image may be linked at.
pub const KERNEL_IMAGE_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Lowest user-space address a program may be linked at (4 MiB; the
/// first pages stay unmapped to catch null dereferences).
pub const USER_BASE: u64 = 0x40_0000;
/// One past the highest user-space address coco hands out.
pub const USER_TOP: u64 = 0x0000_7FFF_FFFF_F000;
/// Top of the initial user stack.
pub const USER_STACK_TOP: u64 = 0x0000_7FFF_FFF0_0000;
/// Size of the initial user stack (a guard page lies below it).
pub const USER_STACK_SIZE: u64 = 64 * 1024;
/// Kernel stack size per thread (a guard page lies below each).
pub const KERNEL_THREAD_STACK_SIZE: u64 = 32 * 1024;

/// Converts a physical address to its direct-map virtual address.
#[must_use]
pub const fn phys_to_virt(phys: u64) -> Option<u64> {
    if phys >= DIRECT_MAP_MAX_LEN {
        return None;
    }
    DIRECT_MAP_BASE.checked_add(phys)
}

/// Converts a direct-map virtual address back to a physical address.
#[must_use]
pub const fn virt_to_phys(virt: u64) -> Option<u64> {
    if virt < DIRECT_MAP_BASE {
        return None;
    }
    let phys = virt.wrapping_sub(DIRECT_MAP_BASE);
    if phys >= DIRECT_MAP_MAX_LEN { None } else { Some(phys) }
}
