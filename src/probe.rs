//! Volume probing + small helpers shared by the foreground `watch`
//! subcommand and the SCM service variant.
//!
//! Most of this module is filesystem-agnostic: drive-letter selection,
//! `DEV_BROADCAST_VOLUME::dbcv_unitmask` decoding, raw-block reading. The
//! one ext4-specific bit is [`is_ext4`], which checks the 2-byte
//! superblock magic. When this code is extracted into the
//! `winfsp-fs-skeleton` project that bit becomes a `FsBackend::detect`
//! trait method — everything else stays.
//!
//! Placed in its own module (rather than `watch::imp`) so `service.rs`
//! can reuse the helpers without pulling in the foreground message-pump
//! glue.
//!
//! On non-Windows host builds the watch + service modules are cfg-gated
//! out, so the helpers here are dead. Allow at module level rather than
//! per-item so the macOS development build stays warning-free without
//! sprinkling cfg gates that obscure the cross-platform shape.

#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::Path;

use crate::device::{BlockSource, FileSource};

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

/// Return true iff `bytes` (at least [`SB_PROBE_LEN`] long) carries an
/// ext4 superblock magic at the canonical offset.
///
/// Pure byte-slice predicate so unit tests don't need a device. The
/// future `winfsp-fs-skeleton` extraction turns this into a
/// `FsBackend::detect(&[u8]) -> bool` trait method.
pub fn is_ext4(bytes: &[u8]) -> bool {
    if bytes.len() < EXT4_MAGIC_OFFSET + 2 {
        return false;
    }
    bytes[EXT4_MAGIC_OFFSET..EXT4_MAGIC_OFFSET + 2] == EXT4_MAGIC
}

/// Open `path` (a Windows volume device like `\\.\X:` or a regular
/// file), read enough bytes to inspect the ext4 superblock, and return
/// whether [`is_ext4`] matches.
///
/// Returns `Ok(false)` for short / unreadable devices rather than an
/// error — the watcher needs to ignore non-block volumes (CD-ROM, empty
/// card reader slots, etc.) without spamming the log.
pub fn probe_path(path: &Path) -> Result<bool> {
    let src = FileSource::open(path)
        .with_context(|| format!("opening {} for probe", path.display()))?;
    let mut buf = vec![0u8; SB_PROBE_LEN];
    if src.read_at(0, &mut buf).is_err() {
        return Ok(false);
    }
    Ok(is_ext4(&buf))
}

/// Decode `DEV_BROADCAST_VOLUME::dbcv_unitmask` (bit N = drive A+N)
/// into the list of affected drive letters. Used by both the
/// foreground watcher and the service.
#[cfg(target_os = "windows")]
pub fn unitmask_to_letters(mask: u32) -> Vec<char> {
    let mut out = Vec::new();
    for i in 0..26u32 {
        if (mask >> i) & 1 != 0 {
            out.push((b'A' + i as u8) as char);
        }
    }
    out
}

/// Pick the lowest free drive letter in `E..=Z` (skipping ones already
/// in use according to `GetLogicalDrives`). Returns `None` if none are
/// free.
///
/// Skips A..D so we don't collide with floppy / system / CD-ROM
/// reservations the user expects to be sticky.
#[cfg(target_os = "windows")]
pub fn pick_drive_letter() -> Option<char> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    let in_use = unsafe { GetLogicalDrives() };
    // Bit 0 = A, bit 4 = E, ...
    for i in 4u32..26 {
        if (in_use >> i) & 1 == 0 {
            return Some((b'A' + i as u8) as char);
        }
    }
    None
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
