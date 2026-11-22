//! Minimal hand-written UEFI bindings (UEFI 2.10 layouts).
//!
//! Only the services and protocols coco-boot uses are typed; unused
//! function-pointer slots are kept as `usize` placeholders so the table
//! layouts stay correct. All tables are accessed through raw pointers that
//! the firmware guarantees valid until `ExitBootServices`.

use core::ffi::c_void;

/// Opaque firmware handle.
pub type Handle = *mut c_void;
/// `EFI_STATUS`.
pub type Status = usize;

/// Success.
pub const SUCCESS: Status = 0;
const ERROR_BIT: usize = 1 << (usize::BITS - 1);
/// `EFI_LOAD_ERROR`.
pub const LOAD_ERROR: Status = ERROR_BIT | 1;
/// `EFI_INVALID_PARAMETER` (stale map key in `ExitBootServices`).
pub const INVALID_PARAMETER: Status = ERROR_BIT | 2;
/// `EFI_BUFFER_TOO_SMALL`.
pub const BUFFER_TOO_SMALL: Status = ERROR_BIT | 5;
/// `EFI_NOT_FOUND`.
pub const NOT_FOUND: Status = ERROR_BIT | 14;

/// `EFI_SYSTEM_TABLE` signature ("IBI SYST").
pub const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;
/// `EFI_BOOT_SERVICES` signature ("BOOTSERV").
pub const BOOT_SERVICES_SIGNATURE: u64 = 0x5652_4553_544F_4F42;

/// `AllocateAnyPages`.
pub const ALLOCATE_ANY_PAGES: u32 = 0;
/// `EfiLoaderData`.
pub const LOADER_DATA: u32 = 2;
/// `EFI_FILE_MODE_READ`.
pub const FILE_MODE_READ: u64 = 1;

/// `EFI_GUID`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

/// `EFI_LOADED_IMAGE_PROTOCOL_GUID`.
pub const LOADED_IMAGE_GUID: Guid = Guid(0x5B1B_31A1, 0x9562, 0x11D2, [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID`.
pub const SIMPLE_FS_GUID: Guid = Guid(0x964E_5B22, 0x6459, 0x11D2, [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID`.
pub const GOP_GUID: Guid = Guid(0x9042_A9DE, 0x23DC, 0x4A38, [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A]);
/// `EFI_ACPI_20_TABLE_GUID`.
pub const ACPI2_GUID: Guid = Guid(0x8868_E871, 0xE4F1, 0x11D3, [0xBC, 0x22, 0x00, 0x80, 0xC7, 0x3C, 0x88, 0x81]);
/// `ACPI_TABLE_GUID` (ACPI 1.0).
pub const ACPI1_GUID: Guid = Guid(0xEB9D_2D30, 0x2D88, 0x11D3, [0x9A, 0x16, 0x00, 0x90, 0x27, 0x3F, 0xC1, 0x4D]);

/// `EFI_TABLE_HEADER`.
#[derive(Debug)]
#[repr(C)]
pub struct TableHeader {
    /// Table signature.
    pub signature: u64,
    /// Specification revision.
    pub revision: u32,
    /// Header size.
    pub header_size: u32,
    /// CRC32 of the table.
    pub crc32: u32,
    reserved: u32,
}

/// `EFI_CONFIGURATION_TABLE`.
#[derive(Debug)]
#[repr(C)]
pub struct ConfigurationTable {
    /// Vendor GUID.
    pub guid: Guid,
    /// Table address.
    pub table: *mut c_void,
}

/// `EFI_SYSTEM_TABLE`.
#[derive(Debug)]
#[repr(C)]
pub struct SystemTable {
    /// Header.
    pub hdr: TableHeader,
    /// Vendor string (UCS-2).
    pub firmware_vendor: *const u16,
    /// Vendor revision.
    pub firmware_revision: u32,
    console_in_handle: Handle,
    con_in: *mut c_void,
    console_out_handle: Handle,
    /// Console output.
    pub con_out: *mut SimpleTextOutput,
    standard_error_handle: Handle,
    std_err: *mut c_void,
    runtime_services: *mut c_void,
    /// Boot services.
    pub boot_services: *mut BootServices,
    /// Number of configuration tables.
    pub number_of_table_entries: usize,
    /// Configuration tables.
    pub configuration_table: *mut ConfigurationTable,
}

/// `EFI_BOOT_SERVICES` (prefix up to `LocateProtocol`).
#[derive(Debug)]
#[repr(C)]
pub struct BootServices {
    /// Header.
    pub hdr: TableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    /// `AllocatePages`.
    pub allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    free_pages: usize,
    /// `GetMemoryMap`.
    pub get_memory_map: unsafe extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    allocate_pool: usize,
    free_pool: usize,
    create_event: usize,
    set_timer: usize,
    wait_for_event: usize,
    signal_event: usize,
    close_event: usize,
    check_event: usize,
    install_protocol_interface: usize,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface: usize,
    /// `HandleProtocol`.
    pub handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    reserved: usize,
    register_protocol_notify: usize,
    locate_handle: usize,
    locate_device_path: usize,
    install_configuration_table: usize,
    load_image: usize,
    start_image: usize,
    exit: usize,
    unload_image: usize,
    /// `ExitBootServices`.
    pub exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,
    get_next_monotonic_count: usize,
    stall: usize,
    /// `SetWatchdogTimer`.
    pub set_watchdog_timer: unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    connect_controller: usize,
    disconnect_controller: usize,
    open_protocol: usize,
    close_protocol: usize,
    open_protocol_information: usize,
    protocols_per_handle: usize,
    locate_handle_buffer: usize,
    /// `LocateProtocol`.
    pub locate_protocol: unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

/// `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL` (prefix).
#[derive(Debug)]
#[repr(C)]
pub struct SimpleTextOutput {
    reset: usize,
    /// `OutputString`.
    pub output_string: unsafe extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
}

/// `EFI_LOADED_IMAGE_PROTOCOL` (prefix).
#[derive(Debug)]
#[repr(C)]
pub struct LoadedImage {
    /// Revision.
    pub revision: u32,
    /// Parent image.
    pub parent_handle: Handle,
    /// System table.
    pub system_table: *mut SystemTable,
    /// Device the image was loaded from.
    pub device_handle: Handle,
}

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`.
#[derive(Debug)]
#[repr(C)]
pub struct SimpleFileSystem {
    /// Revision.
    pub revision: u64,
    /// `OpenVolume`.
    pub open_volume: unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

/// `EFI_FILE_PROTOCOL` (prefix up to `SetPosition`).
#[derive(Debug)]
#[repr(C)]
pub struct FileProtocol {
    /// Revision.
    pub revision: u64,
    /// `Open`.
    pub open: unsafe extern "efiapi" fn(*mut FileProtocol, *mut *mut FileProtocol, *const u16, u64, u64) -> Status,
    /// `Close`.
    pub close: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
    delete: usize,
    /// `Read`.
    pub read: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut u8) -> Status,
    write: usize,
    /// `GetPosition`.
    pub get_position: unsafe extern "efiapi" fn(*mut FileProtocol, *mut u64) -> Status,
    /// `SetPosition`.
    pub set_position: unsafe extern "efiapi" fn(*mut FileProtocol, u64) -> Status,
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL`.
#[derive(Debug)]
#[repr(C)]
pub struct GraphicsOutput {
    query_mode: usize,
    set_mode: usize,
    blt: usize,
    /// Current mode.
    pub mode: *mut GraphicsMode,
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE`.
#[derive(Debug)]
#[repr(C)]
pub struct GraphicsMode {
    /// Number of modes.
    pub max_mode: u32,
    /// Current mode number.
    pub mode: u32,
    /// Current mode information.
    pub info: *mut GraphicsModeInfo,
    /// Size of `info`.
    pub size_of_info: usize,
    /// Framebuffer physical base.
    pub frame_buffer_base: u64,
    /// Framebuffer size in bytes.
    pub frame_buffer_size: usize,
}

/// `EFI_GRAPHICS_OUTPUT_MODE_INFORMATION`.
#[derive(Debug)]
#[repr(C)]
pub struct GraphicsModeInfo {
    /// Version.
    pub version: u32,
    /// Width.
    pub horizontal_resolution: u32,
    /// Height.
    pub vertical_resolution: u32,
    /// `EFI_GRAPHICS_PIXEL_FORMAT`: 0 RGBX, 1 BGRX, 2 bitmask, 3 blt-only.
    pub pixel_format: u32,
    /// Masks for format 2.
    pub pixel_information: [u32; 4],
    /// Stride.
    pub pixels_per_scan_line: u32,
}

/// `EFI_MEMORY_DESCRIPTOR` (descriptors are `desc_size` apart, which may
/// exceed `size_of::<MemoryDescriptor>()`).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MemoryDescriptor {
    /// `EFI_MEMORY_TYPE`.
    pub ty: u32,
    /// Physical start.
    pub phys_start: u64,
    /// Virtual start (unused).
    pub virt_start: u64,
    /// Number of 4 KiB pages.
    pub pages: u64,
    /// Attribute bits.
    pub attribute: u64,
}

/// Encodes an ASCII string as a NUL-terminated UCS-2 array at compile time.
#[must_use]
pub const fn ucs2<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() && i + 1 < N {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}
