//! MBR + GPT partition table parsing.
//!
//! Pure logic, no platform-specific calls — `list(path)` opens a regular
//! `File` for reading and parses the first few sectors. Used by the CLI's
//! `parts` subcommand and (later) by the partition selector when mounting
//! a whole-disk image / `\\.\PhysicalDriveN`.
//!
//! Sector size is fixed at 512 here. Real 4Kn drives need detection
//! (Windows: `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`). That lands together with
//! the Win32 raw-device `BlockDevice` impl.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECTOR: u64 = 512;
const MBR_SIG_OFF: usize = 510;
const MBR_PART_OFF: usize = 446;
const GPT_SIG: &[u8] = b"EFI PART";

/// One partition entry, normalised across MBR and GPT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// First LBA of the partition (sector index, sector size = 512).
    pub start_lba: u64,
    /// Length in sectors.
    pub num_sectors: u64,
    /// Short human-readable kind: `GPT:linux`, `GPT:efi`, `GPT:msbasic`,
    /// `GPT:swap`, `GPT:<hex-guid>`, `MBR:0x83` etc.
    pub kind: String,
    /// GPT only — UTF-16LE-decoded partition name. `None` for MBR.
    pub name: Option<String>,
}

/// Parse the partition table at `path`. Detects GPT (via 0xEE protective
/// MBR entry) and falls back to plain MBR.
pub fn list(path: &Path) -> Result<Vec<Partition>> {
    let mut f = File::open(path).with_context(|| format!("opening {path:?}"))?;
    let mut mbr = [0u8; 512];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut mbr)
        .with_context(|| format!("reading MBR sector from {path:?}"))?;

    if mbr[MBR_SIG_OFF] != 0x55 || mbr[MBR_SIG_OFF + 1] != 0xAA {
        bail!("no MBR signature at offset 510 — not a partitioned device");
    }

    if has_gpt_protective(&mbr) {
        parse_gpt(&mut f).context("parsing GPT")
    } else {
        Ok(parse_mbr(&mbr))
    }
}

fn has_gpt_protective(mbr: &[u8; 512]) -> bool {
    for i in 0..4 {
        let off = MBR_PART_OFF + i * 16;
        if mbr[off + 4] == 0xEE {
            return true;
        }
    }
    false
}

fn parse_mbr(mbr: &[u8; 512]) -> Vec<Partition> {
    let mut out = Vec::new();
    for i in 0..4 {
        let off = MBR_PART_OFF + i * 16;
        let kind_byte = mbr[off + 4];
        let lba = u32::from_le_bytes(mbr[off + 8..off + 12].try_into().unwrap()) as u64;
        let len = u32::from_le_bytes(mbr[off + 12..off + 16].try_into().unwrap()) as u64;
        if kind_byte == 0 || len == 0 {
            continue;
        }
        out.push(Partition {
            start_lba: lba,
            num_sectors: len,
            kind: format!("MBR:0x{kind_byte:02x}"),
            name: None,
        });
    }
    out
}

fn parse_gpt(f: &mut File) -> Result<Vec<Partition>> {
    let mut hdr = [0u8; 512];
    f.seek(SeekFrom::Start(SECTOR))?;
    f.read_exact(&mut hdr).context("reading GPT header sector")?;
    if &hdr[0..8] != GPT_SIG {
        bail!("GPT header signature missing (expected \"EFI PART\")");
    }
    let header_size = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
    if !(92..=512).contains(&header_size) {
        bail!("implausible GPT header size: {header_size}");
    }
    let part_array_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap());
    let n_entries = u32::from_le_bytes(hdr[80..84].try_into().unwrap());
    let entry_size = u32::from_le_bytes(hdr[84..88].try_into().unwrap()) as usize;
    if !(128..=4096).contains(&entry_size) {
        bail!("implausible GPT entry size: {entry_size}");
    }
    if n_entries > 4096 {
        bail!("implausible GPT entry count: {n_entries}");
    }

    let total = n_entries as usize * entry_size;
    let mut buf = vec![0u8; total];
    f.seek(SeekFrom::Start(part_array_lba * SECTOR))?;
    f.read_exact(&mut buf).context("reading GPT entries")?;

    let mut out = Vec::new();
    for i in 0..n_entries as usize {
        let e = &buf[i * entry_size..(i + 1) * entry_size];
        let type_guid: [u8; 16] = e[0..16].try_into().unwrap();
        if type_guid == [0u8; 16] {
            continue;
        }
        let start = u64::from_le_bytes(e[32..40].try_into().unwrap());
        let end = u64::from_le_bytes(e[40..48].try_into().unwrap());
        if end < start {
            continue;
        }
        let name = decode_utf16le(&e[56..128]);
        out.push(Partition {
            start_lba: start,
            num_sectors: end - start + 1,
            kind: classify_gpt_guid(&type_guid),
            name: if name.is_empty() { None } else { Some(name) },
        });
    }
    Ok(out)
}

fn decode_utf16le(buf: &[u8]) -> String {
    let mut out = String::new();
    for chunk in buf.chunks_exact(2) {
        let cu = u16::from_le_bytes(chunk.try_into().unwrap());
        if cu == 0 {
            break;
        }
        if let Some(c) = char::from_u32(cu as u32) {
            out.push(c);
        }
    }
    out
}

// Type GUIDs as on-disk bytes (mixed-endian: first 4 + next 2 + next 2 are LE,
// final 8 are big-endian/byte-by-byte).
const GUID_LINUX_FS: [u8; 16] = [
    0xAF, 0xDA, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];
const GUID_EFI_SYSTEM: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
const GUID_MS_BASIC: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];
const GUID_LINUX_SWAP: [u8; 16] = [
    0x6D, 0xFD, 0x57, 0x06, 0xAB, 0xA4, 0xC4, 0x43, 0x84, 0xE5, 0x09, 0x33, 0xC8, 0x4B, 0x4F, 0x4F,
];

fn classify_gpt_guid(g: &[u8; 16]) -> String {
    if *g == GUID_LINUX_FS {
        "GPT:linux".into()
    } else if *g == GUID_EFI_SYSTEM {
        "GPT:efi".into()
    } else if *g == GUID_MS_BASIC {
        "GPT:msbasic".into()
    } else if *g == GUID_LINUX_SWAP {
        "GPT:swap".into()
    } else {
        format!("GPT:{}", format_guid(g))
    }
}

fn format_guid(g: &[u8; 16]) -> String {
    // Display in canonical 8-4-4-4-12 form, accounting for the
    // mixed-endian on-disk layout.
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        g[3], g[2], g[1], g[0],
        g[5], g[4],
        g[7], g[6],
        g[8], g[9],
        g[10], g[11], g[12], g[13], g[14], g[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("ext4_win_driver_{tag}_{}_{n}.bin", std::process::id()));
        p
    }

    fn write_image(path: &std::path::Path, data: &[u8]) {
        let mut f = File::create(path).unwrap();
        f.write_all(data).unwrap();
    }

    fn build_mbr_with(parts: &[(u8, u32, u32)]) -> [u8; 512] {
        let mut mbr = [0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        for (i, &(kind, lba, len)) in parts.iter().enumerate() {
            let off = MBR_PART_OFF + i * 16;
            mbr[off + 4] = kind;
            mbr[off + 8..off + 12].copy_from_slice(&lba.to_le_bytes());
            mbr[off + 12..off + 16].copy_from_slice(&len.to_le_bytes());
        }
        mbr
    }

    #[test]
    fn rejects_missing_signature() {
        let path = tmp_path("nosig");
        write_image(&path, &[0u8; 1024]);
        let err = list(&path).unwrap_err();
        assert!(err.to_string().contains("no MBR signature"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn parses_plain_mbr() {
        let mbr = build_mbr_with(&[(0x83, 2048, 1_000_000), (0x07, 1_002_048, 500_000)]);
        let path = tmp_path("mbr");
        write_image(&path, &mbr);
        let parts = list(&path).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].start_lba, 2048);
        assert_eq!(parts[0].num_sectors, 1_000_000);
        assert_eq!(parts[0].kind, "MBR:0x83");
        assert_eq!(parts[1].kind, "MBR:0x07");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn skips_empty_mbr_slots() {
        let mbr = build_mbr_with(&[(0x83, 2048, 1000)]);
        let path = tmp_path("mbr_sparse");
        write_image(&path, &mbr);
        let parts = list(&path).unwrap();
        assert_eq!(parts.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn parses_gpt() {
        // Layout:
        //   LBA 0: protective MBR (0xEE entry)
        //   LBA 1: GPT header
        //   LBA 2..: partition entry array (we put it at LBA 2)
        let mut img = vec![0u8; SECTOR as usize * 64];

        // Protective MBR.
        img[510] = 0x55;
        img[511] = 0xAA;
        img[MBR_PART_OFF + 4] = 0xEE;
        let total_sectors = (img.len() / 512) as u32 - 1;
        img[MBR_PART_OFF + 8..MBR_PART_OFF + 12].copy_from_slice(&1u32.to_le_bytes());
        img[MBR_PART_OFF + 12..MBR_PART_OFF + 16].copy_from_slice(&total_sectors.to_le_bytes());

        // GPT header.
        let hdr_off = SECTOR as usize;
        img[hdr_off..hdr_off + 8].copy_from_slice(GPT_SIG);
        img[hdr_off + 12..hdr_off + 16].copy_from_slice(&92u32.to_le_bytes()); // header size
        img[hdr_off + 72..hdr_off + 80].copy_from_slice(&2u64.to_le_bytes()); // entry array LBA
        img[hdr_off + 80..hdr_off + 84].copy_from_slice(&3u32.to_le_bytes()); // n entries
        img[hdr_off + 84..hdr_off + 88].copy_from_slice(&128u32.to_le_bytes()); // entry size

        // Entries: [linux fs, EFI system, empty]
        let arr_off = SECTOR as usize * 2;
        // Entry 0: Linux fs at LBA 2048..=4047 (2000 sectors).
        img[arr_off..arr_off + 16].copy_from_slice(&GUID_LINUX_FS);
        img[arr_off + 32..arr_off + 40].copy_from_slice(&2048u64.to_le_bytes());
        img[arr_off + 40..arr_off + 48].copy_from_slice(&4047u64.to_le_bytes());
        // Name "rootfs" UTF-16LE.
        for (i, c) in "rootfs".encode_utf16().enumerate() {
            img[arr_off + 56 + i * 2..arr_off + 56 + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }

        // Entry 1: EFI system at LBA 4048..=5047.
        let e1 = arr_off + 128;
        img[e1..e1 + 16].copy_from_slice(&GUID_EFI_SYSTEM);
        img[e1 + 32..e1 + 40].copy_from_slice(&4048u64.to_le_bytes());
        img[e1 + 40..e1 + 48].copy_from_slice(&5047u64.to_le_bytes());

        let path = tmp_path("gpt");
        write_image(&path, &img);
        let parts = list(&path).unwrap();
        assert_eq!(parts.len(), 2, "got {parts:?}");
        assert_eq!(parts[0].kind, "GPT:linux");
        assert_eq!(parts[0].start_lba, 2048);
        assert_eq!(parts[0].num_sectors, 2000);
        assert_eq!(parts[0].name.as_deref(), Some("rootfs"));
        assert_eq!(parts[1].kind, "GPT:efi");
        assert_eq!(parts[1].start_lba, 4048);
        assert_eq!(parts[1].num_sectors, 1000);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unknown_gpt_guid_falls_back_to_hex() {
        let g = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
            0xFF, 0x00,
        ];
        let s = classify_gpt_guid(&g);
        assert!(s.starts_with("GPT:"));
        assert!(s.contains('-'));
    }
}
