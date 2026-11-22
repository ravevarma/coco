//! Strict ELF64 parser used by `coco-boot` to load the kernel.
//!
//! The parser treats its input as untrusted: every offset and size is
//! bounds-checked with checked arithmetic, and the image is rejected unless it
//! satisfies the coco loading policy:
//!
//! * ELF64, little-endian, x86_64, `ET_EXEC`, System V ABI.
//! * At most [`MAX_SEGMENTS`] `PT_LOAD` segments, none writable *and*
//!   executable (W^X), none sharing a page with another segment.
//! * All segments inside the caller-supplied virtual window.
//! * No `PT_INTERP`, `PT_DYNAMIC` or `PT_TLS` (static image only).
//! * The entry point lies inside an executable segment.
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

/// Maximum number of loadable segments accepted.
pub const MAX_SEGMENTS: usize = 16;
/// Maximum number of program headers accepted.
pub const MAX_PHDRS: u16 = 32;

const PAGE: u64 = 4096;
const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_TLS: u32 = 7;

/// Segment permission flags (ELF `p_flags`).
pub const PF_X: u32 = 1;
/// Writable.
pub const PF_W: u32 = 2;
/// Readable.
pub const PF_R: u32 = 4;

/// Why an image was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Input shorter than the structure being read.
    Truncated,
    /// Not an ELF file.
    BadMagic,
    /// Not ELF64 / little-endian / version 1.
    BadIdent,
    /// Not an x86_64 executable.
    WrongMachine,
    /// Not `ET_EXEC`.
    NotExecutable,
    /// Header sizes do not match ELF64.
    BadHeaderSize,
    /// Zero or too many program headers.
    BadPhdrCount,
    /// A header or segment range overflows or lies outside the file.
    OutOfBounds,
    /// More than [`MAX_SEGMENTS`] loadable segments.
    TooManySegments,
    /// `p_filesz > p_memsz` or empty segment.
    BadSegmentSize,
    /// Segment both writable and executable.
    WriteExecute,
    /// Segment outside the permitted virtual window.
    OutsideWindow,
    /// Two segments share a page.
    Overlap,
    /// Total memory size exceeds the caller's limit.
    TooLarge,
    /// Dynamic linking, interpreters or TLS are not supported.
    Unsupported,
    /// Entry point is not inside an executable segment.
    BadEntry,
    /// Unaligned or non power-of-two `p_align`, or vaddr/offset incongruent.
    BadAlignment,
    /// No loadable segment.
    NoSegments,
}

/// A validated `PT_LOAD` segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Virtual address of the first byte.
    pub vaddr: u64,
    /// Bytes in memory (>= `file_size`).
    pub mem_size: u64,
    /// Offset of the file data.
    pub file_offset: u64,
    /// Bytes present in the file.
    pub file_size: u64,
    /// `PF_*` permission bits.
    pub flags: u32,
}

impl Segment {
    /// First page-aligned virtual address covered.
    #[must_use]
    pub const fn page_start(&self) -> u64 {
        self.vaddr & !(PAGE - 1)
    }

    /// Number of 4 KiB pages spanned. Validation guarantees no overflow.
    #[must_use]
    pub const fn page_count(&self) -> u64 {
        let end = self.vaddr.saturating_add(self.mem_size);
        let end_page = end.saturating_add(PAGE - 1) & !(PAGE - 1);
        end_page.saturating_sub(self.page_start()) / PAGE
    }

    /// Offset of `vaddr` within its first page.
    #[must_use]
    pub const fn page_offset(&self) -> u64 {
        self.vaddr & (PAGE - 1)
    }

    /// Writable?
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.flags & PF_W != 0
    }

    /// Executable?
    #[must_use]
    pub const fn executable(&self) -> bool {
        self.flags & PF_X != 0
    }
}

/// Loading policy supplied by the caller.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Lowest permitted virtual address (inclusive).
    pub window_start: u64,
    /// Highest permitted virtual address (exclusive).
    pub window_end: u64,
    /// Upper bound on the sum of all segments' page-rounded sizes.
    pub max_total_bytes: u64,
}

/// A parsed and validated executable.
#[derive(Clone, Copy, Debug)]
pub struct Executable<'a> {
    data: &'a [u8],
    entry: u64,
    segments: [Segment; MAX_SEGMENTS],
    count: usize,
}

fn read_u16(d: &[u8], off: usize) -> Result<u16, ElfError> {
    let end = off.checked_add(2).ok_or(ElfError::Truncated)?;
    let b: [u8; 2] = d.get(off..end).ok_or(ElfError::Truncated)?.try_into().map_err(|_| ElfError::Truncated)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(d: &[u8], off: usize) -> Result<u32, ElfError> {
    let end = off.checked_add(4).ok_or(ElfError::Truncated)?;
    let b: [u8; 4] = d.get(off..end).ok_or(ElfError::Truncated)?.try_into().map_err(|_| ElfError::Truncated)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(d: &[u8], off: usize) -> Result<u64, ElfError> {
    let end = off.checked_add(8).ok_or(ElfError::Truncated)?;
    let b: [u8; 8] = d.get(off..end).ok_or(ElfError::Truncated)?.try_into().map_err(|_| ElfError::Truncated)?;
    Ok(u64::from_le_bytes(b))
}

const EMPTY: Segment = Segment { vaddr: 0, mem_size: 0, file_offset: 0, file_size: 0, flags: 0 };

impl<'a> Executable<'a> {
    /// Parses and validates `data` against `policy`.
    ///
    /// # Errors
    /// The first policy or format violation found.
    pub fn parse(data: &'a [u8], policy: &Policy) -> Result<Self, ElfError> {
        if data.len() < EHDR_SIZE {
            return Err(ElfError::Truncated);
        }
        if data.get(0..4) != Some(b"\x7fELF".as_slice()) {
            return Err(ElfError::BadMagic);
        }
        // EI_CLASS=2 (64-bit), EI_DATA=1 (LE), EI_VERSION=1, EI_OSABI=0 (SysV)
        if data.get(4..8) != Some([2u8, 1, 1, 0].as_slice()) {
            return Err(ElfError::BadIdent);
        }
        if read_u16(data, 16)? != 2 {
            return Err(ElfError::NotExecutable);
        }
        if read_u16(data, 18)? != 62 {
            return Err(ElfError::WrongMachine);
        }
        if read_u32(data, 20)? != 1 {
            return Err(ElfError::BadIdent);
        }
        let entry = read_u64(data, 24)?;
        let phoff = read_u64(data, 32)?;
        if usize::from(read_u16(data, 52)?) != EHDR_SIZE || usize::from(read_u16(data, 54)?) != PHDR_SIZE {
            return Err(ElfError::BadHeaderSize);
        }
        let phnum = read_u16(data, 56)?;
        if phnum == 0 || phnum > MAX_PHDRS {
            return Err(ElfError::BadPhdrCount);
        }
        let table_len = u64::from(phnum).checked_mul(PHDR_SIZE as u64).ok_or(ElfError::OutOfBounds)?;
        let table_end = phoff.checked_add(table_len).ok_or(ElfError::OutOfBounds)?;
        if table_end > data.len() as u64 {
            return Err(ElfError::OutOfBounds);
        }

        let mut exe = Self { data, entry, segments: [EMPTY; MAX_SEGMENTS], count: 0 };
        let mut total: u64 = 0;
        for i in 0..usize::from(phnum) {
            let base = usize::try_from(phoff)
                .ok()
                .and_then(|p| i.checked_mul(PHDR_SIZE).and_then(|o| p.checked_add(o)))
                .ok_or(ElfError::OutOfBounds)?;
            let field = |off: usize| base.checked_add(off).ok_or(ElfError::OutOfBounds);
            let p_type = read_u32(data, base)?;
            match p_type {
                PT_LOAD => {}
                PT_DYNAMIC | PT_INTERP | PT_TLS => return Err(ElfError::Unsupported),
                _ => continue,
            }
            let seg = Segment {
                flags: read_u32(data, field(4)?)?,
                file_offset: read_u64(data, field(8)?)?,
                vaddr: read_u64(data, field(16)?)?,
                file_size: read_u64(data, field(32)?)?,
                mem_size: read_u64(data, field(40)?)?,
            };
            let align = read_u64(data, field(48)?)?;
            Self::check_segment(&seg, align, data.len() as u64, policy)?;
            let bytes = seg.page_count().checked_mul(PAGE).ok_or(ElfError::TooLarge)?;
            total = total.checked_add(bytes).ok_or(ElfError::TooLarge)?;
            if total > policy.max_total_bytes {
                return Err(ElfError::TooLarge);
            }
            for other in exe.segments() {
                let a_end = seg.page_start().checked_add(seg.page_count().checked_mul(PAGE).ok_or(ElfError::Overlap)?);
                let b_end =
                    other.page_start().checked_add(other.page_count().checked_mul(PAGE).ok_or(ElfError::Overlap)?);
                let (Some(a_end), Some(b_end)) = (a_end, b_end) else {
                    return Err(ElfError::Overlap);
                };
                if seg.page_start() < b_end && other.page_start() < a_end {
                    return Err(ElfError::Overlap);
                }
            }
            let slot = exe.segments.get_mut(exe.count).ok_or(ElfError::TooManySegments)?;
            *slot = seg;
            exe.count = exe.count.checked_add(1).ok_or(ElfError::TooManySegments)?;
        }
        if exe.count == 0 {
            return Err(ElfError::NoSegments);
        }
        let entry_ok = exe.segments().iter().any(|s| {
            s.executable() && entry >= s.vaddr && s.vaddr.checked_add(s.mem_size).is_some_and(|end| entry < end)
        });
        if !entry_ok {
            return Err(ElfError::BadEntry);
        }
        Ok(exe)
    }

    fn check_segment(seg: &Segment, align: u64, file_len: u64, policy: &Policy) -> Result<(), ElfError> {
        if seg.mem_size == 0 || seg.file_size > seg.mem_size {
            return Err(ElfError::BadSegmentSize);
        }
        if seg.writable() && seg.executable() {
            return Err(ElfError::WriteExecute);
        }
        let file_end = seg.file_offset.checked_add(seg.file_size).ok_or(ElfError::OutOfBounds)?;
        if file_end > file_len {
            return Err(ElfError::OutOfBounds);
        }
        if align > 1 && (!align.is_power_of_two() || seg.vaddr.checked_rem(align) != seg.file_offset.checked_rem(align))
        {
            return Err(ElfError::BadAlignment);
        }
        let mem_end = seg.vaddr.checked_add(seg.mem_size).ok_or(ElfError::OutsideWindow)?;
        // Page-rounded end must also be representable.
        mem_end.checked_add(PAGE - 1).ok_or(ElfError::OutsideWindow)?;
        if seg.vaddr < policy.window_start || mem_end > policy.window_end {
            return Err(ElfError::OutsideWindow);
        }
        Ok(())
    }

    /// Entry point virtual address.
    #[must_use]
    pub const fn entry(&self) -> u64 {
        self.entry
    }

    /// Validated loadable segments, in program-header order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        self.segments.get(..self.count).unwrap_or(&[])
    }

    /// File bytes backing `seg` (validated at parse time).
    #[must_use]
    pub fn file_data(&self, seg: &Segment) -> &'a [u8] {
        let start = usize::try_from(seg.file_offset).unwrap_or(usize::MAX);
        let len = usize::try_from(seg.file_size).unwrap_or(usize::MAX);
        start.checked_add(len).and_then(|end| self.data.get(start..end)).unwrap_or(&[])
    }

    /// Sum of page-rounded segment sizes in bytes.
    #[must_use]
    pub fn total_pages(&self) -> u64 {
        self.segments().iter().map(Segment::page_count).fold(0u64, u64::saturating_add)
    }
}

#[cfg(test)]
mod tests;
