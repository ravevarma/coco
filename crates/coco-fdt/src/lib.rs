//! Flattened Device Tree (DTB, devicetree specification v0.4) reader.
//!
//! The blob comes from firmware or the boot loader and is untrusted: every
//! offset and length is bounds-checked, nesting depth is limited, all
//! arithmetic is checked, and nothing is allocated. Only what coco needs at
//! boot is extracted:
//! * RAM ranges from `/memory*` nodes (`device_type = "memory"`, `reg`),
//! * reserved ranges from the memory-reservation block and from the `reg`
//!   of `/reserved-memory` children,
//! * the `reg` base of the first node whose `compatible` matches a string
//!   (e.g. a UART).
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

const MAGIC: u32 = 0xD00D_FEED;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;
/// Deepest node nesting accepted.
pub const MAX_DEPTH: usize = 16;
/// Most ranges reported per list.
pub const MAX_RANGES: usize = 16;

/// Parse errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FdtError {
    /// Blob shorter than a header field or block it declares.
    Truncated,
    /// Wrong magic number.
    BadMagic,
    /// Unsupported version.
    BadVersion,
    /// A block offset or size is inconsistent.
    BadLayout,
    /// Malformed structure block (unknown token, unbalanced nodes, too deep).
    BadStructure,
}

/// A physical address range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Range {
    /// Base address.
    pub base: u64,
    /// Length in bytes.
    pub size: u64,
}

/// A bounded list of ranges.
#[derive(Clone, Copy, Debug)]
pub struct Ranges {
    items: [Range; MAX_RANGES],
    len: usize,
    /// Ranges dropped because the list was full.
    pub dropped: usize,
}

impl Ranges {
    const fn new() -> Self {
        Self { items: [Range { base: 0, size: 0 }; MAX_RANGES], len: 0, dropped: 0 }
    }

    fn push(&mut self, r: Range) {
        if r.size == 0 {
            return;
        }
        match self.items.get_mut(self.len) {
            Some(slot) => {
                *slot = r;
                self.len = self.len.saturating_add(1);
            }
            None => self.dropped = self.dropped.saturating_add(1),
        }
    }

    /// The ranges.
    #[must_use]
    pub fn as_slice(&self) -> &[Range] {
        self.items.get(..self.len).unwrap_or(&[])
    }
}

fn be32(d: &[u8], off: usize) -> Result<u32, FdtError> {
    let end = off.checked_add(4).ok_or(FdtError::Truncated)?;
    let b: [u8; 4] = d.get(off..end).ok_or(FdtError::Truncated)?.try_into().map_err(|_| FdtError::Truncated)?;
    Ok(u32::from_be_bytes(b))
}

fn be64(d: &[u8], off: usize) -> Result<u64, FdtError> {
    let end = off.checked_add(8).ok_or(FdtError::Truncated)?;
    let b: [u8; 8] = d.get(off..end).ok_or(FdtError::Truncated)?.try_into().map_err(|_| FdtError::Truncated)?;
    Ok(u64::from_be_bytes(b))
}

/// Reads a `cells`-cell big-endian number (1 or 2 cells) at `off`.
fn cells(d: &[u8], off: usize, n: u32) -> Result<u64, FdtError> {
    match n {
        1 => be32(d, off).map(u64::from),
        2 => be64(d, off),
        _ => Err(FdtError::BadStructure),
    }
}

/// Everything coco extracts from a device tree.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    /// RAM ranges.
    pub memory: Ranges,
    /// Reserved ranges (memory-reservation block and `/reserved-memory`).
    pub reserved: Ranges,
    /// `reg` base of the first node matching the requested `compatible`.
    pub device: Option<u64>,
    /// Total size of the blob (so the caller can reserve it).
    pub total_size: u32,
}

fn str_at(d: &[u8], off: usize) -> Result<&[u8], FdtError> {
    let tail = d.get(off..).ok_or(FdtError::BadLayout)?;
    let len = tail.iter().position(|&b| b == 0).ok_or(FdtError::BadLayout)?;
    tail.get(..len).ok_or(FdtError::BadLayout)
}

fn align4(x: usize) -> Result<usize, FdtError> {
    x.checked_add(3).map(|v| v & !3).ok_or(FdtError::BadLayout)
}

/// True if the NUL-separated string list `list` contains `want`.
fn list_contains(list: &[u8], want: &[u8]) -> bool {
    list.split(|&b| b == 0).any(|s| s == want)
}

#[derive(Clone, Copy, Default)]
struct Level {
    addr_cells: u32,
    size_cells: u32,
    is_memory: bool,
    in_reserved: bool,
    is_reserved_container: bool,
    compatible_match: bool,
    reg: Option<(usize, usize)>, // (offset, len) of this node's reg property
}

/// Parses `blob`, looking for a node compatible with `compatible` (pass an
/// empty slice to skip that search).
///
/// # Errors
/// Any [`FdtError`].
pub fn parse(blob: &[u8], compatible: &[u8]) -> Result<Summary, FdtError> {
    if be32(blob, 0)? != MAGIC {
        return Err(FdtError::BadMagic);
    }
    let total = be32(blob, 4)?;
    let total_usize = usize::try_from(total).map_err(|_| FdtError::BadLayout)?;
    if total_usize > blob.len() || total_usize < 40 {
        return Err(FdtError::Truncated);
    }
    let d = blob.get(..total_usize).ok_or(FdtError::Truncated)?;
    let off_struct = be32(d, 8)? as usize;
    let off_strings = be32(d, 12)? as usize;
    let off_rsvmap = be32(d, 16)? as usize;
    let version = be32(d, 20)?;
    let last_comp = be32(d, 24)?;
    if version < 16 || last_comp > 17 {
        return Err(FdtError::BadVersion);
    }
    let size_strings = be32(d, 32)? as usize;
    let size_struct = be32(d, 36)? as usize;
    let struct_end = off_struct.checked_add(size_struct).ok_or(FdtError::BadLayout)?;
    let strings_end = off_strings.checked_add(size_strings).ok_or(FdtError::BadLayout)?;
    if struct_end > d.len() || strings_end > d.len() || off_rsvmap >= d.len() {
        return Err(FdtError::BadLayout);
    }
    let strings = d.get(off_strings..strings_end).ok_or(FdtError::BadLayout)?;
    let structure = d.get(..struct_end).ok_or(FdtError::BadLayout)?;

    let mut out = Summary { memory: Ranges::new(), reserved: Ranges::new(), device: None, total_size: total };

    // Memory-reservation block: (address, size) pairs ending with (0, 0).
    let mut p = off_rsvmap;
    for _ in 0..MAX_RANGES.saturating_mul(4) {
        let base = be64(d, p)?;
        let size = be64(d, p.checked_add(8).ok_or(FdtError::BadLayout)?)?;
        if base == 0 && size == 0 {
            break;
        }
        out.reserved.push(Range { base, size });
        p = p.checked_add(16).ok_or(FdtError::BadLayout)?;
    }

    // Structure block walk.
    let mut stack = [Level::default(); MAX_DEPTH];
    let mut depth = 0usize; // number of open nodes
    let mut p = off_struct;
    loop {
        let token = be32(structure, p)?;
        p = p.checked_add(4).ok_or(FdtError::BadStructure)?;
        match token {
            FDT_BEGIN_NODE => {
                let name = str_at(structure, p)?;
                p = align4(p.checked_add(name.len()).and_then(|x| x.checked_add(1)).ok_or(FdtError::BadStructure)?)?;
                let parent = match depth.checked_sub(1) {
                    Some(i) => *stack.get(i).ok_or(FdtError::BadStructure)?,
                    // Spec defaults for the root's parent context.
                    None => Level { addr_cells: 2, size_cells: 1, ..Level::default() },
                };
                let level = stack.get_mut(depth).ok_or(FdtError::BadStructure)?;
                *level = Level {
                    // Children inherit the cell sizes declared by this node
                    // (set when its properties are seen); start with spec
                    // defaults.
                    addr_cells: 2,
                    size_cells: 1,
                    is_memory: name == b"memory" || name.starts_with(b"memory@"),
                    in_reserved: parent.is_reserved_container,
                    is_reserved_container: depth == 1 && name == b"reserved-memory",
                    compatible_match: false,
                    reg: None,
                };
                depth = depth.checked_add(1).ok_or(FdtError::BadStructure)?;
            }
            FDT_END_NODE => {
                let i = depth.checked_sub(1).ok_or(FdtError::BadStructure)?;
                let node = *stack.get(i).ok_or(FdtError::BadStructure)?;
                // `reg` is interpreted with the *parent's* cell sizes.
                let (ac, sc) = match i.checked_sub(1) {
                    Some(pi) => {
                        let pl = stack.get(pi).ok_or(FdtError::BadStructure)?;
                        (pl.addr_cells, pl.size_cells)
                    }
                    None => (2, 1),
                };
                if let Some((off, len)) = node.reg {
                    let entry = usize::try_from(ac.checked_add(sc).ok_or(FdtError::BadStructure)?)
                        .ok()
                        .and_then(|c| c.checked_mul(4))
                        .ok_or(FdtError::BadStructure)?;
                    if entry > 0 && (ac == 1 || ac == 2) && (sc == 1 || sc == 2) {
                        let count = len.checked_div(entry).unwrap_or(0).min(MAX_RANGES);
                        for k in 0..count {
                            let at = off
                                .checked_add(k.checked_mul(entry).ok_or(FdtError::BadStructure)?)
                                .ok_or(FdtError::BadStructure)?;
                            let base = cells(structure, at, ac)?;
                            let size_at = (ac as usize)
                                .checked_mul(4)
                                .and_then(|o| at.checked_add(o))
                                .ok_or(FdtError::BadStructure)?;
                            let size = cells(structure, size_at, sc)?;
                            let r = Range { base, size };
                            if node.is_memory && i == 1 {
                                out.memory.push(r);
                            } else if node.in_reserved {
                                out.reserved.push(r);
                            }
                            if k == 0 && node.compatible_match && out.device.is_none() {
                                out.device = Some(base);
                            }
                        }
                    }
                }
                depth = i;
                if depth == 0 {
                    // Root closed; the next token must be FDT_END.
                    if be32(structure, p)? != FDT_END {
                        return Err(FdtError::BadStructure);
                    }
                    return Ok(out);
                }
            }
            FDT_PROP => {
                let len = be32(structure, p)? as usize;
                let nameoff = be32(structure, p.checked_add(4).ok_or(FdtError::BadStructure)?)? as usize;
                let value_off = p.checked_add(8).ok_or(FdtError::BadStructure)?;
                let value_end = value_off.checked_add(len).ok_or(FdtError::BadStructure)?;
                let value = structure.get(value_off..value_end).ok_or(FdtError::BadStructure)?;
                let name = str_at(strings, nameoff)?;
                let i = depth.checked_sub(1).ok_or(FdtError::BadStructure)?;
                let node = stack.get_mut(i).ok_or(FdtError::BadStructure)?;
                match name {
                    b"#address-cells" => node.addr_cells = be32(value, 0)?,
                    b"#size-cells" => node.size_cells = be32(value, 0)?,
                    b"reg" => node.reg = Some((value_off, len)),
                    b"device_type" if value.starts_with(b"memory") => node.is_memory = true,
                    b"compatible" if !compatible.is_empty() && list_contains(value, compatible) => {
                        node.compatible_match = true;
                    }
                    _ => {}
                }
                p = align4(value_end)?;
            }
            FDT_NOP => {}
            _ => return Err(FdtError::BadStructure), // FDT_END before the root closed, or junk
        }
    }
}

#[cfg(test)]
mod tests;
