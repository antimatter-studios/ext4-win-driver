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
pub trait BlockSource: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
    fn size(&self) -> u64;
}

/// File-backed source. Works for regular files everywhere and for raw
/// Windows devices (`\\.\X:`, `\\.\PhysicalDriveN`).
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = open_with_share(path)?;
        let size = compute_size(&file).with_context(|| format!("sizing {path:?}"))?;
        Ok(Self { file, size })
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
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Platform-specific opening + sizing
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn open_with_share(path: &Path) -> Result<File> {
    File::open(path).with_context(|| format!("opening {path:?}"))
}

#[cfg(windows)]
fn open_with_share(path: &Path) -> Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE = 7. Required
    // when reading a mounted volume so the kernel doesn't reject us with
    // ERROR_SHARING_VIOLATION.
    OpenOptions::new()
        .read(true)
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
    // BLKGETSIZE64, macOS: DKIOCGETBLOCKCOUNT). Out of scope — the goal
    // is Windows. Best effort: seek-to-end.
    let mut f = file.try_clone()?;
    Ok(f.seek(SeekFrom::End(0))?)
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
