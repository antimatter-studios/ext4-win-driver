//! ext4-specific superblock detection + path probe.
//!
//! Generic helpers (drive-letter selection, GUID_DEVINTERFACE_DISK,
//! device-interface name parsing, raw-block reading) live in
//! [`winfsp_fs_skeleton`]. Only the ext4-coupled bits stay here.

use anyhow::{Context, Result};
use std::path::Path;

use winfsp_fs_skeleton::device::{BlockSource, FileSource};

/// Minimum bytes we need to read from a block source to inspect the
/// ext4 superblock magic. Superblock starts at byte 1024; `s_magic`
/// lives at offset 0x38 within it. Reading 1100 gives us enough slack
/// to keep the same buffer if we ever check more fields.
const SB_PROBE_LEN: usize = 1100;

/// Byte offset of the ext4 superblock magic (`s_magic`) from the start
/// of the device. 1024 (superblock start) + 0x38 (s_magic offset).
const EXT4_MAGIC_OFFSET: usize = 1024 + 0x38;

/// ext4 superblock magic, little-endian (`0xEF53`).
const EXT4_MAGIC: [u8; 2] = [0x53, 0xEF];

/// Return true iff `bytes` carries an ext4 superblock magic at the
/// canonical offset. Pure byte-slice predicate so unit tests don't
/// need a device. This is the function we hand to
/// `FsBackend::detect` from [`crate::main`]'s `Ext4Backend` impl.
pub fn is_ext4(bytes: &[u8]) -> bool {
    if bytes.len() < EXT4_MAGIC_OFFSET + 2 {
        return false;
    }
    bytes[EXT4_MAGIC_OFFSET..EXT4_MAGIC_OFFSET + 2] == EXT4_MAGIC
}

/// Open `path` (a Windows volume device like `\\.\X:` or a regular
/// file), read enough bytes to inspect the ext4 superblock, and
/// return whether [`is_ext4`] matches.
///
/// Returns `Ok(false)` for short / unreadable devices rather than an
/// error -- callers that probe whole-disk paths shouldn't spam logs
/// for non-block volumes (CD-ROM, empty card reader slots, etc.).
pub fn probe_path(path: &Path) -> Result<bool> {
    let src = FileSource::open(path)
        .with_context(|| format!("opening {} for probe", path.display()))?;
    let mut buf = vec![0u8; SB_PROBE_LEN];
    if src.read_at(0, &mut buf).is_err() {
        return Ok(false);
    }
    Ok(is_ext4(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_ext4_matches_magic_at_offset() {
        let mut buf = vec![0u8; SB_PROBE_LEN];
        buf[EXT4_MAGIC_OFFSET] = 0x53;
        buf[EXT4_MAGIC_OFFSET + 1] = 0xEF;
        assert!(is_ext4(&buf));
    }

    #[test]
    fn is_ext4_rejects_wrong_magic() {
        let mut buf = vec![0u8; SB_PROBE_LEN];
        buf[EXT4_MAGIC_OFFSET] = 0x42;
        buf[EXT4_MAGIC_OFFSET + 1] = 0x42;
        assert!(!is_ext4(&buf));
    }

    #[test]
    fn is_ext4_rejects_short_buffer() {
        let buf = vec![0x53u8; 16];
        assert!(!is_ext4(&buf));
    }
}
