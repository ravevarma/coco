//! Minimal GPT disk-image writer (protective MBR, primary and backup
//! headers, one EFI System Partition).

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::Res;

const SECTOR: u64 = 512;
const ENTRIES: u64 = 128;
const ENTRY_SIZE: u64 = 128;
const ENTRY_SECTORS: u64 = ENTRIES * ENTRY_SIZE / SECTOR; // 32
/// First LBA of the ESP (1 MiB alignment).
pub const ESP_START_LBA: u64 = 2048;

/// EFI System Partition type GUID C12A7328-F81F-11D2-BA4B-00A0C93EC93B.
const ESP_TYPE: [u8; 16] = guid(0xC12A_7328, 0xF81F, 0x11D2, [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]);
/// Fixed disk/partition GUIDs keep images reproducible.
const DISK_GUID: [u8; 16] = guid(0xC0C0_0001, 0xC0C0, 0x4C0C, [0x8C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x01]);
const PART_GUID: [u8; 16] = guid(0xC0C0_0002, 0xC0C0, 0x4C0C, [0x8C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x02]);

/// Encodes a GUID in the on-disk mixed-endian layout.
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// CRC-32 (IEEE 802.3, reflected, as required by GPT).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn put(buf: &mut [u8], off: usize, bytes: &[u8]) -> Res<()> {
    buf.get_mut(off..off + bytes.len()).ok_or("gpt buffer overflow")?.copy_from_slice(bytes);
    Ok(())
}

fn header(current: u64, backup: u64, entries_lba: u64, last_usable: u64, entries_crc: u32) -> Res<Vec<u8>> {
    let mut h = vec![0u8; SECTOR as usize];
    put(&mut h, 0, b"EFI PART")?;
    put(&mut h, 8, &0x0001_0000u32.to_le_bytes())?;
    put(&mut h, 12, &92u32.to_le_bytes())?;
    put(&mut h, 24, &current.to_le_bytes())?;
    put(&mut h, 32, &backup.to_le_bytes())?;
    put(&mut h, 40, &(2 + ENTRY_SECTORS).to_le_bytes())?;
    put(&mut h, 48, &last_usable.to_le_bytes())?;
    put(&mut h, 56, &DISK_GUID)?;
    put(&mut h, 72, &entries_lba.to_le_bytes())?;
    put(&mut h, 80, &(ENTRIES as u32).to_le_bytes())?;
    put(&mut h, 84, &(ENTRY_SIZE as u32).to_le_bytes())?;
    put(&mut h, 88, &entries_crc.to_le_bytes())?;
    let crc = crc32(h.get(..92).ok_or("gpt header")?);
    put(&mut h, 16, &crc.to_le_bytes())?;
    Ok(h)
}

/// Writes a GPT disk containing `esp_img` as its only partition.
pub fn write_disk(disk: &Path, esp_img: &Path) -> Res<()> {
    let esp_len = fs::metadata(esp_img).map_err(|e| format!("stat {}: {e}", esp_img.display()))?.len();
    if esp_len % SECTOR != 0 {
        return Err("ESP image size is not a multiple of 512".into());
    }
    let esp_sectors = esp_len / SECTOR;
    let total = ESP_START_LBA + esp_sectors + ESP_START_LBA; // 1 MiB tail (holds backup GPT)
    let last_lba = total - 1;
    let last_usable = last_lba - ENTRY_SECTORS - 1;
    let esp_end = ESP_START_LBA + esp_sectors - 1;
    if esp_end > last_usable {
        return Err("ESP does not fit".into());
    }

    let mut entries = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    put(&mut entries, 0, &ESP_TYPE)?;
    put(&mut entries, 16, &PART_GUID)?;
    put(&mut entries, 32, &ESP_START_LBA.to_le_bytes())?;
    put(&mut entries, 40, &esp_end.to_le_bytes())?;
    let name: Vec<u8> = "coco ESP".encode_utf16().flat_map(u16::to_le_bytes).collect();
    put(&mut entries, 56, &name)?;
    let entries_crc = crc32(&entries);

    let mut mbr = vec![0u8; SECTOR as usize];
    // Protective MBR: one 0xEE partition covering the disk.
    let size = u32::try_from(total - 1).unwrap_or(u32::MAX);
    put(&mut mbr, 446, &[0x00, 0x00, 0x02, 0x00, 0xEE, 0xFF, 0xFF, 0xFF])?;
    put(&mut mbr, 454, &1u32.to_le_bytes())?;
    put(&mut mbr, 458, &size.to_le_bytes())?;
    put(&mut mbr, 510, &[0x55, 0xAA])?;

    let primary = header(1, last_lba, 2, last_usable, entries_crc)?;
    let backup_entries_lba = last_lba - ENTRY_SECTORS;
    let backup = header(last_lba, 1, backup_entries_lba, last_usable, entries_crc)?;

    let _ = fs::remove_file(disk);
    let mut f = fs::File::create(disk).map_err(|e| format!("create {}: {e}", disk.display()))?;
    f.set_len(total * SECTOR).map_err(|e| format!("size disk: {e}"))?;
    let mut write_at = |lba: u64, data: &[u8]| -> Res<()> {
        f.seek(SeekFrom::Start(lba * SECTOR)).map_err(|e| format!("seek: {e}"))?;
        f.write_all(data).map_err(|e| format!("write: {e}"))
    };
    write_at(0, &mbr)?;
    write_at(1, &primary)?;
    write_at(2, &entries)?;
    write_at(backup_entries_lba, &entries)?;
    write_at(last_lba, &backup)?;
    let mut esp = fs::File::open(esp_img).map_err(|e| format!("open esp: {e}"))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut lba = ESP_START_LBA;
    loop {
        let n = esp.read(&mut buf).map_err(|e| format!("read esp: {e}"))?;
        if n == 0 {
            break;
        }
        write_at(lba, buf.get(..n).ok_or("read size")?)?;
        lba += (n as u64).div_ceil(SECTOR);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn crc32_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn guid_encoding() {
        // C12A7328-F81F-11D2-BA4B-00A0C93EC93B on disk.
        assert_eq!(
            ESP_TYPE,
            [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]
        );
    }

    #[test]
    fn header_crc_self_consistent() {
        let h = header(1, 1000, 2, 900, 0x1234).unwrap();
        let stored = u32::from_le_bytes(h[16..20].try_into().unwrap());
        let mut zeroed = h[..92].to_vec();
        zeroed[16..20].fill(0);
        assert_eq!(crc32(&zeroed), stored);
    }
}
