//! Subcommand implementations.
//!
//! All filesystem access goes through the `fs_ext4_*` C ABI exposed by the
//! `fs-ext4` library. The C ABI is the high-level surface of the library;
//! Rust modules underneath are primitives. Using the C ABI keeps this file
//! short and tracks any future improvements upstream automatically.
//!
//! When partition / WinFsp support lands the mount path will switch to
//! `fs_ext4_mount_with_callbacks` so a custom `BlockDevice` can be plugged in.

use anyhow::{Context, Result, anyhow, bail};
use fs_ext4::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::path::Path;

use crate::partition;

/// RAII wrapper around `*mut fs_ext4_fs_t`. Frees with `fs_ext4_umount` on drop.
struct Mount(*mut fs_ext4_fs_t);

impl Mount {
    fn open(image: &Path) -> Result<Self> {
        let s = image
            .to_str()
            .ok_or_else(|| anyhow!("image path is not valid UTF-8: {image:?}"))?;
        let c = CString::new(s).context("image path contains NUL byte")?;
        let fs = unsafe { fs_ext4_mount(c.as_ptr()) };
        if fs.is_null() {
            bail!("mount {image:?} failed: {}", last_err());
        }
        Ok(Self(fs))
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fs_ext4_umount(self.0) };
        }
    }
}

fn last_err() -> String {
    unsafe {
        let p = fs_ext4_last_error();
        if p.is_null() {
            return "<null>".into();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn cchar_slice_to_string(buf: &[std::os::raw::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn ftype_str(ft: u8) -> &'static str {
    match ft {
        0 => "?",
        1 => "f",
        2 => "d",
        3 => "c",
        4 => "b",
        5 => "p",
        6 => "s",
        7 => "l",
        _ => "?",
    }
}

fn format_uuid(u: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u[0], u[1], u[2], u[3],
        u[4], u[5],
        u[6], u[7],
        u[8], u[9],
        u[10], u[11], u[12], u[13], u[14], u[15],
    )
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

pub fn info(image: &Path) -> Result<()> {
    let m = Mount::open(image)?;
    let mut vi: fs_ext4_volume_info_t = unsafe { std::mem::zeroed() };
    let r = unsafe { fs_ext4_get_volume_info(m.0, &mut vi) };
    if r != 0 {
        bail!("fs_ext4_get_volume_info failed: {}", last_err());
    }

    let label = cchar_slice_to_string(&vi.volume_name);
    let last_mounted = cchar_slice_to_string(&vi.last_mounted);

    let bs = vi.block_size as u64;
    let used_bytes = (vi.total_blocks - vi.free_blocks) * bs;
    let free_bytes = vi.free_blocks * bs;
    let total_bytes = vi.total_blocks * bs;

    println!("label:          {label:?}");
    println!("uuid:           {}", format_uuid(&vi.uuid));
    println!("last_mounted:   {last_mounted:?}");
    println!("block_size:     {}", vi.block_size);
    println!(
        "total:          {total_bytes} bytes ({} blocks)",
        vi.total_blocks
    );
    println!(
        "used:           {used_bytes} bytes ({} blocks)",
        vi.total_blocks - vi.free_blocks
    );
    println!(
        "free:           {free_bytes} bytes ({} blocks)",
        vi.free_blocks
    );
    println!("inodes:         {} total, {} free", vi.total_inodes, vi.free_inodes);
    println!("inode_size:     {}", vi.inode_size);
    println!("rev:            {}.{}", vi.rev_level, vi.minor_rev_level);
    println!("feat_compat:    0x{:08x}", vi.feature_compat);
    println!("feat_incompat:  0x{:08x}", vi.feature_incompat);
    println!("feat_ro_compat: 0x{:08x}", vi.feature_ro_compat);
    println!(
        "state:          0x{:04x}{}",
        vi.state,
        if vi.mounted_dirty != 0 {
            "  (DIRTY — needs journal replay / fsck before RW)"
        } else {
            ""
        }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------------

pub fn ls(image: &Path, path: &str) -> Result<()> {
    let m = Mount::open(image)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let iter = unsafe { fs_ext4_dir_open(m.0, cp.as_ptr()) };
    if iter.is_null() {
        bail!("dir_open({path:?}) failed: {}", last_err());
    }
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
        let name = String::from_utf8_lossy(&name_bytes);
        println!(
            "{:>10} {} {}",
            entry.inode,
            ftype_str(entry.file_type),
            name
        );
    }
    unsafe { fs_ext4_dir_close(iter) };
    Ok(())
}

// ---------------------------------------------------------------------------
// stat
// ---------------------------------------------------------------------------

pub fn stat(image: &Path, path: &str) -> Result<()> {
    let m = Mount::open(image)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let r = unsafe { fs_ext4_stat(m.0, cp.as_ptr(), &mut attr) };
    if r != 0 {
        bail!("stat({path:?}) failed: {}", last_err());
    }
    println!("path:        {path}");
    println!("inode:       {}", attr.inode);
    println!("size:        {}", attr.size);
    println!("mode:        0o{:o}", attr.mode);
    println!("uid/gid:     {}/{}", attr.uid, attr.gid);
    println!("link_count:  {}", attr.link_count);
    println!("atime:       {}", attr.atime);
    println!("mtime:       {}", attr.mtime);
    println!("ctime:       {}", attr.ctime);
    println!("crtime:      {}", attr.crtime);
    println!("file_type:   {:?}", attr.file_type as u32);
    Ok(())
}

// ---------------------------------------------------------------------------
// cat
// ---------------------------------------------------------------------------

pub fn cat(image: &Path, path: &str) -> Result<()> {
    use std::io::Write;

    let m = Mount::open(image)?;
    let cp = CString::new(path).context("path contains NUL byte")?;

    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    if unsafe { fs_ext4_stat(m.0, cp.as_ptr(), &mut attr) } != 0 {
        bail!("stat({path:?}) failed: {}", last_err());
    }
    if attr.size == 0 {
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();
    let mut offset: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    while offset < attr.size {
        let want = std::cmp::min(buf.len() as u64, attr.size - offset);
        let n = unsafe {
            fs_ext4_read_file(
                m.0,
                cp.as_ptr(),
                buf.as_mut_ptr() as *mut c_void,
                offset,
                want,
            )
        };
        if n < 0 {
            bail!("read_file({path:?}) failed: {}", last_err());
        }
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[..n as usize])?;
        offset += n as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tree
// ---------------------------------------------------------------------------

pub fn tree(image: &Path, max_depth: u32) -> Result<()> {
    let m = Mount::open(image)?;
    println!("/");
    walk(&m, "/", 0, max_depth)
}

fn walk(m: &Mount, dir: &str, depth: u32, max_depth: u32) -> Result<()> {
    if depth >= max_depth {
        return Ok(());
    }
    let cp = CString::new(dir).context("path contains NUL byte")?;
    let iter = unsafe { fs_ext4_dir_open(m.0, cp.as_ptr()) };
    if iter.is_null() {
        eprintln!("  (dir_open({dir:?}) failed: {})", last_err());
        return Ok(());
    }
    let mut entries: Vec<(u32, u8, String)> = Vec::new();
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
        let name = String::from_utf8_lossy(&name_bytes).into_owned();
        if name == "." || name == ".." {
            continue;
        }
        entries.push((entry.inode, entry.file_type, name));
    }
    unsafe { fs_ext4_dir_close(iter) };

    let prefix = "  ".repeat(depth as usize + 1);
    for (ino, ft, name) in &entries {
        println!("{prefix}{:>10} {} {}", ino, ftype_str(*ft), name);
    }
    for (_, ft, name) in entries {
        if ft == 2 {
            let child = if dir.ends_with('/') {
                format!("{dir}{name}")
            } else {
                format!("{dir}/{name}")
            };
            walk(m, &child, depth + 1, max_depth)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// parts
// ---------------------------------------------------------------------------

pub fn parts(image: &Path) -> Result<()> {
    let parts = partition::list(image)?;
    if parts.is_empty() {
        println!("no partitions found");
        return Ok(());
    }
    println!("{:>3} {:>16} {:>16} {:>10} {}", "#", "start (LBA)", "size (sectors)", "type", "name");
    for (i, p) in parts.iter().enumerate() {
        println!(
            "{:>3} {:>16} {:>16} {:>10} {}",
            i + 1,
            p.start_lba,
            p.num_sectors,
            p.kind,
            p.name.as_deref().unwrap_or("")
        );
    }
    Ok(())
}
