//! Block-source abstraction for the CLI.
//!
//! `BlockSource` is a tiny `read_at(offset, buf)` + `size()` trait. The
//! single implementation, [`FileSource`], wraps `std::fs::File` and uses
//! positional reads (`pread` on Unix, `ReadFile` with `OVERLAPPED` on
//! Windows) so it's safe to share across threads without a `Mutex`.
//!
//! On Windows, [`FileSource::open`] additionally:
//!   - opens with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`
//!     so a mounted volume can still be opened for reading;
//!   - falls back to `IOCTL_DISK_GET_LENGTH_INFO` when `metadata().len()`
//!     reports 0 (raw devices like `\\.\X:` and `\\.\PhysicalDriveN`).
//!
//! On Unix, raw block devices are out of scope for now — the use case is
//! Windows. macOS regular files Just Work for the test images we exercise
//! during development.

use anyhow::{Context, Result};
use std::fs::File;
use std::path::Path;

/// Random-access read source. `Send + Sync` so it can sit behind an `Arc`
/// shared between the CLI and the C ABI's read callback.
///
/// `write_at` and `flush` are optional. The default impls return an error
/// so existing read-only impls don't need to change. Implementors that
/// open the underlying handle for write should override them.
pub trait BlockSource: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
    fn size(&self) -> u64;

    /// Write `buf` at `offset`. Default = NotConnected so a RW callback
    /// fed a read-only `BlockSource` fails fast instead of corrupting.
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "write_at: BlockSource is read-only",
        ))
    }

    /// Flush buffered writes to the underlying device. Default = no-op
    /// success (RO sources have nothing to flush).
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// File-backed source. Works for regular files everywhere and for raw
/// Windows devices (`\\.\X:`, `\\.\PhysicalDriveN`).
pub struct FileSource {
    file: File,
    size: u64,
    writable: bool,
    /// Required IO alignment in bytes. `1` for regular files (no
    /// constraint); for raw Windows devices we set it to the device's
    /// physical sector size (512 on classic drives, 4096 on advanced
    /// format / 4Kn). Read paths round down/up to this granularity
    /// because Windows raw-disk handles reject `ReadFile` calls
    /// whose offset or length isn't a multiple of the sector size.
    sector: u64,
}

impl FileSource {
    /// Open read-only.
    pub fn open(path: &Path) -> Result<Self> {
        let file = open_with_share(path, false)?;
        let size = compute_size(&file).with_context(|| format!("sizing {path:?}"))?;
        let sector = detect_sector_alignment(&file);
        Ok(Self {
            file,
            size,
            writable: false,
            sector,
        })
    }

    /// Open read-write. On Windows uses the same generous share mode
    /// (`FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`) plus
    /// `.write(true)` so a mounted volume can still be reopened. On Unix
    /// this is just `OpenOptions::new().read(true).write(true)`.
    pub fn open_rw(path: &Path) -> Result<Self> {
        let file = open_with_share(path, true)?;
        let size = compute_size(&file).with_context(|| format!("sizing {path:?}"))?;
        let sector = detect_sector_alignment(&file);
        Ok(Self {
            file,
            size,
            writable: true,
            sector,
        })
    }
}

impl BlockSource for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.read_exact_at(buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt;

        // Fast path: regular files (sector == 1) or raw-device reads
        // that happen to already be sector-aligned. Avoids the bounce
        // buffer + memcpy for the common case.
        let aligned_offset = offset & !(self.sector - 1);
        let aligned_len_needed = {
            let end = offset + buf.len() as u64;
            let aligned_end = (end + self.sector - 1) & !(self.sector - 1);
            aligned_end - aligned_offset
        };
        if self.sector == 1
            || (aligned_offset == offset && aligned_len_needed == buf.len() as u64)
        {
            let mut total = 0usize;
            while total < buf.len() {
                let n = self
                    .file
                    .seek_read(&mut buf[total..], offset + total as u64)?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "short read",
                    ));
                }
                total += n;
            }
            return Ok(());
        }

        // Slow path: raw-device read that isn't sector-aligned. Round
        // offset down + length up to the device's sector size, read
        // into a scratch buffer, then memcpy the requested range out.
        // The overhead is at most one extra sector at each end --
        // negligible next to the syscall + DMA cost.
        let mut scratch = vec![0u8; aligned_len_needed as usize];
        let mut total = 0usize;
        while total < scratch.len() {
            let n = self
                .file
                .seek_read(&mut scratch[total..], aligned_offset + total as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "short read",
                ));
            }
            total += n;
        }
        let copy_start = (offset - aligned_offset) as usize;
        buf.copy_from_slice(&scratch[copy_start..copy_start + buf.len()]);
        Ok(())
    }

    #[cfg(unix)]
    fn write_at(&self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "FileSource opened read-only",
            ));
        }
        self.file.write_all_at(buf, offset)
    }

    #[cfg(windows)]
    fn write_at(&self, offset: u64, buf: &[u8]) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt;
        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "FileSource opened read-only",
            ));
        }
        let mut total = 0usize;
        while total < buf.len() {
            let n = self.file.seek_write(&buf[total..], offset + total as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "short write",
                ));
            }
            total += n;
        }
        Ok(())
    }

    fn flush(&self) -> std::io::Result<()> {
        if !self.writable {
            return Ok(());
        }
        self.file.sync_data()
    }
}

// ---------------------------------------------------------------------------
// Platform-specific opening + sizing
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn open_with_share(path: &Path, write: bool) -> Result<File> {
    use std::fs::OpenOptions;
    OpenOptions::new()
        .read(true)
        .write(write)
        .open(path)
        .with_context(|| format!("opening {path:?}"))
}

#[cfg(windows)]
fn open_with_share(path: &Path, write: bool) -> Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE = 7. Required
    // when reading/writing a mounted volume so the kernel doesn't reject
    // us with ERROR_SHARING_VIOLATION.
    OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(0x7)
        .open(path)
        .with_context(|| format!("opening {path:?}"))
}

#[cfg(unix)]
fn compute_size(file: &File) -> Result<u64> {
    use std::io::{Seek, SeekFrom};
    if let Ok(m) = file.metadata() {
        let len = m.len();
        if len > 0 {
            return Ok(len);
        }
    }
    // Block-device sizing on Unix needs platform-specific ioctls (Linux:
    // BLKGETSIZE64, macOS: DKIOCGETBLOCKCOUNT). Out of scope -- the goal
    // is Windows. Best effort: seek-to-end.
    let mut f = file.try_clone()?;
    Ok(f.seek(SeekFrom::End(0))?)
}

/// Required IO alignment in bytes. `1` means "no constraint" -- regular
/// files behave that way. For raw Windows devices (`\\.\X:`,
/// `\\.\PhysicalDriveN`) we query the device's logical sector size via
/// `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` and round to that. The IOCTL
/// fails on regular files, which is how we distinguish the two.
#[cfg(unix)]
fn detect_sector_alignment(_file: &File) -> u64 {
    1
}

#[cfg(windows)]
fn compute_size(file: &File) -> Result<u64> {
    if let Ok(m) = file.metadata() {
        let len = m.len();
        if len > 0 {
            return Ok(len);
        }
    }
    win32_device_size(file)
}

#[cfg(windows)]
fn win32_device_size(file: &File) -> Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO,
    };
    let mut info: GET_LENGTH_INFORMATION = unsafe { std::mem::zeroed() };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            IOCTL_DISK_GET_LENGTH_INFO,
            std::ptr::null_mut(),
            0,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("IOCTL_DISK_GET_LENGTH_INFO");
    }
    Ok(info.Length as u64)
}

/// Probe `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` to find the device's
/// logical sector size. Returns 1 (i.e. "no alignment constraint")
/// when the IOCTL fails -- which is what we want for regular files.
#[cfg(windows)]
fn detect_sector_alignment(file: &File) -> u64 {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DISK_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
    };

    let mut g: DISK_GEOMETRY_EX = unsafe { std::mem::zeroed() };
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            std::ptr::null_mut(),
            0,
            &mut g as *mut _ as *mut _,
            std::mem::size_of::<DISK_GEOMETRY_EX>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        // Regular file (or other non-disk handle). No alignment
        // constraint at all.
        return 1;
    }
    let bps = g.Geometry.BytesPerSector;
    if bps == 0 || (bps & (bps - 1)) != 0 {
        // Defensive: only accept power-of-two sector sizes.
        return 512;
    }
    bps as u64
}
