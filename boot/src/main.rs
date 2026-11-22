//! coco-boot: the native UEFI loader for the coco kernel.
//!
//! Boot flow (all steps before `ExitBootServices` report errors on the
//! firmware console and return an error status to the firmware):
//!
//! 1. Read `\coco\kernel.elf` (and optional `\coco\cmdline.txt`) from the
//!    volume the loader was started from.
//! 2. Validate the ELF image with `coco-elf` (W^X, window, bounds).
//! 3. Copy segments into `KERNEL_IMAGE` pages and build fresh 4-level page
//!    tables: kernel image (per-segment permissions), boot stack with an
//!    unmapped guard page, a direct map of RAM and the framebuffer, and a
//!    temporary executable identity alias of the direct map for the switch.
//! 4. Allocate the boot-info block, exit boot services, convert and
//!    normalize the final memory map, enable NXE/WP, switch CR3 and call the
//!    kernel entry with the boot-info pointer in `rdi`.
#![no_std]
#![no_main]

mod uefi;

use core::convert::Infallible;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use coco_abi::layout::{
    BOOT_STACK_BOTTOM, BOOT_STACK_SIZE, BOOT_STACK_TOP, DIRECT_MAP_BASE, DIRECT_MAP_MAX_LEN, KERNEL_IMAGE_BASE,
    LARGE_PAGE_SIZE, PAGE_SIZE, phys_to_virt,
};
use coco_abi::memory::{kind_from_uefi, normalize, uefi_types};
use coco_abi::{
    BOOT_INFO_MAGIC, BOOT_INFO_VERSION, BootInfo, FramebufferInfo, MAX_CMDLINE_LEN, MAX_MEMORY_REGIONS, MemoryKind,
    MemoryRegion, PixelFormat, RegionError, flags,
};
use coco_arch::cpu;
use coco_elf::{ElfError, Executable, Policy};
use coco_memory::paging::{MapError, Mapper, PageFlags, PageSize, PhysAccess, TableFrameSource};
use uefi::{BootServices, FileProtocol, Handle, MemoryDescriptor, Status, SystemTable};

const KERNEL_PATH: [u16; 17] = uefi::ucs2("\\coco\\kernel.elf");
const CMDLINE_PATH: [u16; 18] = uefi::ucs2("\\coco\\cmdline.txt");
/// Largest kernel file the loader will read.
const MAX_KERNEL_FILE: u64 = 32 << 20;
/// Upper bound on the loaded kernel's memory footprint.
const MAX_KERNEL_MEMORY: u64 = 64 << 20;
/// Highest page-aligned address a kernel segment may end at.
const KERNEL_WINDOW_END: u64 = 0xFFFF_FFFF_FFFF_F000;
/// Extra memory-map descriptors reserved for allocations made after sizing.
const MAP_SLACK: usize = 32;

/// Firmware console used by the panic handler while boot services are live.
static CONSOLE: AtomicPtr<uefi::SimpleTextOutput> = AtomicPtr::new(ptr::null_mut());

/// Loader failure reasons (reported through `Debug`).
#[derive(Debug)]
#[allow(dead_code, reason = "payloads are read through the Debug impl")]
enum LoaderError {
    Firmware(&'static str, Status),
    Elf(ElfError),
    Map(MapError),
    Regions(RegionError),
    KernelTooLarge(u64),
    MissingKernel,
    TooManyRegions(usize),
    BadCmdline,
    FiveLevelPaging,
    NotDirectMapped(u64),
}

impl From<MapError> for LoaderError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}

fn check(what: &'static str, status: Status) -> Result<(), LoaderError> {
    if status == uefi::SUCCESS { Ok(()) } else { Err(LoaderError::Firmware(what, status)) }
}

/// Live boot-services access. Consumed by [`Firmware::exit_boot_services`],
/// so no firmware call can be made after exit (typestate).
struct Firmware {
    image: Handle,
    st: *mut SystemTable,
    bs: *mut BootServices,
}

/// Writes UCS-2 text to the firmware console.
struct ConOut(*mut uefi::SimpleTextOutput);

impl Write for ConOut {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.0.is_null() {
            return Ok(());
        }
        let mut buf = [0u16; 130];
        let mut n = 0;
        let flush = |buf: &mut [u16; 130], n: &mut usize| {
            if let Some(slot) = buf.get_mut(*n) {
                *slot = 0;
            }
            // SAFETY: `self.0` is the firmware's ConOut protocol (non-null,
            // valid until ExitBootServices, after which CONSOLE is cleared
            // and no ConOut is constructed).
            let proto = unsafe { &*self.0 };
            // SAFETY: firmware function of that protocol; `buf` is
            // NUL-terminated UCS-2.
            unsafe { (proto.output_string)(self.0, buf.as_ptr()) };
            *n = 0;
        };
        for ch in s.chars() {
            if ch == '\n'
                && let Some(slot) = buf.get_mut(n)
            {
                *slot = u16::from(b'\r');
                n += 1;
            }
            let c = if ch.is_ascii() { ch as u16 } else { u16::from(b'?') };
            if let Some(slot) = buf.get_mut(n) {
                *slot = c;
                n += 1;
            }
            if n >= 126 {
                flush(&mut buf, &mut n);
            }
        }
        if n > 0 {
            flush(&mut buf, &mut n);
        }
        Ok(())
    }
}

macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = writeln!(ConOut(CONSOLE.load(Ordering::Relaxed)), $($arg)*);
    }};
}

impl Firmware {
    fn bs(&self) -> &BootServices {
        // SAFETY: `bs` was validated (non-null, signature) in `efi_main` and
        // stays valid until `exit_boot_services` consumes `self`.
        unsafe { &*self.bs }
    }

    /// Allocates `pages` zeroed pages of memory type `ty`.
    fn alloc_pages(&self, ty: u32, pages: u64) -> Result<u64, LoaderError> {
        let count = usize::try_from(pages).map_err(|_| LoaderError::Firmware("AllocatePages size", 0))?;
        let mut addr = 0u64;
        // SAFETY: valid boot-services table; `addr` is a valid out pointer.
        let status = unsafe { (self.bs().allocate_pages)(uefi::ALLOCATE_ANY_PAGES, ty, count, &mut addr) };
        check("AllocatePages", status)?;
        let bytes = usize::try_from(pages.saturating_mul(PAGE_SIZE)).unwrap_or(usize::MAX);
        // SAFETY: the firmware just allocated `pages` pages at `addr`
        // exclusively for us; UEFI identity-maps all memory.
        unsafe { ptr::write_bytes(addr as *mut u8, 0, bytes) };
        Ok(addr)
    }

    fn handle_protocol<T>(&self, handle: Handle, guid: &uefi::Guid) -> Result<*mut T, LoaderError> {
        let mut iface: *mut c_void = ptr::null_mut();
        // SAFETY: valid table, GUID and out pointer.
        let status = unsafe { (self.bs().handle_protocol)(handle, guid, &mut iface) };
        check("HandleProtocol", status)?;
        if iface.is_null() {
            return Err(LoaderError::Firmware("HandleProtocol returned null", 0));
        }
        Ok(iface.cast())
    }

    fn locate_protocol<T>(&self, guid: &uefi::Guid) -> Result<*mut T, LoaderError> {
        let mut iface: *mut c_void = ptr::null_mut();
        // SAFETY: valid table, GUID and out pointer.
        let status = unsafe { (self.bs().locate_protocol)(guid, ptr::null_mut(), &mut iface) };
        check("LocateProtocol", status)?;
        if iface.is_null() {
            return Err(LoaderError::Firmware("LocateProtocol returned null", 0));
        }
        Ok(iface.cast())
    }

    /// Opens the root directory of the volume the loader was loaded from.
    fn open_boot_volume(&self) -> Result<File, LoaderError> {
        let loaded: *mut uefi::LoadedImage = self.handle_protocol(self.image, &uefi::LOADED_IMAGE_GUID)?;
        // SAFETY: firmware returned a valid LoadedImage protocol pointer.
        let device = unsafe { (*loaded).device_handle };
        let fs: *mut uefi::SimpleFileSystem = self.handle_protocol(device, &uefi::SIMPLE_FS_GUID)?;
        let mut root: *mut FileProtocol = ptr::null_mut();
        // SAFETY: the firmware returned a valid protocol pointer.
        let proto = unsafe { &*fs };
        // SAFETY: protocol function with valid `this` and out pointers.
        check("OpenVolume", unsafe { (proto.open_volume)(fs, &mut root) })?;
        if root.is_null() {
            return Err(LoaderError::Firmware("OpenVolume returned null", 0));
        }
        Ok(File(root))
    }

    /// Reads a whole file into fresh `LoaderData` pages. `Ok(None)` if absent.
    fn read_file(&self, root: &File, path: &[u16], max: u64) -> Result<Option<(u64, u64)>, LoaderError> {
        let mut file: *mut FileProtocol = ptr::null_mut();
        // SAFETY: `root` is a valid open directory (File invariant); `path`
        // is NUL-terminated; `file` is a valid out pointer.
        let status = unsafe { (root.proto().open)(root.0, &mut file, path.as_ptr(), uefi::FILE_MODE_READ, 0) };
        if status == uefi::NOT_FOUND {
            return Ok(None);
        }
        check("File.Open", status)?;
        if file.is_null() {
            return Err(LoaderError::Firmware("File.Open returned null", 0));
        }
        let file = File(file);
        let result = self.read_open_file(&file, max);
        file.close();
        result.map(Some)
    }

    fn read_open_file(&self, file: &File, max: u64) -> Result<(u64, u64), LoaderError> {
        let mut size = 0u64;
        let f = file.proto();
        // SAFETY: valid open file (File invariant). u64::MAX = end of file.
        check("File.SetPosition(end)", unsafe { (f.set_position)(file.0, u64::MAX) })?;
        // SAFETY: valid open file and out pointer.
        check("File.GetPosition", unsafe { (f.get_position)(file.0, &mut size) })?;
        // SAFETY: valid open file.
        check("File.SetPosition(0)", unsafe { (f.set_position)(file.0, 0) })?;
        if size > max {
            return Err(LoaderError::KernelTooLarge(size));
        }
        let pages = size.div_ceil(PAGE_SIZE).max(1);
        let buf = self.alloc_pages(uefi::LOADER_DATA, pages)?;
        let mut done = 0u64;
        while done < size {
            let mut chunk = usize::try_from(size - done).unwrap_or(usize::MAX);
            // SAFETY: `buf + done .. buf + size` lies within the allocation.
            let status = unsafe { (f.read)(file.0, &mut chunk, (buf + done) as *mut u8) };
            check("File.Read", status)?;
            if chunk == 0 {
                return Err(LoaderError::Firmware("File.Read: unexpected end of file", 0));
            }
            done = done.saturating_add(chunk as u64).min(size);
        }
        Ok((buf, size))
    }

    /// Returns `(bytes_needed, descriptor_size)` for the current memory map.
    fn memory_map_size(&self) -> Result<(usize, usize), LoaderError> {
        let (mut size, mut key, mut desc_size, mut version) = (0usize, 0usize, 0usize, 0u32);
        // SAFETY: a zero-size query is explicitly allowed by the spec.
        let status =
            unsafe { (self.bs().get_memory_map)(&mut size, ptr::null_mut(), &mut key, &mut desc_size, &mut version) };
        if status != uefi::BUFFER_TOO_SMALL {
            return Err(LoaderError::Firmware("GetMemoryMap(size)", status));
        }
        if desc_size < core::mem::size_of::<MemoryDescriptor>() {
            return Err(LoaderError::Firmware("GetMemoryMap descriptor size", desc_size));
        }
        Ok((size, desc_size))
    }

    /// Fills `buf` with the memory map; returns `(len, key, desc_size)`.
    fn memory_map(&self, buf: u64, cap: usize) -> Result<(usize, usize, usize), LoaderError> {
        let (mut size, mut key, mut desc_size, mut version) = (cap, 0usize, 0usize, 0u32);
        // SAFETY: `buf` points to `cap` writable bytes we own.
        let status =
            unsafe { (self.bs().get_memory_map)(&mut size, buf as *mut u8, &mut key, &mut desc_size, &mut version) };
        check("GetMemoryMap", status)?;
        if desc_size < core::mem::size_of::<MemoryDescriptor>() {
            return Err(LoaderError::Firmware("GetMemoryMap descriptor size", desc_size));
        }
        Ok((size, key, desc_size))
    }

    /// Fetches the final memory map and exits boot services. On success the
    /// firmware is gone; returns `(map_len, desc_size)`.
    fn exit_boot_services(self, buf: u64, cap: usize) -> Result<(usize, usize), (Self, LoaderError)> {
        let mut last = uefi::SUCCESS;
        // The spec allows ExitBootServices to fail once if the map changed
        // (e.g. a timer event allocated memory); retry with a fresh map.
        for _ in 0..2 {
            let (len, key, desc_size) = match self.memory_map(buf, cap) {
                Ok(v) => v,
                Err(e) => return Err((self, e)),
            };
            // SAFETY: valid image handle and current map key. No firmware
            // output may happen between GetMemoryMap and this call.
            last = unsafe { (self.bs().exit_boot_services)(self.image, key) };
            if last == uefi::SUCCESS {
                CONSOLE.store(ptr::null_mut(), Ordering::Relaxed);
                return Ok((len, desc_size));
            }
            if last != uefi::INVALID_PARAMETER {
                break;
            }
        }
        Err((self, LoaderError::Firmware("ExitBootServices", last)))
    }

    fn find_acpi_rsdp(&self) -> u64 {
        // SAFETY: the system table was validated in `efi_main`.
        let st = unsafe { &*self.st };
        let mut acpi1 = 0u64;
        if st.configuration_table.is_null() {
            return 0;
        }
        // SAFETY: the firmware guarantees `number_of_table_entries` valid,
        // immutable entries at `configuration_table` (checked non-null).
        let tables = unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
        for entry in tables {
            if entry.guid == uefi::ACPI2_GUID {
                return entry.table as u64;
            }
            if entry.guid == uefi::ACPI1_GUID {
                acpi1 = entry.table as u64;
            }
        }
        acpi1
    }

    fn framebuffer(&self) -> FramebufferInfo {
        let Ok(gop) = self.locate_protocol::<uefi::GraphicsOutput>(&uefi::GOP_GUID) else {
            log!("coco-boot: no graphics output protocol; serial-only boot");
            return FramebufferInfo::NONE;
        };
        // SAFETY: valid GOP pointer from the firmware.
        let gop = unsafe { &*gop };
        if gop.mode.is_null() {
            return FramebufferInfo::NONE;
        }
        // SAFETY: firmware-owned mode structure, valid while boot services run.
        let mode = unsafe { &*gop.mode };
        if mode.info.is_null() {
            return FramebufferInfo::NONE;
        }
        // SAFETY: as above.
        let info = unsafe { &*mode.info };
        let format = match info.pixel_format {
            0 => PixelFormat::Rgbx,
            1 => PixelFormat::Bgrx,
            other => {
                log!("coco-boot: unsupported GOP pixel format {other}; serial-only boot");
                return FramebufferInfo::NONE;
            }
        };
        FramebufferInfo {
            phys_base: mode.frame_buffer_base,
            virt_base: phys_to_virt(mode.frame_buffer_base).unwrap_or(0),
            byte_len: mode.frame_buffer_size as u64,
            width: info.horizontal_resolution,
            height: info.vertical_resolution,
            stride: info.pixels_per_scan_line,
            format: format as u32,
        }
    }
}

/// An open `EFI_FILE_PROTOCOL` handle.
///
/// Invariant: the pointer is non-null and the file is open; [`File::close`]
/// consumes the handle so it cannot be used afterwards.
struct File(*mut FileProtocol);

impl File {
    fn proto(&self) -> &FileProtocol {
        // SAFETY: type invariant (valid open file protocol instance).
        unsafe { &*self.0 }
    }

    fn close(self) {
        // SAFETY: valid open file; `self` is consumed so it is closed once.
        let _ = unsafe { (self.proto().close)(self.0) };
    }
}

/// Physical memory is identity-mapped by UEFI while boot services run.
struct Identity;

// SAFETY: every table the loader's mapper touches is a page we allocated
// (`PAGE_TABLES` type) and UEFI identity-maps all memory, so `phys` is a
// valid, exclusively owned, aligned 4 KiB table at address `phys`.
unsafe impl PhysAccess for Identity {
    fn table(&self, phys: u64) -> *mut u64 {
        phys as *mut u64
    }
}

/// Supplies zeroed page-table frames from the firmware.
struct TableFrames<'a> {
    fw: &'a Firmware,
    allocated: u64,
}

impl TableFrameSource for TableFrames<'_> {
    fn allocate_table(&mut self) -> Option<u64> {
        let frame = self.fw.alloc_pages(uefi_types::PAGE_TABLES, 1).ok()?;
        self.allocated = self.allocated.saturating_add(1);
        Some(frame)
    }
}

/// Maps `[start, end)` into the direct map, using 2 MiB pages where the
/// range allows and 4 KiB pages at unaligned edges. Pages already mapped
/// (shared with a neighbouring region) are skipped.
fn direct_map_range(
    mapper: &mut Mapper<Identity>,
    frames: &mut TableFrames<'_>,
    start: u64,
    end: u64,
) -> Result<(), LoaderError> {
    let flags = PageFlags::WRITABLE;
    let mut phys = start & !(PAGE_SIZE - 1);
    while phys < end {
        let virt = DIRECT_MAP_BASE + phys;
        let large_fits =
            phys.is_multiple_of(LARGE_PAGE_SIZE) && phys.checked_add(LARGE_PAGE_SIZE).is_some_and(|e| e <= end);
        if large_fits && mapper.translate(virt).is_none() {
            match mapper.map(virt, phys, PageSize::Size2M, flags, frames) {
                Ok(flush) => {
                    flush.ignore(); // tables are not live yet
                    phys += LARGE_PAGE_SIZE;
                    continue;
                }
                Err(MapError::SizeConflict) => {} // a 4K table exists; fall through
                Err(e) => return Err(e.into()),
            }
        }
        if mapper.translate(virt).is_none() {
            mapper.map(virt, phys, PageSize::Size4K, flags, frames)?.ignore();
        }
        phys += PAGE_SIZE;
    }
    Ok(())
}

/// Should this memory type be part of the direct map? MMIO and reserved
/// ranges are excluded (drivers map them later with the right caching).
fn direct_mapped(kind: MemoryKind) -> bool {
    !matches!(kind, MemoryKind::Mmio | MemoryKind::Reserved | MemoryKind::Unusable)
}

fn descriptors(buf: u64, len: usize, desc_size: usize) -> impl Iterator<Item = MemoryDescriptor> {
    (0..len / desc_size).map(move |i| {
        // SAFETY: `buf..buf+len` was filled by GetMemoryMap with `len /
        // desc_size` descriptors of stride `desc_size`; read_unaligned
        // because the stride need not preserve alignment.
        unsafe { ptr::read_unaligned((buf as usize + i * desc_size) as *const MemoryDescriptor) }
    })
}

struct Prepared {
    cr3: u64,
    entry: u64,
    boot_info_phys: u64,
    regions_phys: u64,
    region_cap: usize,
    map_buf: u64,
    map_cap: usize,
    nx: bool,
}

fn prepare(fw: &Firmware, tsc_start: u64) -> Result<Prepared, LoaderError> {
    if cpu::read_cr4() & cpu::CR4_LA57 != 0 {
        return Err(LoaderError::FiveLevelPaging);
    }
    let nx = cpu::has_nx();
    let root = fw.open_boot_volume()?;
    let (kfile, klen) = fw.read_file(&root, &KERNEL_PATH, MAX_KERNEL_FILE)?.ok_or(LoaderError::MissingKernel)?;
    log!("coco-boot: kernel.elf {} bytes", klen);
    let cmdline = fw.read_file(&root, &CMDLINE_PATH, MAX_CMDLINE_LEN)?;
    root.close();

    // SAFETY: `read_file` filled `klen` bytes at `kfile` in pages we own;
    // nothing else writes them while this slice lives.
    let image = unsafe { core::slice::from_raw_parts(kfile as *const u8, klen as usize) };
    let policy =
        Policy { window_start: KERNEL_IMAGE_BASE, window_end: KERNEL_WINDOW_END, max_total_bytes: MAX_KERNEL_MEMORY };
    let exe = Executable::parse(image, &policy).map_err(LoaderError::Elf)?;

    let pml4 = fw.alloc_pages(uefi_types::PAGE_TABLES, 1)?;
    let mut frames = TableFrames { fw, allocated: 1 };
    // SAFETY: `pml4` is a zeroed page we exclusively own; `Identity`
    // satisfies `PhysAccess` while boot services run.
    let mut mapper = unsafe { Mapper::new(pml4, Identity) };

    // Kernel segments.
    for seg in exe.segments() {
        let pages = seg.page_count();
        let phys = fw.alloc_pages(uefi_types::KERNEL_IMAGE, pages)?;
        let data = exe.file_data(seg);
        // SAFETY: destination lies inside the `pages` pages just allocated
        // (page_offset + file_size <= pages * 4096 by ELF validation) and
        // does not overlap the source file buffer.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), (phys + seg.page_offset()) as *mut u8, data.len()) };
        let mut flags = PageFlags::EMPTY;
        if seg.writable() {
            flags = flags | PageFlags::WRITABLE;
        }
        if !seg.executable() && nx {
            flags = flags | PageFlags::NO_EXECUTE;
        }
        for p in 0..pages {
            mapper
                .map(seg.page_start() + p * PAGE_SIZE, phys + p * PAGE_SIZE, PageSize::Size4K, flags, &mut frames)?
                .ignore();
        }
        log!(
            "coco-boot: segment {:#x} +{:#x} {}{}{}",
            seg.vaddr,
            seg.mem_size,
            'r',
            if seg.writable() { 'w' } else { '-' },
            if seg.executable() { 'x' } else { '-' }
        );
    }

    // Boot stack, with the guard page below it left unmapped.
    let stack_pages = BOOT_STACK_SIZE / PAGE_SIZE;
    let stack = fw.alloc_pages(uefi_types::KERNEL_STACK, stack_pages)?;
    let stack_flags = if nx { PageFlags::WRITABLE | PageFlags::NO_EXECUTE } else { PageFlags::WRITABLE };
    for p in 0..stack_pages {
        mapper
            .map(BOOT_STACK_BOTTOM + p * PAGE_SIZE, stack + p * PAGE_SIZE, PageSize::Size4K, stack_flags, &mut frames)?
            .ignore();
    }

    // Direct map of RAM-like regions (snapshot of the current map; every
    // later allocation comes from conventional memory already covered).
    let (size, desc_size) = fw.memory_map_size()?;
    let snap_cap = size + MAP_SLACK * desc_size;
    let snap = fw.alloc_pages(uefi::LOADER_DATA, (snap_cap as u64).div_ceil(PAGE_SIZE))?;
    let (snap_len, _, snap_desc) = fw.memory_map(snap, snap_cap)?;
    let mut map_end = 0u64;
    for d in descriptors(snap, snap_len, snap_desc) {
        if d.pages == 0 || !direct_mapped(kind_from_uefi(d.ty)) {
            continue;
        }
        let end = d.pages.checked_mul(PAGE_SIZE).and_then(|l| d.phys_start.checked_add(l));
        let end = end.filter(|&e| e <= DIRECT_MAP_MAX_LEN).ok_or(LoaderError::Regions(RegionError::OutOfRange(0)))?;
        direct_map_range(&mut mapper, &mut frames, d.phys_start, end)?;
        map_end = map_end.max(end);
    }
    let fb = fw.framebuffer();
    if fb.format != PixelFormat::None as u32 {
        let end = fb.phys_base.checked_add(fb.byte_len).filter(|&e| e <= DIRECT_MAP_MAX_LEN);
        let end = end.ok_or(LoaderError::Regions(RegionError::OutOfRange(0)))?;
        direct_map_range(&mut mapper, &mut frames, fb.phys_base, end)?;
        map_end = map_end.max(end);
        log!("coco-boot: framebuffer {}x{} stride {} at {:#x}", fb.width, fb.height, fb.stride, fb.phys_base);
    }
    let direct_map_len = map_end.div_ceil(LARGE_PAGE_SIZE) * LARGE_PAGE_SIZE;

    // Boot-info block: header, region array, command line.
    let (size, desc_size) = fw.memory_map_size()?;
    let region_cap = size / desc_size + MAP_SLACK;
    if region_cap as u64 > MAX_MEMORY_REGIONS {
        return Err(LoaderError::TooManyRegions(region_cap));
    }
    let header = core::mem::size_of::<BootInfo>() as u64;
    let regions_bytes = (region_cap * core::mem::size_of::<MemoryRegion>()) as u64;
    let info_pages = (header + regions_bytes + MAX_CMDLINE_LEN).div_ceil(PAGE_SIZE);
    let info = fw.alloc_pages(uefi_types::BOOT_INFO, info_pages)?;
    let regions_phys = info + header;
    let cmdline_phys = regions_phys + regions_bytes;
    let mut cmdline_len = 0u64;
    if let Some((buf, len)) = cmdline {
        // SAFETY: `read_file` filled `len` bytes at `buf`.
        let raw = unsafe { core::slice::from_raw_parts(buf as *const u8, len as usize) };
        // Tolerate a trailing newline from text editors; reject anything else
        // that is not printable ASCII.
        let trimmed = raw.strip_suffix(b"\r\n").or_else(|| raw.strip_suffix(b"\n")).unwrap_or(raw);
        BootInfo::validate_cmdline(trimmed).map_err(|_| LoaderError::BadCmdline)?;
        // SAFETY: the cmdline area has MAX_CMDLINE_LEN bytes and
        // `trimmed.len() <= MAX_CMDLINE_LEN` (validated above).
        unsafe { ptr::copy_nonoverlapping(trimmed.as_ptr(), cmdline_phys as *mut u8, trimmed.len()) };
        cmdline_len = trimmed.len() as u64;
    }
    for addr in [info, info + info_pages * PAGE_SIZE - 1, stack, pml4] {
        if mapper.translate(DIRECT_MAP_BASE + addr).is_none() {
            return Err(LoaderError::NotDirectMapped(addr));
        }
    }
    let rsdp = fw.find_acpi_rsdp();

    let mut bflags = flags::WRITE_PROTECT | flags::IDENTITY_MAPPED;
    if nx {
        bflags |= flags::NX_ENABLED;
    }
    let boot_info = BootInfo {
        magic: BOOT_INFO_MAGIC,
        version: BOOT_INFO_VERSION,
        header_size: header as u32,
        memory_regions: DIRECT_MAP_BASE + regions_phys,
        memory_region_count: 0, // filled after ExitBootServices
        framebuffer: fb,
        cmdline: DIRECT_MAP_BASE + cmdline_phys,
        cmdline_len,
        acpi_rsdp: rsdp,
        stack_bottom: BOOT_STACK_BOTTOM,
        stack_top: BOOT_STACK_TOP,
        direct_map_base: DIRECT_MAP_BASE,
        direct_map_len,
        tsc_loader_start: tsc_start,
        tsc_handoff: 0,
        flags: bflags,
    };
    // SAFETY: `info` is a fresh, page-aligned, zeroed BOOT_INFO allocation
    // large enough for the header.
    unsafe { ptr::write(info as *mut BootInfo, boot_info) };

    // Identity alias of the direct map (lower half), executable, so the
    // instructions after `mov cr3` still resolve. NX goes on the direct
    // map's PML4 entries instead of its leaves, which the alias shares.
    let slots = direct_map_len.div_ceil(1 << 39) as usize;
    for i in 0..slots {
        let entry = mapper.root_entry(256 + i);
        // SAFETY: entry `256 + i` is a valid level-3 table built above; the
        // alias shares it (no ownership transfer) and tables are not live.
        unsafe { mapper.set_root_entry(i, entry & !PageFlags::NO_EXECUTE.bits()) };
        if nx {
            // SAFETY: same entry with NX added; tables are not live.
            unsafe { mapper.set_root_entry(256 + i, entry | PageFlags::NO_EXECUTE.bits()) };
        }
    }

    // Memory-map buffer for ExitBootServices (allocated last; slack covers
    // the descriptors this allocation itself may add).
    let (size, desc_size) = fw.memory_map_size()?;
    let map_cap = size + MAP_SLACK * desc_size;
    let map_buf = fw.alloc_pages(uefi::LOADER_DATA, (map_cap as u64).div_ceil(PAGE_SIZE))?;

    log!(
        "coco-boot: page tables {} pages, direct map {} MiB, entry {:#x}, nx={}",
        frames.allocated,
        direct_map_len >> 20,
        exe.entry(),
        nx
    );
    Ok(Prepared { cr3: pml4, entry: exe.entry(), boot_info_phys: info, regions_phys, region_cap, map_buf, map_cap, nx })
}

/// Fatal error once the firmware console may no longer be used: report on
/// COM1 (if present) and halt.
fn fatal_after_exit(msg: fmt::Arguments<'_>) -> ! {
    // SAFETY: boot services are gone (or unusable after a failed
    // ExitBootServices), so nothing else drives the UART.
    if let Some(mut port) = unsafe { coco_arch::serial::SerialPort::init(coco_arch::serial::COM1) } {
        let _ = writeln!(port, "coco-boot: FATAL (firmware unavailable): {msg}");
    }
    cpu::halt_forever()
}

fn run(fw: Firmware, tsc_start: u64) -> Result<Infallible, (Firmware, LoaderError)> {
    let p = match prepare(&fw, tsc_start) {
        Ok(p) => p,
        Err(e) => return Err((fw, e)),
    };
    let (map_len, desc_size) = match fw.exit_boot_services(p.map_buf, p.map_cap) {
        Ok(v) => v,
        // After a failed ExitBootServices the spec allows only
        // GetMemoryMap/ExitBootServices, so the firmware console must not
        // be used: report on COM1 and halt instead of returning.
        Err((_fw, e)) => {
            CONSOLE.store(ptr::null_mut(), Ordering::Relaxed);
            fatal_after_exit(format_args!("exiting boot services: {e:?}"))
        }
    };
    // ---- Boot services are gone: no allocation, no firmware console. ----

    // SAFETY: `regions_phys` has room for `region_cap` regions inside the
    // zeroed BOOT_INFO allocation, exclusively ours; MemoryRegion is POD.
    let regions = unsafe { core::slice::from_raw_parts_mut(p.regions_phys as *mut MemoryRegion, p.region_cap) };
    let mut count = 0usize;
    for d in descriptors(p.map_buf, map_len, desc_size) {
        if d.pages == 0 {
            continue;
        }
        let Some(slot) = regions.get_mut(count) else {
            fatal_after_exit(format_args!("memory map exceeds capacity {}", p.region_cap))
        };
        *slot = MemoryRegion::new(d.phys_start, d.pages, kind_from_uefi(d.ty));
        count += 1;
    }
    let Some(used) = regions.get_mut(..count) else { fatal_after_exit(format_args!("region slice")) };
    let count = match normalize(used) {
        Ok(c) => c,
        Err(e) => fatal_after_exit(format_args!("firmware memory map is inconsistent: {e:?}")),
    };

    if p.nx {
        // SAFETY: EFER exists on every x86_64 CPU; we run at CPL 0.
        let efer = unsafe { cpu::rdmsr(cpu::MSR_EFER) };
        // SAFETY: CPUID reported NX support; setting EFER.NXE only makes
        // bit 63 of page-table entries meaningful.
        unsafe { cpu::wrmsr(cpu::MSR_EFER, efer | cpu::EFER_NXE) };
    }
    let info = p.boot_info_phys as *mut BootInfo;
    // SAFETY: `info` holds the BootInfo written in `prepare`; we are the
    // only code running.
    unsafe {
        (*info).memory_region_count = count as u64;
    }
    let tsc = cpu::rdtsc();
    // SAFETY: as above.
    unsafe {
        (*info).tsc_handoff = tsc;
    }
    // SAFETY: the new page tables map this code (identity alias), the boot
    // stack, the kernel image and the boot info; interrupts are disabled
    // for the switch and the kernel entry never returns.
    unsafe { enter_kernel(p.cr3, BOOT_STACK_TOP, p.entry, DIRECT_MAP_BASE + p.boot_info_phys) }
}

/// Switches to the kernel address space and calls the kernel entry.
///
/// # Safety
/// `cr3` must map the current instruction stream (identity), `stack_top`
/// (16-byte aligned, writable) and `entry`; `boot_info` must be valid in the
/// new address space.
unsafe fn enter_kernel(cr3: u64, stack_top: u64, entry: u64, boot_info: u64) -> ! {
    // SAFETY: caller contract. `call` leaves rsp ≡ 8 (mod 16) at the entry
    // point, as the System V ABI requires.
    unsafe {
        core::arch::asm!(
            "cli",
            "mov rax, cr0",
            "or rax, {wp}",
            "mov cr0, rax",
            "mov cr3, r8",
            "mov rsp, r9",
            "xor ebp, ebp",
            "call r10",
            "2:",
            "cli",
            "hlt",
            "jmp 2b",
            wp = const cpu::CR0_WP,
            // Fixed registers: rax is used as scratch above, so the inputs
            // must not be allocated to it.
            in("r8") cr3,
            in("r9") stack_top,
            in("r10") entry,
            in("rdi") boot_info,
            options(noreturn),
        )
    }
}

/// UEFI entry point.
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    let tsc_start = cpu::rdtsc();
    if st.is_null() {
        return uefi::INVALID_PARAMETER;
    }
    // SAFETY: the firmware passes a valid system table pointer (checked
    // non-null above); its signature is verified before further use.
    let table = unsafe { &*st };
    let (sig, bs, con_out) = (table.hdr.signature, table.boot_services, table.con_out);
    if sig != uefi::SYSTEM_TABLE_SIGNATURE || bs.is_null() {
        return uefi::INVALID_PARAMETER;
    }
    // SAFETY: `bs` is non-null and comes from a valid system table.
    if unsafe { (*bs).hdr.signature } != uefi::BOOT_SERVICES_SIGNATURE {
        return uefi::INVALID_PARAMETER;
    }
    CONSOLE.store(con_out, Ordering::Relaxed);
    let fw = Firmware { image, st, bs };
    // Disable the firmware's 5-minute watchdog (spec-defined call).
    // SAFETY: valid boot-services table.
    let _ = unsafe { (fw.bs().set_watchdog_timer)(0, 0, 0, ptr::null()) };
    log!("coco-boot {} starting", env!("CARGO_PKG_VERSION"));
    match run(fw, tsc_start) {
        Ok(never) => match never {},
        Err((_fw, e)) => {
            log!("coco-boot: FATAL: {:?}", e);
            uefi::LOAD_ERROR
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let con = CONSOLE.load(Ordering::Relaxed);
    if con.is_null() {
        fatal_after_exit(format_args!("loader panic: {info}"));
    }
    let _ = writeln!(ConOut(con), "coco-boot: PANIC: {info}");
    cpu::halt_forever()
}
