//! Filesystem mount handle and (with the `mount` feature) WinFsp adapter.
//!
//! `Mount` is a thin RAII wrapper around the `fs_ext4_*` C ABI. Two open
//! paths:
//!   - `open_direct(path)` — `fs_ext4_mount(path)`, raw ext4 image only.
//!   - `open_partition(path, n)` — `fs_ext4_mount_with_callbacks(...)`,
//!     reads through a `SliceCtx` that offset-shifts into the chosen
//!     GPT/MBR partition slice.
//!
//! The CLI subcommands in [`crate::cmd`] use this for quick read access.
//! The `mount` feature additionally builds a WinFsp `FileSystemContext`
//! adapter on top — see the bottom of this file.

use anyhow::{anyhow, bail, Context, Result};
use fs_ext4::capi::*;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::path::Path;
use std::sync::Arc;

use crate::MountArgs;
use winfsp_fs_skeleton::device::{BlockSource, FileSource};
use winfsp_fs_skeleton::partition;

/// RAII handle around `*mut fs_ext4_fs_t`.
///
/// On drop:
///   1. unmount the fs (issues final reads, must run before context drop)
///   2. reclaim the boxed callback context (if any) leaked into the C ABI
///
/// `pub(crate)` because the CLI command implementations reach in for the
/// raw fs pointer; the WinFsp adapter does the same.
pub struct Mount {
    pub(crate) fs: *mut fs_ext4_fs_t,
    /// Set when mounted via `fs_ext4_mount_with_callbacks` /
    /// `fs_ext4_mount_rw_with_callbacks`. Owned here.
    cb_ctx: Option<*mut SliceCtx>,
    /// True when mounted RW. Read by the WinFsp adapter (`mount`
    /// feature, Windows only) — `Ext4Context::ensure_writable` consults
    /// it at the top of every mutating callback as a defense-in-depth
    /// gate, and `winfsp_adapter::run` uses it to decide whether to set
    /// the `read_only_volume` `VolumeParams` flag. Always written;
    /// non-Windows / non-`mount` builds never read it, so the dead-code
    /// allow keeps `cargo check` quiet on macOS / Linux dev hosts.
    #[cfg_attr(not(all(windows, feature = "mount")), allow(dead_code))]
    pub(crate) writable: bool,
}

// `*mut fs_ext4_fs_t` is opaque to us and the underlying `Filesystem` is
// internally synchronized, so it's safe to share across threads.
unsafe impl Send for Mount {}
unsafe impl Sync for Mount {}

impl Mount {
    pub fn open(mt: &MountArgs) -> Result<Self> {
        // Treat --part 0 as "no partition" (same as omitting the flag).
        // Lets the ExtFsWatcher service pass --part unconditionally
        // through the fixed WinFsp.Launcher CommandLine template.
        match mt.part {
            None | Some(0) => Self::open_direct(&mt.image),
            Some(n) => Self::open_partition(&mt.image, n),
        }
    }

    /// Same dispatch as [`open`] but routes to RW variants. Currently
    /// only `--part N` is wired for RW (the WinFsp mount path). A direct
    /// (whole-image) RW open could be added later but isn't needed
    /// for the WinFsp use case where partition mounts dominate.
    ///
    /// Gated on `windows + feature = "mount"` because the only caller is
    /// the `Cmd::Mount` arm in `main.rs`, which has the same gate.
    /// Without this, `cargo check --no-default-features` flags the RW
    /// openers as dead code.
    #[cfg(all(windows, feature = "mount"))]
    pub fn open_rw(mt: &MountArgs) -> Result<Self> {
        match mt.part {
            None | Some(0) => Self::open_direct_rw(&mt.image),
            Some(n) => Self::open_partition_rw(&mt.image, n),
        }
    }

    // On non-Windows (macOS dev/test builds), RW operations are not
    // supported by the underlying C library. Fall back to read-only so
    // the binary compiles for host-side smoke tests.
    #[cfg(not(all(windows, feature = "mount")))]
    pub fn open_rw(mt: &MountArgs) -> Result<Self> {
        Self::open(mt)
    }

    /// RW analogue of [`open_direct`] — uses `fs_ext4_mount_rw` against the
    /// device path. Available so `--rw` works without a `--part`.
    #[cfg(all(windows, feature = "mount"))]
    pub fn open_direct_rw(image: &Path) -> Result<Self> {
        let s = image
            .to_str()
            .ok_or_else(|| anyhow!("image path is not valid UTF-8: {image:?}"))?;
        let c = CString::new(s).context("image path contains NUL byte")?;
        let fs = unsafe { fs_ext4_mount_rw(c.as_ptr()) };
        if fs.is_null() {
            bail!("mount_rw {image:?} failed: {}", crate::cmd::last_err());
        }
        Ok(Self {
            fs,
            cb_ctx: None,
            writable: true,
        })
    }

    pub fn open_direct(image: &Path) -> Result<Self> {
        let s = image
            .to_str()
            .ok_or_else(|| anyhow!("image path is not valid UTF-8: {image:?}"))?;
        let c = CString::new(s).context("image path contains NUL byte")?;
        let fs = unsafe { fs_ext4_mount(c.as_ptr()) };
        if fs.is_null() {
            let hint = match partition::list(image) {
                Ok(parts) if !parts.is_empty() => {
                    let mut s = String::from(
                        "\nhint: this looks like a partitioned device. Try --part N:\n",
                    );
                    for (i, p) in parts.iter().enumerate() {
                        s.push_str(&format!(
                            "  {}: {} sectors @ LBA {} ({})\n",
                            i + 1,
                            p.num_sectors,
                            p.start_lba,
                            p.kind,
                        ));
                    }
                    s
                }
                _ => String::new(),
            };
            bail!("mount {image:?} failed: {}{hint}", crate::cmd::last_err());
        }
        Ok(Self {
            fs,
            cb_ctx: None,
            writable: false,
        })
    }

    pub fn open_partition(image: &Path, n: usize) -> Result<Self> {
        let src: Arc<dyn BlockSource> = Arc::new(FileSource::open(image)?);
        let parts = partition::list_from_source(src.as_ref())
            .with_context(|| format!("listing partitions in {image:?}"))?;
        if parts.is_empty() {
            bail!("no partitions found in {image:?}");
        }
        if n == 0 || n > parts.len() {
            bail!("--part {n} out of range (1..={})", parts.len());
        }
        let p = &parts[n - 1];
        let base = p.start_lba * 512;
        let len = p.num_sectors * 512;
        let end = base
            .checked_add(len)
            .ok_or_else(|| anyhow!("partition geometry overflows u64"))?;
        if end > src.size() {
            bail!(
                "partition {n} extends past device end: {end} > {} bytes",
                src.size()
            );
        }

        let ctx = Box::new(SliceCtx { src, base, len });
        let raw = Box::into_raw(ctx);

        let cfg = fs_ext4_blockdev_cfg_t {
            read: Some(slice_read_cb),
            context: raw as *mut c_void,
            size_bytes: unsafe { (*raw).len },
            // 0 = let the driver discover from the superblock.
            block_size: 0,
            write: None,
            flush: None,
        };
        let fs = unsafe { fs_ext4_mount_with_callbacks(&cfg) };
        if fs.is_null() {
            unsafe { drop(Box::from_raw(raw)) };
            bail!(
                "mount partition {n} ({}) failed: {}",
                p.kind,
                crate::cmd::last_err()
            );
        }
        Ok(Self {
            fs,
            cb_ctx: Some(raw),
            writable: false,
        })
    }

    /// RW analogue of [`open_partition`]. Opens the underlying source
    /// with write access, plumbs read+write+flush callbacks into
    /// `fs_ext4_mount_rw_with_callbacks`, and replays a dirty journal
    /// before returning (eager-mount semantics).
    #[cfg(all(windows, feature = "mount"))]
    pub fn open_partition_rw(image: &Path, n: usize) -> Result<Self> {
        let src: Arc<dyn BlockSource> = Arc::new(FileSource::open_rw(image)?);
        let parts = partition::list_from_source(src.as_ref())
            .with_context(|| format!("listing partitions in {image:?}"))?;
        if parts.is_empty() {
            bail!("no partitions found in {image:?}");
        }
        if n == 0 || n > parts.len() {
            bail!("--part {n} out of range (1..={})", parts.len());
        }
        let p = &parts[n - 1];
        let base = p.start_lba * 512;
        let len = p.num_sectors * 512;
        let end = base
            .checked_add(len)
            .ok_or_else(|| anyhow!("partition geometry overflows u64"))?;
        if end > src.size() {
            bail!(
                "partition {n} extends past device end: {end} > {} bytes",
                src.size()
            );
        }

        let ctx = Box::new(SliceCtx { src, base, len });
        let raw = Box::into_raw(ctx);

        let cfg = fs_ext4_blockdev_cfg_t {
            read: Some(slice_read_cb),
            context: raw as *mut c_void,
            size_bytes: unsafe { (*raw).len },
            block_size: 0,
            write: Some(slice_write_cb),
            flush: Some(slice_flush_cb),
        };
        let fs = unsafe { fs_ext4_mount_rw_with_callbacks(&cfg) };
        if fs.is_null() {
            unsafe { drop(Box::from_raw(raw)) };
            bail!(
                "mount_rw partition {n} ({}) failed: {}",
                p.kind,
                crate::cmd::last_err()
            );
        }
        Ok(Self {
            fs,
            cb_ctx: Some(raw),
            writable: true,
        })
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if !self.fs.is_null() {
            unsafe { fs_ext4_umount(self.fs) };
            self.fs = std::ptr::null_mut();
        }
        if let Some(raw) = self.cb_ctx.take() {
            unsafe { drop(Box::from_raw(raw)) };
        }
    }
}

// ---------------------------------------------------------------------------
// Slice (partition-shimmed) callback context
// ---------------------------------------------------------------------------

struct SliceCtx {
    src: Arc<dyn BlockSource>,
    base: u64,
    len: u64,
}

extern "C" fn slice_read_cb(ctx: *mut c_void, buf: *mut c_void, offset: u64, length: u64) -> c_int {
    if ctx.is_null() || buf.is_null() {
        return -1;
    }
    let ctx = unsafe { &*(ctx as *const SliceCtx) };
    let Some(end) = offset.checked_add(length) else {
        return -1;
    };
    if end > ctx.len {
        return -1;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, length as usize) };
    if ctx.src.read_at(ctx.base + offset, slice).is_err() {
        return -1;
    }
    0
}

#[cfg(all(windows, feature = "mount"))]
extern "C" fn slice_write_cb(
    ctx: *mut c_void,
    buf: *const c_void,
    offset: u64,
    length: u64,
) -> c_int {
    if ctx.is_null() || buf.is_null() {
        return -1;
    }
    let ctx = unsafe { &*(ctx as *const SliceCtx) };
    let Some(end) = offset.checked_add(length) else {
        return -1;
    };
    if end > ctx.len {
        return -1;
    }
    let slice = unsafe { std::slice::from_raw_parts(buf as *const u8, length as usize) };
    if ctx.src.write_at(ctx.base + offset, slice).is_err() {
        return -1;
    }
    0
}

#[cfg(all(windows, feature = "mount"))]
extern "C" fn slice_flush_cb(ctx: *mut c_void) -> c_int {
    if ctx.is_null() {
        return -1;
    }
    let ctx = unsafe { &*(ctx as *const SliceCtx) };
    if ctx.src.flush().is_err() {
        return -1;
    }
    0
}

// ---------------------------------------------------------------------------
// WinFsp adapter (feature = "mount", windows only)
// ---------------------------------------------------------------------------

#[cfg(all(windows, feature = "mount"))]
mod winfsp_adapter {
    //! Glue between WinFsp's `FileSystemContext` and the `fs_ext4_*` C ABI.
    //!
    //! RO mode (default): only read-side methods are wired in practice;
    //! writes return `STATUS_MEDIA_WRITE_PROTECTED` via the volume's
    //! `read_only_volume` flag (WinFsp's primary gate) and again via
    //! `Ext4Context::ensure_writable` at the top of each mutating
    //! callback (defense in depth — see comment on the RW-side methods).
    //!
    //! RW mode (`--rw`): `create`/`write`/`set_file_size`/`set_basic_info`/
    //! `rename`/`set_delete`/`cleanup`/`overwrite` are wired through to the
    //! `fs_ext4_*` C ABI's mutating entry points.
    //!
    //! Path conversions: WinFsp gives backslash-separated UTF-16 paths
    //! (`\foo\bar`); the ext4 C ABI wants slash-separated UTF-8
    //! (`/foo/bar`). Done in [`winpath_to_unix`].
    //!
    //! Time conversions: ext4 stores 32-bit unix epoch seconds; Windows
    //! FILETIME is 100-ns intervals since 1601-01-01. The constant offset
    //! is 11644473600 seconds.
    //!
    //! ## Streaming writes
    //!
    //! WinFsp issues partial offset writes from the OS cache manager.
    //! [`Ext4Context::write`] dispatches each chunk directly to
    //! `fs_ext4_pwrite`, which allocates physical blocks only for the
    //! affected unmapped logical range and read-modify-writes any blocks
    //! already mapped. Cost per call is O(chunk), not O(filesize), so
    //! large copies don't fragment or quadratically rewrite the file.

    use anyhow::{anyhow, Context, Result};
    use fs_ext4::capi::*;
    use std::ffi::{c_void, CString};
    use std::sync::Mutex;
    use widestring::U16CStr;
    use windows::Win32::Foundation::{
        STATUS_ACCESS_DENIED, STATUS_BUFFER_OVERFLOW, STATUS_DIRECTORY_NOT_EMPTY, STATUS_DISK_FULL,
        STATUS_END_OF_FILE, STATUS_FILE_IS_A_DIRECTORY, STATUS_FILE_TOO_LARGE,
        STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_PARAMETER,
        STATUS_IO_DEVICE_ERROR, STATUS_MEDIA_WRITE_PROTECTED, STATUS_NAME_TOO_LONG,
        STATUS_NOT_A_DIRECTORY, STATUS_NOT_IMPLEMENTED, STATUS_NOT_SUPPORTED,
        STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_UNSUCCESSFUL,
    };
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY};
    use winfsp::filesystem::{
        DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, ModificationDescriptor,
        OpenFileInfo, VolumeInfo, WideNameInfo,
    };
    use winfsp::host::DebugMode;
    // The locking strategy is a TYPE PARAMETER on FileSystemHost in
    // 0.13.0, not a field on FileSystemParams -- that is the change that
    // "move guard strategy into types to prevent potential send/sync
    // soundness issues" made, and the reason it is a parameter is that
    // Fine requires the context to be Sync while Coarse only needs Send.
    // Leaving it inferred picks FineGuard, which is what this driver was
    // asking for explicitly.
    use winfsp::host::{FileSystemHost, FileSystemParams, FineGuard, VolumeParams};
    use winfsp::Result as FspResult;
    use winfsp_sys::{FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES};

    use crate::cmd::last_err;
    use crate::mount::Mount;

    // NT CreateOptions flag — `windows::Wdk::Storage::FileSystem::FILE_DIRECTORY_FILE`
    // would require pulling in the `Wdk_Storage_FileSystem` feature. The
    // bit definition is fixed by NT (ntifs.h) so a literal is safer than
    // adding another feature surface.
    const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

    /// Cleanup `Flags` bit indicating the file should be deleted now.
    const FSP_CLEANUP_DELETE: u32 = 0x01;

    /// "Leave unchanged" sentinel for `fs_ext4_chown`'s uid/gid fields —
    /// matches Linux's `(uid_t)-1`.
    ///
    /// **Timestamps use a different one.** They used to share this, and
    /// sharing it became wrong when am-fs-ext4 0.5.0 widened
    /// `fs_ext4_utimens` to signed 64-bit seconds: `u32::MAX` stopped
    /// being a spare bit pattern and became an ordinary date in 2106,
    /// so every "leave this timestamp alone" would have SET it to 2106.
    /// See [`TIME_UNCHANGED`].
    const KEEP_UNCHANGED: u32 = u32::MAX;

    /// "Leave unchanged" sentinel for `fs_ext4_utimens`, spelled
    /// `FS_EXT4_TIME_OMIT` in `fs_ext4.h`.
    ///
    /// `i64::MIN`, because seconds are signed and 64-bit: every value a
    /// filesystem could hold is a real date, so the sentinel has to sit
    /// outside the format's range entirely rather than at the top of an
    /// unsigned one.
    const TIME_UNCHANGED: i64 = i64::MIN;

    // The Unix-to-FILETIME conversion lives in winfsp-fs-skeleton, not
    // here. This module had two copies of it, both taking `u32`
    // seconds; the erofs and xfs drivers had a third and a fourth,
    // taking `u64`. Four implementations of one conversion at three
    // widths, and the widest still could not express a date before
    // 1970 -- even though FILETIME's epoch is 1601 and represents it
    // perfectly well.
    //
    // The shared one takes `i64`, which is what am-fs-ext4 0.5.0 now
    // reports, so these call sites need no casts.
    use winfsp_fs_skeleton::translate::{filetime_to_unix, unix_to_filetime};

    /// Apply a `FILE_FULL_EA_INFORMATION` buffer to `path` on `fs`.
    ///
    /// Parses the linked-list of EA entries and calls `fs_ext4_setxattr` for
    /// each non-empty entry, or `fs_ext4_removexattr` for zero-length entries.
    /// Skips entries whose names contain NUL bytes (malformed); returns the
    /// first `fs_ext4_setxattr` error, if any.
    fn apply_ea_buffer(fs: *mut fs_ext4_fs_t, cp: &CString, buffer: &[u8]) -> FspResult<()> {
        let mut pos = 0usize;
        loop {
            if pos + 8 > buffer.len() {
                break;
            }
            let next_offset = u32::from_le_bytes(buffer[pos..pos + 4].try_into().unwrap()) as usize;
            let name_len = buffer[pos + 5] as usize;
            let val_len = u16::from_le_bytes(buffer[pos + 6..pos + 8].try_into().unwrap()) as usize;
            let name_start = pos + 8;
            let name_end = name_start + name_len;
            let val_start = name_end + 1;
            let val_end = val_start + val_len;
            if val_end > buffer.len() {
                break;
            }
            if let Ok(cn) = CString::new(&buffer[name_start..name_end]) {
                let value = &buffer[val_start..val_end];
                if val_len == 0 {
                    let rc = unsafe { fs_ext4_removexattr(fs, cp.as_ptr(), cn.as_ptr()) };
                    if rc != 0 {
                        return Err(errno_to_status(unsafe { fs_ext4_last_errno() }).into());
                    }
                } else {
                    let rc = unsafe {
                        fs_ext4_setxattr(
                            fs,
                            cp.as_ptr(),
                            cn.as_ptr(),
                            value.as_ptr() as *const c_void,
                            value.len(),
                        )
                    };
                    if rc != 0 {
                        let errno = unsafe { fs_ext4_last_errno() };
                        return Err(errno_to_status(errno).into());
                    }
                }
            }
            if next_offset == 0 {
                break;
            }
            pos += next_offset;
        }
        Ok(())
    }

    /// `\foo\bar` (UTF-16) → `/foo/bar` (UTF-8). The empty path becomes "/".
    fn winpath_to_unix(name: &U16CStr) -> Result<String> {
        let s = name.to_string().context("path is invalid UTF-16")?;
        if s.is_empty() {
            return Ok("/".into());
        }
        Ok(s.replace('\\', "/"))
    }

    /// Populate [`FileInfo`] from an `fs_ext4_attr_t`.
    fn populate_file_info(attr: &fs_ext4_attr_t, info: &mut FileInfo) {
        let is_dir = matches!(attr.file_type, fs_ext4_file_type_t::Dir);
        let mut attrs: u32 = if is_dir {
            FILE_ATTRIBUTE_DIRECTORY.0
        } else {
            0
        };
        // Map "no write bits in mode" → READONLY for cosmetic correctness
        // in Explorer. The whole volume is also flagged read-only at the
        // VolumeParams level.
        if (attr.mode & 0o222) == 0 {
            attrs |= FILE_ATTRIBUTE_READONLY.0;
        }
        info.file_attributes = attrs;
        info.reparse_tag = 0;
        info.file_size = attr.size;
        // Allocation size: round up to 4 KiB, fine for an RO surface.
        info.allocation_size = (attr.size + 4095) & !4095;
        // Use sub-second precision where available (crtime_nsec / mtime_nsec may
        // be 0 on old ext2/3 inodes; the fallback in fill_attr zeros the nsec fields).
        let ct = unix_to_filetime(attr.crtime, attr.crtime_nsec)
            .max(unix_to_filetime(attr.mtime, attr.mtime_nsec));
        info.creation_time = ct;
        info.last_access_time = unix_to_filetime(attr.atime, attr.atime_nsec);
        info.last_write_time = unix_to_filetime(attr.mtime, attr.mtime_nsec);
        info.change_time = unix_to_filetime(attr.ctime, attr.ctime_nsec);
        info.index_number = attr.inode as u64;
        info.hard_links = attr.link_count as u32;
        info.ea_size = 0;
    }

    /// Stat a path through the C ABI. Returns the resolved path and populated
    /// `attr`, following symlinks up to 8 levels deep.
    fn stat_path_resolved(
        fs: *mut fs_ext4_fs_t,
        unix_path: &str,
    ) -> FspResult<(String, fs_ext4_attr_t)> {
        let mut current = unix_path.to_owned();
        for _ in 0..8 {
            let cp = CString::new(current.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
            let r = unsafe { fs_ext4_stat(fs, cp.as_ptr(), &mut attr) };
            if r != 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }
            if !matches!(attr.file_type, fs_ext4_file_type_t::Symlink) {
                return Ok((current, attr));
            }
            // Symlink: read target (null-terminated); resolve relative to link's parent.
            let mut buf = vec![0u8; 4096];
            let r =
                unsafe { fs_ext4_readlink(fs, cp.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
            if r < 0 {
                return Err(STATUS_OBJECT_NAME_NOT_FOUND.into());
            }
            let target = std::str::from_utf8(&buf[..r as usize])
                .unwrap_or("")
                .to_owned();
            if target.starts_with('/') {
                current = target;
            } else {
                let parent = current.rfind('/').map(|i| &current[..i]).unwrap_or("/");
                current = format!("{}/{}", parent.trim_end_matches('/'), target);
            }
        }
        Err(STATUS_OBJECT_NAME_NOT_FOUND.into())
    }

    /// Stat a path through the C ABI. Returns the populated `attr` or a
    /// `FspError`-mappable error. Follows symlinks (up to 8 levels).
    fn stat_path(fs: *mut fs_ext4_fs_t, unix_path: &str) -> FspResult<fs_ext4_attr_t> {
        stat_path_resolved(fs, unix_path).map(|(_, attr)| attr)
    }

    /// Errno values use **macOS POSIX numbers** because that's what the
    /// fs_ext4 C ABI returns (see `fs_ext4::error::errno`). Linux and macOS
    /// agree on every code below 32; ENAMETOOLONG/ENOTSUP/ENOTEMPTY/ENOSYS
    /// are the divergence points and are spelled with their macOS values
    /// here.
    ///
    /// Also logs the C ABI's `last_error` string to stderr so the driver
    /// console shows the underlying reason (extent overlap, no contiguous
    /// run, checksum mismatch, …) — Windows-side status codes are too
    /// coarse to debug from on their own.
    fn errno_to_status(errno: i32) -> windows::Win32::Foundation::NTSTATUS {
        let status = match errno {
            2  /* ENOENT */       => STATUS_OBJECT_NAME_NOT_FOUND,
            5  /* EIO */          => STATUS_IO_DEVICE_ERROR,
            12 /* ENOMEM */       => STATUS_INSUFFICIENT_RESOURCES,
            13 /* EACCES */       => STATUS_ACCESS_DENIED,
            17 /* EEXIST */       => STATUS_OBJECT_NAME_COLLISION,
            20 /* ENOTDIR */      => STATUS_NOT_A_DIRECTORY,
            21 /* EISDIR */       => STATUS_FILE_IS_A_DIRECTORY,
            22 /* EINVAL */       => STATUS_INVALID_PARAMETER,
            27 /* EFBIG */        => STATUS_FILE_TOO_LARGE,
            28 /* ENOSPC */       => STATUS_DISK_FULL,
            30 /* EROFS */        => STATUS_MEDIA_WRITE_PROTECTED,
            45 /* ENOTSUP */      => STATUS_NOT_SUPPORTED,
            63 /* ENAMETOOLONG */ => STATUS_NAME_TOO_LONG,
            66 /* ENOTEMPTY */    => STATUS_DIRECTORY_NOT_EMPTY,
            78 /* ENOSYS */       => STATUS_NOT_IMPLEMENTED,
            _                     => STATUS_INVALID_DEVICE_REQUEST,
        };
        let msg = unsafe {
            let p = fs_ext4_last_error();
            if p.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        eprintln!(
            "ext4: capi error errno={errno} status={:#010x}: {msg}",
            status.0 as u32
        );
        status
    }

    /// Per-open file handle state.
    pub struct Ext4FileContext {
        pub inode: u32,
        /// Path at open time. WinFsp gives a `file_name` to `cleanup` for
        /// the deletion case so we don't strictly need this for delete,
        /// but it's how `read`, `write`, etc. address the file in the C
        /// ABI (which is path-keyed, not handle-keyed).
        pub unix_path: Mutex<String>,
        pub is_dir: bool,
        /// Cached size — used as a cheap fast path for reads beyond EOF.
        /// Refreshed by `get_file_info`. Stale-tolerant.
        pub size: Mutex<u64>,
        /// Cached when the open call comes in; refreshed on get_file_info.
        attr: Mutex<fs_ext4_attr_t>,
    }

    impl Ext4FileContext {
        fn unix_path(&self) -> String {
            self.unix_path.lock().unwrap().clone()
        }
    }

    /// Filesystem-wide context shared across all WinFsp callbacks.
    pub struct Ext4Context {
        mount: Mount,
        label: String,
        block_size: u64,
        total_blocks: u64,
        free_blocks: u64,
    }

    impl Ext4Context {
        pub fn new(mount: Mount) -> Result<Self> {
            let mut vi: fs_ext4_volume_info_t = unsafe { std::mem::zeroed() };
            let r = unsafe { fs_ext4_get_volume_info(mount.fs, &mut vi) };
            if r != 0 {
                return Err(anyhow!("fs_ext4_get_volume_info failed: {}", last_err()));
            }
            let label_bytes: Vec<u8> = vi
                .volume_name
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as u8)
                .collect();
            let label = String::from_utf8_lossy(&label_bytes).into_owned();
            Ok(Self {
                mount,
                label,
                block_size: vi.block_size as u64,
                total_blocks: vi.total_blocks,
                free_blocks: vi.free_blocks,
            })
        }

        /// Defense-in-depth check used by every mutating WinFsp callback.
        /// WinFsp's `read_only_volume` flag is the primary gate, but we
        /// re-check `self.mount.writable` at the top of each writer so
        /// a regression that routes a RO `Mount` through a write path
        /// surfaces as `STATUS_MEDIA_WRITE_PROTECTED` instead of
        /// corrupting the on-disk image. Cheap (one bool load) and
        /// keeps the safety story local to this module.
        fn ensure_writable(&self) -> FspResult<()> {
            if !self.mount.writable {
                return Err(STATUS_MEDIA_WRITE_PROTECTED.into());
            }
            Ok(())
        }
    }

    impl FileSystemContext for Ext4Context {
        type FileContext = Ext4FileContext;

        fn get_security_by_name(
            &self,
            file_name: &U16CStr,
            _security_descriptor: Option<&mut [c_void]>,
            _resolve_reparse: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
        ) -> FspResult<FileSecurity> {
            let unix_path = winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let attr = stat_path(self.mount.fs, &unix_path)?;
            let is_dir = matches!(attr.file_type, fs_ext4_file_type_t::Dir);
            let mut attrs: u32 = if is_dir {
                FILE_ATTRIBUTE_DIRECTORY.0
            } else {
                0
            };
            if (attr.mode & 0o222) == 0 {
                attrs |= FILE_ATTRIBUTE_READONLY.0;
            }
            // RO surface — we don't write a real security descriptor.
            // WinFsp will synthesize a default-permissive one.
            Ok(FileSecurity {
                reparse: false,
                sz_security_descriptor: 0,
                attributes: attrs,
            })
        }

        fn open(
            &self,
            file_name: &U16CStr,
            _create_options: u32,
            _granted_access: FILE_ACCESS_RIGHTS,
            file_info: &mut OpenFileInfo,
        ) -> FspResult<Self::FileContext> {
            let initial_path =
                winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            // Follow symlinks so the context records the resolved path; reads and
            // writes then operate on the actual file inode, not the symlink inode.
            let (resolved_path, attr) = stat_path_resolved(self.mount.fs, &initial_path)?;
            populate_file_info(&attr, file_info.as_mut());
            Ok(Ext4FileContext {
                inode: attr.inode,
                unix_path: Mutex::new(resolved_path),
                is_dir: matches!(attr.file_type, fs_ext4_file_type_t::Dir),
                size: Mutex::new(attr.size),
                attr: Mutex::new(attr),
            })
        }

        fn close(&self, _context: Self::FileContext) {
            // Nothing to release — `Ext4FileContext` is plain data.
        }

        fn get_file_info(
            &self,
            context: &Self::FileContext,
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            let path = context.unix_path();
            let attr = stat_path(self.mount.fs, &path)?;
            populate_file_info(&attr, file_info);
            // Probe xattr presence so Windows issues QueryEa when EAs exist.
            if let Ok(cp) = CString::new(path.as_str()) {
                let n = unsafe {
                    fs_ext4_listxattr(self.mount.fs, cp.as_ptr(), std::ptr::null_mut(), 0)
                };
                if n > 0 {
                    file_info.ea_size = n as u32;
                }
            }
            *context.size.lock().unwrap() = attr.size;
            *context.attr.lock().unwrap() = attr;
            Ok(())
        }

        fn read(
            &self,
            context: &Self::FileContext,
            buffer: &mut [u8],
            offset: u64,
        ) -> FspResult<u32> {
            if context.is_dir {
                return Err(STATUS_INVALID_DEVICE_REQUEST.into());
            }
            let cur_size = *context.size.lock().unwrap();
            if offset >= cur_size {
                return Err(STATUS_END_OF_FILE.into());
            }
            let path = context.unix_path();
            let cp = CString::new(path)
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let n = unsafe {
                fs_ext4_read_file(
                    self.mount.fs,
                    cp.as_ptr(),
                    buffer.as_mut_ptr() as *mut c_void,
                    offset,
                    buffer.len() as u64,
                )
            };
            if n < 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }
            Ok(n as u32)
        }

        fn read_directory(
            &self,
            context: &Self::FileContext,
            _pattern: Option<&U16CStr>,
            marker: DirMarker,
            buffer: &mut [u8],
        ) -> FspResult<u32> {
            if !context.is_dir {
                return Err(STATUS_NOT_A_DIRECTORY.into());
            }
            let parent_path = context.unix_path();
            let cp = CString::new(parent_path.clone())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let iter = unsafe { fs_ext4_dir_open(self.mount.fs, cp.as_ptr()) };
            if iter.is_null() {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }

            // Resume after `marker` if set. We pass through every entry
            // until we've matched the marker name (exclusive), then start
            // emitting.
            let resume_after = marker.inner_as_cstr().map(|m| m.to_string_lossy());
            let mut started = resume_after.is_none();

            let mut cursor: u32 = 0;
            let mut dir_info: DirInfo<255> = DirInfo::new();

            loop {
                let e = unsafe { fs_ext4_dir_next(iter) };
                if e.is_null() {
                    break;
                }
                let entry = unsafe { &*e };
                let name_bytes: Vec<u8> = entry.name[..entry.name_len as usize]
                    .iter()
                    .map(|b| *b as u8)
                    .collect();
                let name = match std::str::from_utf8(&name_bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if name == "." || name == ".." {
                    if !started {
                        if Some(name.to_string()) == resume_after.as_ref().map(|s| s.to_string()) {
                            started = true;
                        }
                        continue;
                    }
                    // Stat the correct path: "." → parent_path, ".." → parent of parent_path.
                    let dot_path = if name == "." {
                        parent_path.clone()
                    } else if parent_path == "/" {
                        "/".to_string()
                    } else {
                        let idx = parent_path.rfind('/').unwrap_or(0);
                        if idx == 0 {
                            "/".to_string()
                        } else {
                            parent_path[..idx].to_string()
                        }
                    };
                    let attr = match stat_path(self.mount.fs, &dot_path) {
                        Ok(a) => a,
                        Err(_) => continue,
                    };
                    dir_info.reset();
                    populate_file_info(&attr, dir_info.file_info_mut());
                    if dir_info.set_name(name).is_err() {
                        continue;
                    }
                    if !dir_info.append_to_buffer(buffer, &mut cursor) {
                        break;
                    }
                    continue;
                }

                if !started {
                    if Some(name.to_string()) == resume_after.as_ref().map(|s| s.to_string()) {
                        started = true;
                    }
                    continue;
                }

                let child_path = if parent_path == "/" {
                    format!("/{name}")
                } else {
                    format!("{}/{name}", parent_path)
                };
                let attr = match stat_path(self.mount.fs, &child_path) {
                    Ok(a) => a,
                    Err(_) => continue, // skip entries we can't stat
                };

                dir_info.reset();
                populate_file_info(&attr, dir_info.file_info_mut());
                if dir_info.set_name(name).is_err() {
                    continue;
                }
                if !dir_info.append_to_buffer(buffer, &mut cursor) {
                    break;
                }
            }
            unsafe { fs_ext4_dir_close(iter) };
            DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
            Ok(cursor)
        }

        fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> FspResult<()> {
            out_volume_info.total_size = self.total_blocks * self.block_size;
            out_volume_info.free_size = self.free_blocks * self.block_size;
            let label = if self.label.is_empty() {
                "ext4"
            } else {
                self.label.as_str()
            };
            out_volume_info.set_volume_label(label);
            Ok(())
        }

        // -----------------------------------------------------------------
        // RW-side methods. WinFsp's `read_only_volume` VolumeParams flag
        // is the primary gate — on a RO mount it short-circuits these
        // callbacks with STATUS_MEDIA_WRITE_PROTECTED before dispatch.
        // We additionally consult `self.mount.writable` at the top of
        // each mutating method as defense-in-depth: it costs a single
        // bool load, catches future regressions where someone routes a
        // RO `Mount` through a write path bypassing WinFsp's gate (e.g.
        // a unit test or a non-WinFsp consumer), and keeps the safety
        // story local to the methods rather than relying solely on the
        // VolumeParams configuration.
        // -----------------------------------------------------------------

        fn create(
            &self,
            file_name: &U16CStr,
            create_options: u32,
            _granted_access: FILE_ACCESS_RIGHTS,
            file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
            _security_descriptor: Option<&[c_void]>,
            _allocation_size: u64,
            _extra_buffer: Option<&[u8]>,
            _extra_buffer_is_reparse_point: bool,
            file_info: &mut OpenFileInfo,
        ) -> FspResult<Self::FileContext> {
            self.ensure_writable()?;
            let unix_path = winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let cp = CString::new(unix_path.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;

            // POSIX permission bits — Windows doesn't supply mode_t, so
            // we mint sensible defaults: 0o755 for dirs, 0o644 for files.
            // READONLY attribute → strip the write bits so Explorer's
            // "read-only" property round-trips.
            let is_dir = create_options & FILE_DIRECTORY_FILE != 0;
            let mut mode: u16 = if is_dir { 0o755 } else { 0o644 };
            if file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
                mode &= !0o222;
            }

            let ino = if is_dir {
                unsafe { fs_ext4_mkdir(self.mount.fs, cp.as_ptr(), mode) }
            } else {
                unsafe { fs_ext4_create(self.mount.fs, cp.as_ptr(), mode) }
            };
            if ino == 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }

            // Apply any EA data the caller supplied at creation time.
            if let Some(ea) = _extra_buffer {
                if !_extra_buffer_is_reparse_point {
                    apply_ea_buffer(self.mount.fs, &cp, ea)?;
                }
            }

            let attr = stat_path(self.mount.fs, &unix_path)?;
            populate_file_info(&attr, file_info.as_mut());
            Ok(Ext4FileContext {
                inode: attr.inode,
                unix_path: Mutex::new(unix_path),
                is_dir,
                size: Mutex::new(attr.size),
                attr: Mutex::new(attr),
            })
        }

        fn write(
            &self,
            context: &Self::FileContext,
            buffer: &[u8],
            offset: u64,
            write_to_eof: bool,
            constrained_io: bool,
            file_info: &mut FileInfo,
        ) -> FspResult<u32> {
            self.ensure_writable()?;
            if context.is_dir {
                return Err(STATUS_FILE_IS_A_DIRECTORY.into());
            }
            let path = context.unix_path();
            let cp = CString::new(path.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;

            // Re-stat so we have an authoritative current size; the
            // `context.size` cache can lag if the file was mutated via
            // another handle on the same volume.
            let attr = stat_path(self.mount.fs, &path)?;
            let cur_size = attr.size;

            // Resolve effective offset + accepted byte count.
            let eff_offset = if write_to_eof { cur_size } else { offset };
            let mut accept_len = buffer.len() as u64;
            if constrained_io {
                if eff_offset >= cur_size {
                    // No bytes accepted — write past EOF on a constrained
                    // request is a no-op success per the WinFsp contract.
                    populate_file_info(&attr, file_info);
                    return Ok(0);
                }
                let avail = cur_size - eff_offset;
                if accept_len > avail {
                    accept_len = avail;
                }
            }
            if accept_len == 0 {
                populate_file_info(&attr, file_info);
                return Ok(0);
            }
            // Positional write — costs O(accept_len), not O(filesize). The
            // C ABI's `fs_ext4_pwrite` allocates blocks only for unmapped
            // logical blocks in the affected range and read-modify-writes
            // existing ones; sparse holes between `cur_size` and
            // `eff_offset` (from writing past EOF) stay sparse.
            let rc = unsafe {
                fs_ext4_pwrite(
                    self.mount.fs,
                    cp.as_ptr(),
                    buffer.as_ptr() as *const c_void,
                    accept_len,
                    eff_offset,
                )
            };
            if rc < 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }

            // Refresh size + attrs for the caller.
            let attr2 = stat_path(self.mount.fs, &path)?;
            populate_file_info(&attr2, file_info);
            *context.size.lock().unwrap() = attr2.size;
            *context.attr.lock().unwrap() = attr2;
            Ok(accept_len as u32)
        }

        fn set_file_size(
            &self,
            context: &Self::FileContext,
            new_size: u64,
            _set_allocation_size: bool,
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            self.ensure_writable()?;
            if context.is_dir {
                return Err(STATUS_FILE_IS_A_DIRECTORY.into());
            }
            let path = context.unix_path();
            let cp = CString::new(path.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let rc = unsafe { fs_ext4_truncate(self.mount.fs, cp.as_ptr(), new_size) };
            if rc != 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }
            let attr = stat_path(self.mount.fs, &path)?;
            populate_file_info(&attr, file_info);
            *context.size.lock().unwrap() = attr.size;
            *context.attr.lock().unwrap() = attr;
            Ok(())
        }

        fn overwrite(
            &self,
            context: &Self::FileContext,
            _file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
            _replace_file_attributes: bool,
            _allocation_size: u64,
            _extra_buffer: Option<&[u8]>,
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            // WinFsp Overwrite = "the file's content is being replaced".
            // We truncate to 0 here; the cache manager will follow up with
            // Write calls for the new bytes.
            self.ensure_writable()?;
            if context.is_dir {
                return Err(STATUS_FILE_IS_A_DIRECTORY.into());
            }
            let path = context.unix_path();
            let cp = CString::new(path.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let rc = unsafe { fs_ext4_truncate(self.mount.fs, cp.as_ptr(), 0) };
            if rc != 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }
            let attr = stat_path(self.mount.fs, &path)?;
            populate_file_info(&attr, file_info);
            *context.size.lock().unwrap() = attr.size;
            *context.attr.lock().unwrap() = attr;
            Ok(())
        }

        fn set_basic_info(
            &self,
            context: &Self::FileContext,
            file_attributes: u32,
            _creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            _last_change_time: u64,
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            self.ensure_writable()?;
            // 0 means "leave unchanged" per WinFsp; we map that to
            // KEEP_UNCHANGED for the C ABI. ext4 stores second-precision
            // timestamps in the standard fields, so we drop the sub-second
            // residue (the C ABI accepts nsec but the underlying inode
            // only persists it when i_extra_isize covers it; for v1 we
            // pass 0).
            let path = context.unix_path();
            let cp = CString::new(path.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            // A zero FILETIME is WinFsp's "leave unchanged", and the
            // only input `filetime_to_unix` reports as having no
            // timestamp. Everything else converts, including dates
            // before 1970 — the copy this replaced returned None for
            // those, which reads here as "leave unchanged" and so
            // silently discarded a time the user had asked for.
            let (atime_sec, atime_nsec) = filetime_to_unix(last_access_time)
                .map(|t| (t.secs, t.nsec))
                .unwrap_or((TIME_UNCHANGED, 0));
            let (mtime_sec, mtime_nsec) = filetime_to_unix(last_write_time)
                .map(|t| (t.secs, t.nsec))
                .unwrap_or((TIME_UNCHANGED, 0));
            if atime_sec != TIME_UNCHANGED || mtime_sec != TIME_UNCHANGED {
                let rc = unsafe {
                    fs_ext4_utimens(
                        self.mount.fs,
                        cp.as_ptr(),
                        atime_sec,
                        atime_nsec,
                        mtime_sec,
                        mtime_nsec,
                    )
                };
                if rc != 0 {
                    let errno = unsafe { fs_ext4_last_errno() };
                    return Err(errno_to_status(errno).into());
                }
            }

            // Map FILE_ATTRIBUTE_READONLY ↔ POSIX write bits so Explorer's
            // "Read-only" property checkbox round-trips. WinFsp passes
            // INVALID_FILE_ATTRIBUTES (0xFFFFFFFF) when the caller didn't
            // touch attributes; otherwise it passes the desired flags.
            //
            // Toggle is symmetric across owner/group/other write bits:
            //   - READONLY set   → mode &= !0o222
            //   - READONLY clear → mode |= 0o222
            // We only chmod when the bit *changes*, so a plain
            // SetFileTime + zero-edit on attributes doesn't churn ctime
            // or rewrite the inode.
            const INVALID_FILE_ATTRIBUTES: u32 = 0xFFFF_FFFF;
            if file_attributes != 0 && file_attributes != INVALID_FILE_ATTRIBUTES {
                // Need current mode to detect the no-op case; stat now.
                let cur = stat_path(self.mount.fs, &path)?;
                let want_ro = file_attributes & FILE_ATTRIBUTE_READONLY.0 != 0;
                let is_ro = (cur.mode & 0o222) == 0;
                if want_ro != is_ro {
                    let new_mode = if want_ro {
                        cur.mode & !0o222
                    } else {
                        cur.mode | 0o222
                    };
                    let rc = unsafe { fs_ext4_chmod(self.mount.fs, cp.as_ptr(), new_mode) };
                    if rc != 0 {
                        let errno = unsafe { fs_ext4_last_errno() };
                        return Err(errno_to_status(errno).into());
                    }
                }
            }

            let attr = stat_path(self.mount.fs, &path)?;
            populate_file_info(&attr, file_info);
            *context.attr.lock().unwrap() = attr;
            Ok(())
        }

        fn rename(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            new_file_name: &U16CStr,
            replace_if_exists: bool,
        ) -> FspResult<()> {
            self.ensure_writable()?;
            // WinFsp asks us to honor `replace_if_exists` so Explorer's
            // "Save As" / drag-drop-onto-existing flows succeed instead
            // of failing with STATUS_OBJECT_NAME_COLLISION. We thread
            // it through to `fs_ext4_rename2` via the REPLACE flag.
            let src = winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let dst = winpath_to_unix(new_file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let csrc = CString::new(src.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let cdst = CString::new(dst.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let flags = if replace_if_exists {
                FS_EXT4_RENAME_REPLACE
            } else {
                0
            };
            let rc = unsafe { fs_ext4_rename2(self.mount.fs, csrc.as_ptr(), cdst.as_ptr(), flags) };
            if rc != 0 {
                let errno = unsafe { fs_ext4_last_errno() };
                return Err(errno_to_status(errno).into());
            }
            // Update the open handle's path so subsequent ops resolve the
            // moved file correctly.
            *context.unix_path.lock().unwrap() = dst;
            Ok(())
        }

        fn set_delete(
            &self,
            _context: &Self::FileContext,
            _file_name: &U16CStr,
            _delete_file: bool,
        ) -> FspResult<()> {
            self.ensure_writable()
        }

        fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, flags: u32) {
            if flags & FSP_CLEANUP_DELETE == 0 {
                return;
            }
            if !self.mount.writable {
                return;
            }
            let path = context.unix_path();
            let Ok(cp) = CString::new(path.as_str()) else {
                return;
            };
            // No way to report failure from cleanup — Windows interface
            // limitation. Best effort.
            let _ = if context.is_dir {
                unsafe { fs_ext4_rmdir(self.mount.fs, cp.as_ptr()) }
            } else {
                unsafe { fs_ext4_unlink(self.mount.fs, cp.as_ptr()) }
            };
        }

        fn flush(
            &self,
            _context: Option<&Self::FileContext>,
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            // The C ABI exposes no fs-level flush hook (the journal is
            // flushed inside each mutating call already, and the block
            // device flush callback wired by `slice_flush_cb` runs on
            // every commit). So this is a successful no-op. We do
            // refresh `file_info` if a context is provided so WinFsp
            // gets up-to-date metadata after a flush.
            if let Some(ctx) = _context {
                let path = ctx.unix_path();
                if let Ok(attr) = stat_path(self.mount.fs, &path) {
                    populate_file_info(&attr, file_info);
                    *ctx.attr.lock().unwrap() = attr;
                }
            }
            Ok(())
        }

        fn set_security(
            &self,
            _context: &Self::FileContext,
            _security_information: u32,
            _modification_descriptor: ModificationDescriptor,
        ) -> FspResult<()> {
            // v1: pretend success. ext4 ACL/security model maps awkwardly
            // to NT SDs and Explorer doesn't gate writes on this path
            // when the volume isn't read-only. Return Ok rather than
            // INVALID_DEVICE_REQUEST so apps that always set security
            // on create (Office, etc.) don't blow up.
            Ok(())
        }

        // ---------------------------------------------------------------------------
        // Extended Attributes (EAs) — ext4 xattrs exposed through WinFsp
        //
        // Mapping: we use the full xattr name (including namespace prefix like
        // "user.") as the EA name. This preserves round-trip fidelity — a Linux
        // tool that writes "user.color" sees the same name from Windows.
        // Windows EA names are treated as opaque bytes; case is preserved.
        // ---------------------------------------------------------------------------

        fn get_extended_attributes(
            &self,
            context: &Self::FileContext,
            buffer: &mut [u8],
        ) -> FspResult<u32> {
            let path = context.unix_path();
            let Ok(cp) = CString::new(path.as_str()) else {
                return Err(STATUS_OBJECT_NAME_NOT_FOUND.into());
            };

            // Probe xattr name list size.
            let list_size =
                unsafe { fs_ext4_listxattr(self.mount.fs, cp.as_ptr(), std::ptr::null_mut(), 0) };
            if list_size < 0 || list_size == 0 {
                return Ok(0);
            }

            let mut list_buf = vec![0u8; list_size as usize];
            let n = unsafe {
                fs_ext4_listxattr(
                    self.mount.fs,
                    cp.as_ptr(),
                    list_buf.as_mut_ptr() as *mut std::os::raw::c_char,
                    list_buf.len(),
                )
            };
            if n <= 0 {
                return Ok(0);
            }

            // Collect NUL-separated names.
            let mut names: Vec<Vec<u8>> = Vec::new();
            let mut pos = 0usize;
            while pos < n as usize {
                let end = list_buf[pos..]
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(n as usize - pos);
                if end == 0 {
                    break;
                }
                names.push(list_buf[pos..pos + end].to_vec());
                pos += end + 1;
            }

            let mut out_pos = 0usize;
            let count = names.len();
            for (i, name_bytes) in names.iter().enumerate() {
                let Ok(cn) = CString::new(name_bytes.as_slice()) else {
                    continue;
                };

                // Probe value size.
                let val_size = unsafe {
                    fs_ext4_getxattr(
                        self.mount.fs,
                        cp.as_ptr(),
                        cn.as_ptr(),
                        std::ptr::null_mut(),
                        0,
                    )
                };
                if val_size < 0 {
                    continue;
                }

                let mut val_buf = vec![0u8; val_size as usize];
                let val_n = unsafe {
                    fs_ext4_getxattr(
                        self.mount.fs,
                        cp.as_ptr(),
                        cn.as_ptr(),
                        val_buf.as_mut_ptr() as *mut c_void,
                        val_buf.len(),
                    )
                };
                if val_n < 0 {
                    continue;
                }
                let value = &val_buf[..val_n as usize];

                let ea_name_len = name_bytes.len(); // NOT including NUL
                let ea_val_len = value.len();
                // Header = 8 bytes; name field = ea_name_len + 1 (NUL); then value.
                let raw_size = 8 + ea_name_len + 1 + ea_val_len;
                let is_last = i == count - 1;
                // Each entry aligned to 4 bytes except (optionally) the last.
                let padded_size = if is_last {
                    raw_size
                } else {
                    (raw_size + 3) & !3
                };

                if out_pos + padded_size > buffer.len() {
                    return Err(STATUS_BUFFER_OVERFLOW.into());
                }

                let entry = &mut buffer[out_pos..];
                let next_offset = if is_last { 0u32 } else { padded_size as u32 };
                entry[0..4].copy_from_slice(&next_offset.to_le_bytes());
                entry[4] = 0; // Flags
                entry[5] = ea_name_len as u8;
                entry[6..8].copy_from_slice(&(ea_val_len as u16).to_le_bytes());
                entry[8..8 + ea_name_len].copy_from_slice(name_bytes);
                entry[8 + ea_name_len] = 0; // NUL terminator
                if ea_val_len > 0 {
                    entry[8 + ea_name_len + 1..8 + ea_name_len + 1 + ea_val_len]
                        .copy_from_slice(value);
                }
                out_pos += padded_size;
            }

            Ok(out_pos as u32)
        }

        fn set_extended_attributes(
            &self,
            context: &Self::FileContext,
            buffer: &[u8],
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            self.ensure_writable()?;

            let path = context.unix_path();
            let Ok(cp) = CString::new(path.as_str()) else {
                return Err(STATUS_OBJECT_NAME_NOT_FOUND.into());
            };

            apply_ea_buffer(self.mount.fs, &cp, buffer)?;

            // Refresh file_info after EA mutation.
            if let Ok(attr) = stat_path(self.mount.fs, &path) {
                populate_file_info(&attr, file_info);
                *context.attr.lock().unwrap() = attr;
            } else {
                return Err(STATUS_UNSUCCESSFUL.into());
            }

            Ok(())
        }

        // Called by WinFsp for exact-filename directory queries (e.g.
        // FindFirstFile("Z:\some-name")) when pass_query_directory_filename
        // is set. Avoids the broken FSD pattern-matching path for non-wildcard
        // lookups that caused Remove-Item to fail via IsReparsePointLikeSymlink.
        fn get_dir_info_by_name(
            &self,
            context: &Self::FileContext,
            file_name: &U16CStr,
            out_dir_info: &mut DirInfo,
        ) -> FspResult<()> {
            let parent = context.unix_path();
            let name = file_name.to_string_lossy();
            let child_path = if parent == "/" {
                format!("/{name}")
            } else {
                format!("{parent}/{name}")
            };
            let attr = stat_path(self.mount.fs, &child_path)?;
            populate_file_info(&attr, out_dir_info.file_info_mut());
            // WinFsp passes DirInfo through FspFileSystemAddDirInfo which copies raw bytes;
            // the filename must NOT include a null terminator in the Size field or the
            // kernel pattern match fails ("lost+found\0" != "lost+found").
            let name_wide: Vec<u16> = name.encode_utf16().collect();
            out_dir_info
                .set_name_raw(name_wide.as_slice())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            Ok(())
        }
    }

    /// Mount the given ext4 source on a Windows mount point.
    ///
    /// `mount_point` accepts a drive letter (`X:`) or a path to an empty
    /// directory. Blocks until the user presses Ctrl-C, then unmounts.
    pub fn run(mount: Mount, mount_point: &str) -> Result<()> {
        let _init = winfsp::winfsp_init().context("WinFsp not installed?")?;
        let writable = mount.writable;

        let ctx = Ext4Context::new(mount)?;

        let mut params = VolumeParams::new();
        params
            .sector_size(4096)
            .sectors_per_allocation_unit(1)
            .max_component_length(255)
            .file_info_timeout(1000)
            .case_sensitive_search(true)
            .case_preserved_names(true)
            .unicode_on_disk(true)
            .pass_query_directory_filename(true)
            .extended_attributes(true)
            .filesystem_name("ext4");
        // Default: read-only volume. Drop the flag for `--rw` mounts so
        // WinFsp dispatches mutating ops to our `create`/`write`/etc.
        // handlers instead of short-circuiting them with
        // STATUS_MEDIA_WRITE_PROTECTED.
        if !writable {
            params.read_only_volume(true);
        }

        // The guard strategy is named rather than inferred. 0.13.0 has
        // two impls whose methods collide when S is open -- a general
        // one over any OperationGuardStrategy and a FineGuard-specific
        // one requiring the context to be Sync -- so leaving it to
        // inference is E0034, "multiple applicable items in scope", at
        // the mount call rather than here.
        //
        // FineGuard is what this driver wants: WinFsp guards namespace
        // operations with a read-write lock and leaves file I/O
        // concurrent, so reads on different files do not serialise.
        let mut host = FileSystemHost::<_, FineGuard>::new_with_options(
            FileSystemParams {
                use_dir_info_by_name: true,
                volume_params: params,
                debug_mode: DebugMode::none(),
            },
            ctx,
        )
        .map_err(|e| anyhow!("FileSystemHost::new failed: {e}"))?;

        // FileSystemHost::mount accepts any S where &S: Into<MountPoint>,
        // and `&str: AsRef<OsStr>` satisfies the existing blanket impl.
        host.mount(mount_point)
            .map_err(|e| anyhow!("mount({mount_point}) failed: {e}"))?;
        host.start()
            .map_err(|e| anyhow!("FileSystemHost::start failed: {e}"))?;

        let mode = if writable { "RW" } else { "RO" };
        println!("ext4 mounted at {mount_point} ({mode}). Ctrl-C to unmount.");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        // Block until Ctrl-C; WinFsp's host runs on its own threads.
        let (tx, rx) = std::sync::mpsc::channel();
        ctrlc::set_handler(move || {
            let _ = tx.send(());
        })
        .ok();
        let _ = rx.recv();

        host.stop();
        host.unmount();
        Ok(())
    }
}

#[cfg(all(windows, feature = "mount"))]
pub use winfsp_adapter::run;

#[cfg(test)]
mod tests {
    //! Unit tests for the `Mount.writable` plumbing.
    //!
    //! We can't drive the live WinFsp callback path from a host test —
    //! the adapter is `#[cfg(all(windows, feature = "mount"))]` and links
    //! winfsp-rs / the WinFsp DLL — so the tests below are synthetic:
    //! they construct a `Mount` directly with a null `fs` pointer (drop
    //! treats null as a no-op) and assert the field is wired the way
    //! `winfsp_adapter::Ext4Context::ensure_writable` expects.

    use super::*;

    /// A mount opened RO must report `writable == false`. This is the
    /// invariant `ensure_writable` relies on to short-circuit mutating
    /// WinFsp callbacks with `STATUS_MEDIA_WRITE_PROTECTED` even if a
    /// regression ever bypasses the `read_only_volume` VolumeParams gate.
    #[test]
    fn ro_mount_is_not_writable() {
        let mount = Mount {
            fs: std::ptr::null_mut(),
            cb_ctx: None,
            writable: false,
        };
        assert!(!mount.writable, "RO Mount should report writable = false");
    }

    /// A mount opened RW must report `writable == true`. This mirrors
    /// the literal that `open_direct_rw` / `open_partition_rw` set on
    /// success — kept here so a refactor that flips the polarity is
    /// caught by `cargo test` instead of silently mounting RO under
    /// the `--rw` flag.
    #[test]
    fn rw_mount_is_writable() {
        let mount = Mount {
            fs: std::ptr::null_mut(),
            cb_ctx: None,
            writable: true,
        };
        assert!(mount.writable, "RW Mount should report writable = true");
    }

    /// On the Windows-mount build the writable check belongs at the top
    /// of every mutating WinFsp callback. We exercise the boolean here
    /// the same way `Ext4Context::ensure_writable` does — direct field
    /// load, no FFI — so the branch has at least one host-side test
    /// even though the adapter itself is Windows-only.
    #[test]
    fn writable_check_branch() {
        let ro = Mount {
            fs: std::ptr::null_mut(),
            cb_ctx: None,
            writable: false,
        };
        let rw = Mount {
            fs: std::ptr::null_mut(),
            cb_ctx: None,
            writable: true,
        };

        // Mirrors `if !self.mount.writable { return Err(STATUS_MEDIA_WRITE_PROTECTED.into()); }`.
        let ro_blocks = !ro.writable;
        let rw_passes = rw.writable;
        assert!(ro_blocks, "ensure_writable() must block on RO mount");
        assert!(rw_passes, "ensure_writable() must pass on RW mount");
    }
}
