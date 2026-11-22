//! ACPI table discovery for coco.
//!
//! Firmware tables are untrusted input. Every read goes through a
//! [`PhysMemory`] implementation that may fail, every length is bounded,
//! every table checksum is verified (tables with a bad checksum are
//! rejected, not trusted), and all arithmetic is checked. No allocation,
//! no `unsafe`.
//!
//! Parsed: RSDP (v1/v2), XSDT or RSDT, MADT (local APIC/x2APIC, I/O APIC,
//! interrupt source overrides, LAPIC address override), FADT (PM timer,
//! PM1 control blocks, reset register, DSDT), HPET base, and the `\_S5`
//! sleep-type package from the DSDT (for soft power-off).
#![no_std]
#![cfg_attr(not(test), deny(clippy::indexing_slicing, clippy::arithmetic_side_effects))]

/// Largest table accepted (DSDTs on laptops are typically < 512 KiB).
pub const MAX_TABLE_LEN: u32 = 4 << 20;
/// Most XSDT/RSDT entries examined.
pub const MAX_ROOT_ENTRIES: usize = 256;
/// Most CPUs recorded from the MADT.
pub const MAX_CPUS: usize = 256;
/// Most I/O APICs recorded.
pub const MAX_IOAPICS: usize = 16;
/// Most interrupt source overrides recorded.
pub const MAX_OVERRIDES: usize = 32;

const SDT_HEADER_LEN: u32 = 36;

/// Access to physical memory holding firmware tables.
pub trait PhysMemory {
    /// Reads `out.len()` bytes starting at physical address `phys`.
    ///
    /// # Errors
    /// [`AcpiError::Unreadable`] if the range cannot be accessed.
    fn read(&mut self, phys: u64, out: &mut [u8]) -> Result<(), AcpiError>;
}

/// ACPI parsing errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpiError {
    /// The firmware did not provide an RSDP address.
    NoRsdp,
    /// A physical range could not be read.
    Unreadable(u64),
    /// Signature mismatch at the given address.
    BadSignature(u64),
    /// Checksum mismatch for the structure at the given address.
    BadChecksum(u64),
    /// A length field is out of bounds.
    BadLength(u64),
    /// Arithmetic overflow computing an address.
    Overflow,
}

fn rd<const N: usize>(mem: &mut dyn PhysMemory, phys: u64) -> Result<[u8; N], AcpiError> {
    let mut b = [0u8; N];
    mem.read(phys, &mut b)?;
    Ok(b)
}
fn rd_u8(mem: &mut dyn PhysMemory, phys: u64) -> Result<u8, AcpiError> {
    Ok(u8::from_le_bytes(rd::<1>(mem, phys)?))
}
fn rd_u16(mem: &mut dyn PhysMemory, phys: u64) -> Result<u16, AcpiError> {
    Ok(u16::from_le_bytes(rd::<2>(mem, phys)?))
}
fn rd_u32(mem: &mut dyn PhysMemory, phys: u64) -> Result<u32, AcpiError> {
    Ok(u32::from_le_bytes(rd::<4>(mem, phys)?))
}
fn rd_u64(mem: &mut dyn PhysMemory, phys: u64) -> Result<u64, AcpiError> {
    Ok(u64::from_le_bytes(rd::<8>(mem, phys)?))
}
fn add(a: u64, b: u64) -> Result<u64, AcpiError> {
    a.checked_add(b).ok_or(AcpiError::Overflow)
}

/// Sums `len` bytes at `phys` (mod 256), streaming in small chunks.
fn checksum(mem: &mut dyn PhysMemory, phys: u64, len: u32) -> Result<u8, AcpiError> {
    let mut sum = 0u8;
    let mut done = 0u32;
    let mut buf = [0u8; 256];
    while done < len {
        let n = len.saturating_sub(done).min(256);
        let chunk = buf.get_mut(..n as usize).ok_or(AcpiError::Overflow)?;
        mem.read(add(phys, u64::from(done))?, chunk)?;
        sum = chunk.iter().fold(sum, |s, &b| s.wrapping_add(b));
        done = done.saturating_add(n);
    }
    Ok(sum)
}

/// A validated System Description Table header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sdt {
    /// Physical address of the table.
    pub phys: u64,
    /// Four-character signature.
    pub signature: [u8; 4],
    /// Total length including the header.
    pub length: u32,
    /// Revision.
    pub revision: u8,
}

/// Reads and validates (signature, length, checksum) the table at `phys`.
///
/// # Errors
/// Any [`AcpiError`].
pub fn read_sdt(mem: &mut dyn PhysMemory, phys: u64, expected: Option<&[u8; 4]>) -> Result<Sdt, AcpiError> {
    let signature = rd::<4>(mem, phys)?;
    if expected.is_some_and(|e| *e != signature) {
        return Err(AcpiError::BadSignature(phys));
    }
    let length = rd_u32(mem, add(phys, 4)?)?;
    if !(SDT_HEADER_LEN..=MAX_TABLE_LEN).contains(&length) {
        return Err(AcpiError::BadLength(phys));
    }
    add(phys, u64::from(length))?;
    if checksum(mem, phys, length)? != 0 {
        return Err(AcpiError::BadChecksum(phys));
    }
    Ok(Sdt { phys, signature, length, revision: rd_u8(mem, add(phys, 8)?)? })
}

/// A local APIC (one logical CPU) from the MADT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Cpu {
    /// APIC ID (xAPIC or x2APIC).
    pub apic_id: u32,
    /// ACPI processor UID.
    pub uid: u32,
    /// Enabled at boot.
    pub enabled: bool,
    /// May be brought online later.
    pub online_capable: bool,
}

/// An I/O APIC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IoApic {
    /// I/O APIC ID.
    pub id: u8,
    /// MMIO base address.
    pub address: u32,
    /// First global system interrupt it handles.
    pub gsi_base: u32,
}

/// An ISA interrupt source override.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Override {
    /// ISA IRQ.
    pub source: u8,
    /// Global system interrupt.
    pub gsi: u32,
    /// MPS INTI flags (polarity, trigger mode).
    pub flags: u16,
}

/// Parsed MADT.
#[derive(Clone, Copy, Debug)]
pub struct Madt {
    /// Local APIC physical address (after any 64-bit override).
    pub lapic_address: u64,
    /// Dual 8259 PICs present (PCAT_COMPAT).
    pub pcat_compat: bool,
    cpus: [Cpu; MAX_CPUS],
    cpu_count: usize,
    ioapics: [IoApic; MAX_IOAPICS],
    ioapic_count: usize,
    overrides: [Override; MAX_OVERRIDES],
    override_count: usize,
    /// Entries ignored because a fixed-size list was full.
    pub dropped_entries: u32,
}

impl Madt {
    /// CPUs listed in the MADT.
    #[must_use]
    pub fn cpus(&self) -> &[Cpu] {
        self.cpus.get(..self.cpu_count).unwrap_or(&[])
    }
    /// I/O APICs.
    #[must_use]
    pub fn ioapics(&self) -> &[IoApic] {
        self.ioapics.get(..self.ioapic_count).unwrap_or(&[])
    }
    /// Interrupt source overrides.
    #[must_use]
    pub fn overrides(&self) -> &[Override] {
        self.overrides.get(..self.override_count).unwrap_or(&[])
    }
    /// Number of CPUs enabled at boot.
    #[must_use]
    pub fn enabled_cpus(&self) -> usize {
        self.cpus().iter().filter(|c| c.enabled).count()
    }
}

fn push<T: Copy>(list: &mut [T], count: &mut usize, dropped: &mut u32, item: T) {
    if let Some(slot) = list.get_mut(*count) {
        *slot = item;
        *count = count.saturating_add(1);
    } else {
        *dropped = dropped.saturating_add(1);
    }
}

fn parse_madt(mem: &mut dyn PhysMemory, sdt: &Sdt) -> Result<Madt, AcpiError> {
    let mut m = Madt {
        lapic_address: u64::from(rd_u32(mem, add(sdt.phys, 36)?)?),
        pcat_compat: rd_u32(mem, add(sdt.phys, 40)?)? & 1 != 0,
        cpus: [Cpu::default(); MAX_CPUS],
        cpu_count: 0,
        ioapics: [IoApic::default(); MAX_IOAPICS],
        ioapic_count: 0,
        overrides: [Override::default(); MAX_OVERRIDES],
        override_count: 0,
        dropped_entries: 0,
    };
    let end = add(sdt.phys, u64::from(sdt.length))?;
    let mut p = add(sdt.phys, 44)?;
    while add(p, 2)? <= end {
        let ty = rd_u8(mem, p)?;
        let len = u64::from(rd_u8(mem, add(p, 1)?)?);
        if len < 2 || add(p, len)? > end {
            return Err(AcpiError::BadLength(p));
        }
        match (ty, len) {
            (0, 8..) => {
                let flags = rd_u32(mem, add(p, 4)?)?;
                let cpu = Cpu {
                    uid: u32::from(rd_u8(mem, add(p, 2)?)?),
                    apic_id: u32::from(rd_u8(mem, add(p, 3)?)?),
                    enabled: flags & 1 != 0,
                    online_capable: flags & 2 != 0,
                };
                push(&mut m.cpus, &mut m.cpu_count, &mut m.dropped_entries, cpu);
            }
            (1, 12..) => {
                let io = IoApic {
                    id: rd_u8(mem, add(p, 2)?)?,
                    address: rd_u32(mem, add(p, 4)?)?,
                    gsi_base: rd_u32(mem, add(p, 8)?)?,
                };
                push(&mut m.ioapics, &mut m.ioapic_count, &mut m.dropped_entries, io);
            }
            (2, 10..) => {
                let o = Override {
                    source: rd_u8(mem, add(p, 3)?)?,
                    gsi: rd_u32(mem, add(p, 4)?)?,
                    flags: rd_u16(mem, add(p, 8)?)?,
                };
                push(&mut m.overrides, &mut m.override_count, &mut m.dropped_entries, o);
            }
            (5, 12..) => m.lapic_address = rd_u64(mem, add(p, 4)?)?,
            (9, 16..) => {
                let flags = rd_u32(mem, add(p, 8)?)?;
                let cpu = Cpu {
                    apic_id: rd_u32(mem, add(p, 4)?)?,
                    uid: rd_u32(mem, add(p, 12)?)?,
                    enabled: flags & 1 != 0,
                    online_capable: flags & 2 != 0,
                };
                push(&mut m.cpus, &mut m.cpu_count, &mut m.dropped_entries, cpu);
            }
            _ => {} // other entry types are not needed yet
        }
        p = add(p, len)?;
    }
    Ok(m)
}

/// ACPI Generic Address Structure (subset).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenericAddress {
    /// 0 = system memory, 1 = system I/O.
    pub space: u8,
    /// Register width in bits.
    pub bit_width: u8,
    /// Address.
    pub address: u64,
}

/// Address space id: system I/O ports.
pub const SPACE_IO: u8 = 1;
/// Address space id: system memory.
pub const SPACE_MEMORY: u8 = 0;

/// Parsed FADT fields coco uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fadt {
    /// ACPI PM timer I/O port.
    pub pm_timer_port: Option<u16>,
    /// PM timer is 32 bits wide (else 24).
    pub pm_timer_32bit: bool,
    /// PM1a control block I/O port.
    pub pm1a_control: Option<u16>,
    /// PM1b control block I/O port.
    pub pm1b_control: Option<u16>,
    /// Reset register and the value to write, if supported.
    pub reset: Option<(GenericAddress, u8)>,
    /// Physical address of the DSDT.
    pub dsdt: u64,
    /// Hardware-reduced ACPI (no PM timer / PM1 blocks).
    pub hardware_reduced: bool,
    /// SMI command port used to switch the platform into ACPI mode.
    pub smi_command: Option<u16>,
    /// Value written to [`Fadt::smi_command`] to enable ACPI mode.
    pub acpi_enable: u8,
}

fn gas(mem: &mut dyn PhysMemory, p: u64) -> Result<GenericAddress, AcpiError> {
    Ok(GenericAddress { space: rd_u8(mem, p)?, bit_width: rd_u8(mem, add(p, 1)?)?, address: rd_u64(mem, add(p, 4)?)? })
}

fn io_port(addr: u64) -> Option<u16> {
    u16::try_from(addr).ok().filter(|&p| p != 0)
}

fn parse_fadt(mem: &mut dyn PhysMemory, sdt: &Sdt) -> Result<Fadt, AcpiError> {
    let has = |off: u32, len: u32| off.checked_add(len).is_some_and(|e| e <= sdt.length);
    let at = |off: u64| add(sdt.phys, off);
    let flags = if has(112, 4) { rd_u32(mem, at(112)?)? } else { 0 };
    let mut f = Fadt {
        pm_timer_port: if has(76, 4) { io_port(u64::from(rd_u32(mem, at(76)?)?)) } else { None },
        pm_timer_32bit: flags & (1 << 8) != 0,
        pm1a_control: if has(64, 4) { io_port(u64::from(rd_u32(mem, at(64)?)?)) } else { None },
        pm1b_control: if has(68, 4) { io_port(u64::from(rd_u32(mem, at(68)?)?)) } else { None },
        reset: None,
        dsdt: if has(40, 4) { u64::from(rd_u32(mem, at(40)?)?) } else { 0 },
        hardware_reduced: flags & (1 << 20) != 0,
        smi_command: if has(48, 4) { io_port(u64::from(rd_u32(mem, at(48)?)?)) } else { None },
        acpi_enable: if has(52, 1) { rd_u8(mem, at(52)?)? } else { 0 },
    };
    if flags & (1 << 10) != 0 && has(116, 13) {
        let reg = gas(mem, at(116)?)?;
        if reg.address != 0 && (reg.space == SPACE_IO || reg.space == SPACE_MEMORY) {
            f.reset = Some((reg, rd_u8(mem, at(128)?)?));
        }
    }
    // 64-bit (X_) fields take precedence when present and non-zero.
    if has(140, 8) {
        let x = rd_u64(mem, at(140)?)?;
        if x != 0 {
            f.dsdt = x;
        }
    }
    for (off, slot) in [(172u64, 0usize), (184, 1), (208, 2)] {
        if !has(off as u32, 12) {
            continue;
        }
        let g = gas(mem, at(off)?)?;
        if g.space != SPACE_IO {
            continue;
        }
        if let Some(port) = io_port(g.address) {
            match slot {
                0 => f.pm1a_control = Some(port),
                1 => f.pm1b_control = Some(port),
                _ => f.pm_timer_port = Some(port),
            }
        }
    }
    if f.hardware_reduced {
        f.pm_timer_port = None;
    }
    Ok(f)
}

/// Decodes an AML PkgLength at `p`; returns (length, bytes used).
fn pkg_length(mem: &mut dyn PhysMemory, p: u64) -> Result<(u32, u64), AcpiError> {
    let lead = rd_u8(mem, p)?;
    let extra = lead >> 6;
    if extra == 0 {
        return Ok((u32::from(lead & 0x3F), 1));
    }
    let mut len = u32::from(lead & 0x0F);
    for i in 0..extra {
        let b = rd_u8(mem, add(p, u64::from(i).saturating_add(1))?)?;
        len |= u32::from(b) << (4u32.saturating_add(8u32.saturating_mul(u32::from(i))));
    }
    Ok((len, u64::from(extra).saturating_add(1)))
}

/// Decodes one integer element of a package (ZeroOp/OneOp/BytePrefix/...).
fn aml_small_int(mem: &mut dyn PhysMemory, p: u64) -> Result<(u8, u64), AcpiError> {
    match rd_u8(mem, p)? {
        0x00 => Ok((0, 1)),
        0x01 => Ok((1, 1)),
        0x0A => Ok((rd_u8(mem, add(p, 1)?)?, 2)),
        0x0B => Ok((rd_u8(mem, add(p, 1)?)?, 3)), // WordPrefix: low byte
        0x0C => Ok((rd_u8(mem, add(p, 1)?)?, 5)), // DWordPrefix: low byte
        _ => Err(AcpiError::BadLength(p)),
    }
}

/// Finds `Name(_S5_, Package() { SLP_TYPa, SLP_TYPb, ... })` in AML and
/// returns the two sleep-type values. This is a targeted pattern match, not
/// an AML interpreter: `_S5_` defined through methods is not supported.
///
/// # Errors
/// Read errors; `Ok(None)` if no usable `_S5_` object exists.
pub fn find_s5(mem: &mut dyn PhysMemory, dsdt: &Sdt) -> Result<Option<(u8, u8)>, AcpiError> {
    const PATTERN: [u8; 5] = [b'_', b'S', b'5', b'_', 0x12];
    let start = add(dsdt.phys, u64::from(SDT_HEADER_LEN))?;
    let end = add(dsdt.phys, u64::from(dsdt.length))?;
    let mut buf = [0u8; 512];
    let mut pos = start;
    while pos < end {
        let n = end.saturating_sub(pos).min(buf.len() as u64) as usize;
        let chunk = buf.get_mut(..n).ok_or(AcpiError::Overflow)?;
        mem.read(pos, chunk)?;
        for (i, w) in chunk.windows(PATTERN.len()).enumerate() {
            if w != PATTERN {
                continue;
            }
            let name_at = add(pos, i as u64)?;
            // Must be preceded by NameOp (0x08), optionally with a root '\'.
            let before = |k: u64| name_at.checked_sub(k).filter(|&a| a >= start);
            let op = match before(1) {
                Some(a) => rd_u8(mem, a)?,
                None => 0,
            };
            let op2 = match before(2) {
                Some(a) => rd_u8(mem, a)?,
                None => 0,
            };
            if op != 0x08 && !(op == b'\\' && op2 == 0x08) {
                continue;
            }
            let mut p = add(name_at, 5)?; // past "_S5_" and PackageOp
            let (_len, used) = pkg_length(mem, p)?;
            p = add(p, used)?;
            let count = rd_u8(mem, p)?;
            p = add(p, 1)?;
            if count < 2 || p >= end {
                continue;
            }
            let (a, used_a) = aml_small_int(mem, p)?;
            let (b, _) = aml_small_int(mem, add(p, used_a)?)?;
            return Ok(Some((a & 7, b & 7)));
        }
        // Advance with overlap so a pattern straddling chunks is found.
        let step = (n as u64).saturating_sub(4).max(1); // keep PATTERN.len() - 1 bytes of overlap
        pos = add(pos, step)?;
    }
    Ok(None)
}

/// Everything coco learns from ACPI at boot.
#[derive(Clone, Copy, Debug)]
pub struct AcpiInfo {
    /// RSDP revision (0 = ACPI 1.0, 2 = ACPI 2.0+).
    pub revision: u8,
    /// OEM identifier.
    pub oem_id: [u8; 6],
    /// Number of root-table entries examined.
    pub table_count: usize,
    /// Tables skipped due to a bad signature/length/checksum.
    pub rejected_tables: usize,
    /// MADT, if present and valid.
    pub madt: Option<Madt>,
    /// FADT, if present and valid.
    pub fadt: Option<Fadt>,
    /// HPET MMIO base, if present.
    pub hpet: Option<u64>,
    /// `\_S5` sleep types from the DSDT, if found.
    pub s5: Option<(u8, u8)>,
}

/// Parses the ACPI tables reachable from the RSDP at `rsdp`.
///
/// # Errors
/// Errors in the RSDP or root table. Individual bad tables are skipped
/// and counted in [`AcpiInfo::rejected_tables`].
pub fn parse(mem: &mut dyn PhysMemory, rsdp: u64) -> Result<AcpiInfo, AcpiError> {
    if rsdp == 0 {
        return Err(AcpiError::NoRsdp);
    }
    if rd::<8>(mem, rsdp)? != *b"RSD PTR " {
        return Err(AcpiError::BadSignature(rsdp));
    }
    if checksum(mem, rsdp, 20)? != 0 {
        return Err(AcpiError::BadChecksum(rsdp));
    }
    let revision = rd_u8(mem, add(rsdp, 15)?)?;
    let oem_id = rd::<6>(mem, add(rsdp, 9)?)?;
    let (root, entry_size) = if revision >= 2 {
        let len = rd_u32(mem, add(rsdp, 20)?)?;
        if !(36..=4096).contains(&len) {
            return Err(AcpiError::BadLength(rsdp));
        }
        if checksum(mem, rsdp, len)? != 0 {
            return Err(AcpiError::BadChecksum(rsdp));
        }
        (rd_u64(mem, add(rsdp, 24)?)?, 8u64)
    } else {
        (u64::from(rd_u32(mem, add(rsdp, 16)?)?), 4u64)
    };
    let root_sig = if entry_size == 8 { b"XSDT" } else { b"RSDT" };
    let root = read_sdt(mem, root, Some(root_sig))?;
    let count = u64::from(root.length.saturating_sub(SDT_HEADER_LEN))
        .checked_div(entry_size)
        .unwrap_or(0)
        .min(MAX_ROOT_ENTRIES as u64);
    let mut info =
        AcpiInfo { revision, oem_id, table_count: 0, rejected_tables: 0, madt: None, fadt: None, hpet: None, s5: None };
    for i in 0..count {
        let at = add(add(root.phys, u64::from(SDT_HEADER_LEN))?, i.saturating_mul(entry_size))?;
        let phys = if entry_size == 8 { rd_u64(mem, at)? } else { u64::from(rd_u32(mem, at)?) };
        info.table_count = info.table_count.saturating_add(1);
        let Ok(sdt) = read_sdt(mem, phys, None) else {
            info.rejected_tables = info.rejected_tables.saturating_add(1);
            continue;
        };
        match &sdt.signature {
            b"APIC" => match parse_madt(mem, &sdt) {
                Ok(m) => info.madt = Some(m),
                Err(_) => info.rejected_tables = info.rejected_tables.saturating_add(1),
            },
            b"FACP" => match parse_fadt(mem, &sdt) {
                Ok(f) => info.fadt = Some(f),
                Err(_) => info.rejected_tables = info.rejected_tables.saturating_add(1),
            },
            b"HPET" if sdt.length >= 56 => {
                let g = gas(mem, add(sdt.phys, 40)?)?;
                if g.space == SPACE_MEMORY && g.address != 0 {
                    info.hpet = Some(g.address);
                }
            }
            _ => {}
        }
    }
    if let Some(dsdt_phys) = info.fadt.map(|f| f.dsdt).filter(|&d| d != 0) {
        match read_sdt(mem, dsdt_phys, Some(b"DSDT")) {
            Ok(dsdt) => info.s5 = find_s5(mem, &dsdt)?,
            Err(_) => info.rejected_tables = info.rejected_tables.saturating_add(1),
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests;
