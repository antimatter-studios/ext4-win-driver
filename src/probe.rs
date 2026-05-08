//! Volume probing + small helpers shared by the foreground `watch`
//! subcommand and the SCM service variant.
//!
//! Most of this module is filesystem-agnostic: drive-letter selection,
//! the disk device-interface class GUID, parsing the device path out
//! of a `WM_DEVICECHANGE` payload, raw-block reading. The one
//! ext4-specific bit is [`is_ext4`], which checks the 2-byte
//! superblock magic. When this code is extracted into the
//! `winfsp-fs-skeleton` project that bit becomes a `FsBackend::detect`
//! trait method -- everything else stays.
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

/// `GUID_DEVINTERFACE_DISK` -- physical disk device interface class.
/// Pass this in `DEV_BROADCAST_DEVICEINTERFACE_W` when registering for
/// `WM_DEVICECHANGE` notifications to receive disk arrival/removal
/// events. Disk-level (rather than volume-level) subscription is
/// required because Windows refuses to assign drive letters to
/// partitions whose type code it doesn't recognise (e.g. `0x83`
/// Linux), so a volume-level subscription would never fire for
/// typical Linux ext4 SD cards.
#[cfg(target_os = "windows")]
pub const GUID_DEVINTERFACE_DISK: windows_sys::core::GUID = windows_sys::core::GUID {
    data1: 0x53F5_6307,
    data2: 0xB6BF,
    data3: 0x11D0,
    data4: [0x94, 0xF2, 0x00, 0xA0, 0xC9, 0x1E, 0xFB, 0x8B],
};

/// Pull the device path out of a `DEV_BROADCAST_DEVICEINTERFACE_W`
/// pointer received via `WM_DEVICECHANGE` lparam. The struct's
/// `dbcc_name` field is a flexible array of `u16`; the actual name
/// length is `dbcc_size` minus the fixed-prefix size, terminated by
/// the first null. Returns the path as a Rust `String` (the
/// device-interface name path is always ASCII-printable in practice).
///
/// Caller must guarantee the pointer is valid for the duration of the
/// call.
///
/// # Safety
///
/// `bdi` must point at a properly-aligned, fully-initialised
/// `DEV_BROADCAST_DEVICEINTERFACE_W` whose `dbcc_size` covers the
/// embedded `dbcc_name` payload. WM_DEVICECHANGE delivers exactly
/// such pointers, so the foreground/service wndprocs satisfy this.
#[cfg(target_os = "windows")]
pub unsafe fn device_interface_name(
    bdi: *const windows_sys::Win32::UI::WindowsAndMessaging::DEV_BROADCAST_DEVICEINTERFACE_W,
) -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::DEV_BROADCAST_DEVICEINTERFACE_W;

    if bdi.is_null() {
        return None;
    }
    let total = (*bdi).dbcc_size as usize;
    // Offset of `dbcc_name` within the struct: dbcc_size (u32, 4) +
    // dbcc_devicetype (u32, 4) + dbcc_reserved (u32, 4) +
    // dbcc_classguid (GUID, 16) = 28. We can't use `size_of - 2`
    // because the struct is padded to its 4-byte alignment, so
    // size_of returns 32 -- which would skip the first wide char of
    // the name (so `\\?\STORAGE...` becomes `\?\STORAGE...`, an
    // ERROR_INVALID_NAME path). offset_of! would be cleaner but
    // requires Rust >= 1.77 -- pin the literal until we bump MSRV.
    const DBCC_NAME_OFFSET: usize = 4 + 4 + 4 + 16;
    if total <= DBCC_NAME_OFFSET {
        return None;
    }
    let name_bytes = total - DBCC_NAME_OFFSET;
    let name_chars = name_bytes / 2;
    let ptr = (bdi as *const u8).add(DBCC_NAME_OFFSET) as *const u16;
    let slice = std::slice::from_raw_parts(ptr, name_chars);
    let trimmed = match slice.iter().position(|&c| c == 0) {
        Some(n) => &slice[..n],
        None => slice,
    };
    let s = OsString::from_wide(trimmed).into_string().ok()?;
    Some(s)
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
