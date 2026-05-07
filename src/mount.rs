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
    /// Set when mounted via `fs_ext4_mount_with_callbacks`. Owned here.
    cb_ctx: Option<*mut SliceCtx>,
}

// `*mut fs_ext4_fs_t` is opaque to us and the underlying `Filesystem` is
// internally synchronized, so it's safe to share across threads.
unsafe impl Send for Mount {}
unsafe impl Sync for Mount {}

impl Mount {
    pub fn open(mt: &MountArgs) -> Result<Self> {
        match mt.part {
            None => Self::open_direct(&mt.image),
            Some(n) => Self::open_partition(&mt.image, n),
        }
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
        Ok(Self { fs, cb_ctx: None })
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

// ---------------------------------------------------------------------------
// WinFsp adapter (feature = "mount", windows only)
// ---------------------------------------------------------------------------

#[cfg(all(windows, feature = "mount"))]
mod winfsp_adapter {
    //! Glue between WinFsp's `FileSystemContext` and the `fs_ext4_*` C ABI.
    //!
    //! Phase A — read-only browsing. Just enough to see ext4 contents
    //! through Explorer; writes return STATUS_INVALID_DEVICE_REQUEST via
    //! the trait's default impls.
    //!
    //! Path conversions: WinFsp gives backslash-separated UTF-16 paths
    //! (`\foo\bar`); the ext4 C ABI wants slash-separated UTF-8
    //! (`/foo/bar`). Done in [`winpath_to_unix`].
    //!
    //! Time conversions: ext4 stores 32-bit unix epoch seconds; Windows
    //! FILETIME is 100-ns intervals since 1601-01-01. The constant offset
    //! is 11644473600 seconds.

    use anyhow::{Context, Result, anyhow};
    use fs_ext4::capi::*;
    use std::ffi::{CString, c_void};
    use std::sync::Mutex;
    use widestring::U16CStr;
    use winfsp::Result as FspResult;
    use winfsp::filesystem::{
        DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo,
        WideNameInfo,
    };
    use winfsp::host::{FileSystemHost, VolumeParams};
    use windows::Win32::Foundation::{
        STATUS_END_OF_FILE, STATUS_INVALID_DEVICE_REQUEST, STATUS_NOT_A_DIRECTORY,
        STATUS_OBJECT_NAME_NOT_FOUND,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    };
    use winfsp_sys::FILE_ACCESS_RIGHTS;

    use crate::cmd::last_err;
    use crate::mount::Mount;

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
            2 /* ENOENT */ => STATUS_OBJECT_NAME_NOT_FOUND,
            20 /* ENOTDIR */ => STATUS_NOT_A_DIRECTORY,
            _ => STATUS_INVALID_DEVICE_REQUEST,
        }
    }

    /// Per-open file handle state.
    pub struct Ext4FileContext {
        pub inode: u32,
        pub unix_path: String,
        pub is_dir: bool,
        pub size: u64,
        /// Cached when the open call comes in; refreshed on get_file_info.
        attr: Mutex<fs_ext4_attr_t>,
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
                unix_path,
                is_dir: matches!(attr.file_type, fs_ext4_file_type_t::Dir),
                size: attr.size,
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
            let attr = stat_path(self.mount.fs, &context.unix_path)?;
            populate_file_info(&attr, file_info);
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
            if offset >= context.size {
                return Err(STATUS_END_OF_FILE.into());
            }
            let cp = CString::new(context.unix_path.as_str())
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
            let cp = CString::new(context.unix_path.as_str())
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

                let child_path = if context.unix_path == "/" {
                    format!("/{name}")
                } else {
                    format!("{}/{name}", context.unix_path)
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
    }

    /// Mount the given ext4 source on a Windows mount point.
    ///
    /// `mount_point` accepts a drive letter (`X:`) or a path to an empty
    /// directory. Blocks until the user presses Ctrl-C, then unmounts.
    pub fn run(mount: Mount, mount_point: &str) -> Result<()> {
        let _init = winfsp::winfsp_init().context("WinFsp not installed?")?;

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
            .read_only_volume(true)
            .filesystem_name("ext4");

        let mut host = FileSystemHost::new(params, ctx)
            .map_err(|e| anyhow!("FileSystemHost::new failed: {e}"))?;

        // FileSystemHost::mount accepts any S where &S: Into<MountPoint>,
        // and `&str: AsRef<OsStr>` satisfies the existing blanket impl.
        host.mount(mount_point)
            .map_err(|e| anyhow!("mount({mount_point}) failed: {e}"))?;
        host.start()
            .map_err(|e| anyhow!("FileSystemHost::start failed: {e}"))?;

        println!("ext4 mounted at {mount_point}. Ctrl-C to unmount.");
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

