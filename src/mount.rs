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

use anyhow::{Context, Result, anyhow, bail};
use fs_ext4::capi::*;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::path::Path;
use std::sync::Arc;

use crate::MountArgs;
use crate::device::{BlockSource, FileSource};
use crate::partition;

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
    /// True when mounted RW. Used by the WinFsp adapter to gate write
    /// callbacks and drop the `read_only_volume` flag.
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
    pub fn open_rw(mt: &MountArgs) -> Result<Self> {
        match mt.part {
            None | Some(0) => Self::open_direct_rw(&mt.image),
            Some(n) => Self::open_partition_rw(&mt.image, n),
        }
    }

    /// RW analogue of [`open_direct`] — uses `fs_ext4_mount_rw` against the
    /// device path. Available so `--rw` works without a `--part`.
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

extern "C" fn slice_read_cb(
    ctx: *mut c_void,
    buf: *mut c_void,
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
    let slice = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, length as usize) };
    if ctx.src.read_at(ctx.base + offset, slice).is_err() {
        return -1;
    }
    0
}

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
    //! RO mode (default): only read-side methods are wired; writes return
    //! `STATUS_MEDIA_WRITE_PROTECTED` via the volume's `read_only_volume`
    //! flag.
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
    //! ## v1 write compromise
    //!
    //! `fs_ext4_write_file` in the C ABI is a *whole-file replace*, not a
    //! positional write. WinFsp issues partial offset writes from the OS
    //! cache manager. To bridge the gap, [`Ext4Context::write`] currently
    //! reads the entire existing file, splices the new bytes in at the
    //! requested offset, and calls `fs_ext4_write_file` with the merged
    //! buffer. This is correct but O(filesize) per write — fine for small
    //! files, painful for large ones. A positional `pwrite` C ABI is the
    //! follow-up; until it lands, large-file workloads will be slow.

    use anyhow::{Context, Result, anyhow};
    use fs_ext4::capi::*;
    use std::ffi::{CString, c_void};
    use std::sync::Mutex;
    use widestring::U16CStr;
    use winfsp::Result as FspResult;
    use winfsp::filesystem::{
        DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, ModificationDescriptor,
        OpenFileInfo, VolumeInfo, WideNameInfo,
    };
    use winfsp::host::{FileSystemHost, VolumeParams};
    use windows::Win32::Foundation::{
        STATUS_ACCESS_DENIED, STATUS_DIRECTORY_NOT_EMPTY, STATUS_DISK_FULL, STATUS_END_OF_FILE,
        STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_DEVICE_REQUEST, STATUS_MEDIA_WRITE_PROTECTED,
        STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    };
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

    /// "Leave unchanged" sentinel for `fs_ext4_chown` and `fs_ext4_utimens`
    /// fields — matches Linux's `(uid_t)-1` / `UTIME_OMIT`-equivalent on
    /// the C ABI side.
    const KEEP_UNCHANGED: u32 = u32::MAX;

    /// Seconds between Windows FILETIME epoch (1601-01-01) and Unix epoch (1970-01-01).
    const FILETIME_EPOCH_OFFSET_SEC: u64 = 11_644_473_600;

    /// Convert a unix-epoch-seconds timestamp to FILETIME (100-ns intervals
    /// since 1601). Saturating on overflow — ext4 timestamps fit in 32 bits
    /// (or 64 with high-precision attrs), well within u64 FILETIME range.
    fn unix_to_filetime(secs: u32) -> u64 {
        (FILETIME_EPOCH_OFFSET_SEC.saturating_add(secs as u64)).saturating_mul(10_000_000)
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
        info.creation_time = unix_to_filetime(attr.crtime.max(attr.mtime));
        info.last_access_time = unix_to_filetime(attr.atime);
        info.last_write_time = unix_to_filetime(attr.mtime);
        info.change_time = unix_to_filetime(attr.ctime);
        info.index_number = attr.inode as u64;
        info.hard_links = 0;
        info.ea_size = 0;
    }

    /// Stat a path through the C ABI. Returns the populated `attr` or a
    /// `FspError`-mappable error.
    fn stat_path(fs: *mut fs_ext4_fs_t, unix_path: &str) -> FspResult<fs_ext4_attr_t> {
        let cp = CString::new(unix_path)
            .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
        let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
        let r = unsafe { fs_ext4_stat(fs, cp.as_ptr(), &mut attr) };
        if r != 0 {
            let errno = unsafe { fs_ext4_last_errno() };
            return Err(errno_to_status(errno).into());
        }
        Ok(attr)
    }

    fn errno_to_status(errno: i32) -> windows::Win32::Foundation::NTSTATUS {
        // Coarse mapping. Refine when we hit specific cases in testing.
        match errno {
            2  /* ENOENT */    => STATUS_OBJECT_NAME_NOT_FOUND,
            13 /* EACCES */    => STATUS_ACCESS_DENIED,
            17 /* EEXIST */    => STATUS_OBJECT_NAME_COLLISION,
            20 /* ENOTDIR */   => STATUS_NOT_A_DIRECTORY,
            21 /* EISDIR */    => STATUS_FILE_IS_A_DIRECTORY,
            28 /* ENOSPC */    => STATUS_DISK_FULL,
            30 /* EROFS */     => STATUS_MEDIA_WRITE_PROTECTED,
            39 /* ENOTEMPTY */ => STATUS_DIRECTORY_NOT_EMPTY,
            _ => STATUS_INVALID_DEVICE_REQUEST,
        }
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
        /// Set by `set_delete`, consumed by `cleanup`. WinFsp guarantees
        /// `cleanup` runs after the last handle is closed, so this is the
        /// place where the actual `unlink`/`rmdir` happens.
        delete: Mutex<bool>,
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
                return Err(anyhow!(
                    "fs_ext4_get_volume_info failed: {}",
                    last_err()
                ));
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
    }

    impl FileSystemContext for Ext4Context {
        type FileContext = Ext4FileContext;

        fn get_security_by_name(
            &self,
            file_name: &U16CStr,
            _security_descriptor: Option<&mut [c_void]>,
            _resolve_reparse: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
        ) -> FspResult<FileSecurity> {
            let unix_path =
                winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
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
            let unix_path =
                winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let attr = stat_path(self.mount.fs, &unix_path)?;
            populate_file_info(&attr, file_info.as_mut());
            Ok(Ext4FileContext {
                inode: attr.inode,
                unix_path: Mutex::new(unix_path),
                is_dir: matches!(attr.file_type, fs_ext4_file_type_t::Dir),
                size: Mutex::new(attr.size),
                attr: Mutex::new(attr),
                delete: Mutex::new(false),
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
        // RW-side methods. These are wired unconditionally; on a RO mount
        // the volume's `read_only_volume` flag means WinFsp short-circuits
        // them with STATUS_MEDIA_WRITE_PROTECTED before dispatch, so the
        // bodies don't need a `if !self.mount.writable { ... }` guard.
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
            let unix_path =
                winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
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

            let attr = stat_path(self.mount.fs, &unix_path)?;
            populate_file_info(&attr, file_info.as_mut());
            Ok(Ext4FileContext {
                inode: attr.inode,
                unix_path: Mutex::new(unix_path),
                is_dir,
                size: Mutex::new(attr.size),
                attr: Mutex::new(attr),
                delete: Mutex::new(false),
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
            let new_end = eff_offset
                .checked_add(accept_len)
                .ok_or_else(|| windows::core::Error::from(STATUS_INVALID_DEVICE_REQUEST))?;
            let new_size = new_end.max(cur_size);

            // v1 compromise: the C ABI's `fs_ext4_write_file` is a
            // whole-file replace, so we read existing content, splice the
            // new bytes in, and write the merged buffer back.
            let mut merged: Vec<u8> = vec![0u8; new_size as usize];
            if cur_size > 0 {
                let n = unsafe {
                    fs_ext4_read_file(
                        self.mount.fs,
                        cp.as_ptr(),
                        merged.as_mut_ptr() as *mut c_void,
                        0,
                        cur_size,
                    )
                };
                if n < 0 {
                    let errno = unsafe { fs_ext4_last_errno() };
                    return Err(errno_to_status(errno).into());
                }
            }
            // Splice the new bytes in. `merged` is already `new_size`
            // bytes; bytes between `cur_size` and `eff_offset` (a hole
            // created by writing past EOF) stay zero.
            let dst = &mut merged[eff_offset as usize..(eff_offset + accept_len) as usize];
            dst.copy_from_slice(&buffer[..accept_len as usize]);

            let rc = unsafe {
                fs_ext4_write_file(
                    self.mount.fs,
                    cp.as_ptr(),
                    merged.as_ptr() as *const c_void,
                    merged.len() as u64,
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
            _file_attributes: u32,
            _creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            _last_change_time: u64,
            file_info: &mut FileInfo,
        ) -> FspResult<()> {
            // 0 means "leave unchanged" per WinFsp; we map that to
            // KEEP_UNCHANGED for the C ABI. ext4 stores second-precision
            // timestamps in the standard fields, so we drop the sub-second
            // residue (the C ABI accepts nsec but the underlying inode
            // only persists it when i_extra_isize covers it; for v1 we
            // pass 0).
            let path = context.unix_path();
            let cp = CString::new(path.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let atime_sec = filetime_to_unix(last_access_time).unwrap_or(KEEP_UNCHANGED);
            let mtime_sec = filetime_to_unix(last_write_time).unwrap_or(KEEP_UNCHANGED);
            if atime_sec != KEEP_UNCHANGED || mtime_sec != KEEP_UNCHANGED {
                let rc = unsafe {
                    fs_ext4_utimens(
                        self.mount.fs,
                        cp.as_ptr(),
                        atime_sec,
                        0,
                        mtime_sec,
                        0,
                    )
                };
                if rc != 0 {
                    let errno = unsafe { fs_ext4_last_errno() };
                    return Err(errno_to_status(errno).into());
                }
            }
            // file_attributes (READONLY etc.) intentionally not mapped in
            // v1 — chmod-driven posix bits are the source of truth.
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
            _replace_if_exists: bool,
        ) -> FspResult<()> {
            // The C ABI rejects existing destinations regardless of
            // `replace_if_exists` (overwrite-on-rename is a follow-up in
            // the library). We thread the flag through anyway so the
            // signature stays honest.
            let src = winpath_to_unix(file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let dst = winpath_to_unix(new_file_name).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
            let csrc = CString::new(src.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let cdst = CString::new(dst.as_str())
                .map_err(|_| windows::core::Error::from(STATUS_OBJECT_NAME_NOT_FOUND))?;
            let rc =
                unsafe { fs_ext4_rename(self.mount.fs, csrc.as_ptr(), cdst.as_ptr()) };
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
            context: &Self::FileContext,
            _file_name: &U16CStr,
            delete_file: bool,
        ) -> FspResult<()> {
            *context.delete.lock().unwrap() = delete_file;
            Ok(())
        }

        fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, flags: u32) {
            if flags & FSP_CLEANUP_DELETE == 0 {
                return;
            }
            if !*context.delete.lock().unwrap() {
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
    }

    /// FILETIME (100-ns since 1601) → Unix-seconds. Returns `None` for
    /// 0 (WinFsp's "leave unchanged" sentinel) and for FILETIMEs that
    /// predate the Unix epoch (clamped to `None` rather than wrapping).
    fn filetime_to_unix(ft: u64) -> Option<u32> {
        if ft == 0 {
            return None;
        }
        let secs_since_1601 = ft / 10_000_000;
        if secs_since_1601 < FILETIME_EPOCH_OFFSET_SEC {
            return None;
        }
        let unix = secs_since_1601 - FILETIME_EPOCH_OFFSET_SEC;
        if unix > u32::MAX as u64 {
            // Beyond Y2038 (in unix32 land). We pass through the high
            // bits anyway — the C ABI takes u32 and ext4 either truncates
            // or stores extra precision via xattrs. For v1 just clamp.
            return Some(u32::MAX - 1);
        }
        Some(unix as u32)
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
            .filesystem_name("ext4");
        // Default: read-only volume. Drop the flag for `--rw` mounts so
        // WinFsp dispatches mutating ops to our `create`/`write`/etc.
        // handlers instead of short-circuiting them with
        // STATUS_MEDIA_WRITE_PROTECTED.
        if !writable {
            params.read_only_volume(true);
        }

        let mut host = FileSystemHost::new(params, ctx)
            .map_err(|e| anyhow!("FileSystemHost::new failed: {e}"))?;

        // FileSystemHost::mount accepts any S where &S: Into<MountPoint>,
        // and `&str: AsRef<OsStr>` satisfies the existing blanket impl.
        host.mount(mount_point)
            .map_err(|e| anyhow!("mount({mount_point}) failed: {e}"))?;
        host.start()
            .map_err(|e| anyhow!("FileSystemHost::start failed: {e}"))?;

        let mode = if writable { "RW" } else { "RO" };
        println!("ext4 mounted at {mount_point} ({mode}). Ctrl-C to unmount.");
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

