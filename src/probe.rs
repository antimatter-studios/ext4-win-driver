//! ext4-specific superblock detection.
//!
//! Generic device probing (drive-letter selection, GUID_DEVINTERFACE_DISK,
//! device-interface name parsing, raw-block reading at any offset) lives
//! in [`winfsp_fs_skeleton`]'s `probe_at_offset`. This module exists only
//! to provide the byte-slice predicate that the skeleton calls back into
//! via the [`crate::Ext4Backend`] `FsBackend::detect` impl.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Buffer length the tests use to construct a synthetic superblock.
    /// Must be at least `EXT4_MAGIC_OFFSET + 2`; 1100 leaves headroom
    /// in case future tests want to set additional superblock fields.
    const TEST_BUF_LEN: usize = 1100;

    #[test]
    fn is_ext4_matches_magic_at_offset() {
        let mut buf = vec![0u8; TEST_BUF_LEN];
        buf[EXT4_MAGIC_OFFSET] = 0x53;
        buf[EXT4_MAGIC_OFFSET + 1] = 0xEF;
        assert!(is_ext4(&buf));
    }

    #[test]
    fn is_ext4_rejects_wrong_magic() {
        let mut buf = vec![0u8; TEST_BUF_LEN];
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
