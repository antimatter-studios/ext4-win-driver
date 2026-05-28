//! Subcommand implementations.
//!
//! Filesystem access goes through `Mount` (in [`crate::mount`]), which
//! wraps the `fs_ext4_*` C ABI. Each subcommand opens a `Mount`, calls a
//! few C ABI functions, prints, and drops.

use anyhow::{bail, Context, Result};
use fs_ext4::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::sync::Mutex;

use crate::mount::Mount;
use crate::MountArgs;
use winfsp_fs_skeleton::partition;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn last_err() -> String {
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

/// Format a POSIX mode integer as an `ls -l`-style string (e.g. `drwxr-xr-x`).
fn format_mode_str(mode: u16, file_type: &fs_ext4_file_type_t) -> String {
    let type_char = match file_type {
        fs_ext4_file_type_t::RegFile => '-',
        fs_ext4_file_type_t::Dir => 'd',
        fs_ext4_file_type_t::Symlink => 'l',
        fs_ext4_file_type_t::ChrDev => 'c',
        fs_ext4_file_type_t::BlkDev => 'b',
        fs_ext4_file_type_t::Fifo => 'p',
        fs_ext4_file_type_t::Sock => 's',
        _ => '?',
    };
    let setuid = mode & 0o4000 != 0;
    let setgid = mode & 0o2000 != 0;
    let sticky = mode & 0o1000 != 0;
    let bit = |mask: u16, c: char, alt: char| if mode & mask != 0 { c } else { alt };
    format!(
        "{}{}{}{}{}{}{}{}{}{}",
        type_char,
        bit(0o400, 'r', '-'),
        bit(0o200, 'w', '-'),
        if setuid {
            if mode & 0o100 != 0 {
                's'
            } else {
                'S'
            }
        } else {
            bit(0o100, 'x', '-')
        },
        bit(0o040, 'r', '-'),
        bit(0o020, 'w', '-'),
        if setgid {
            if mode & 0o010 != 0 {
                's'
            } else {
                'S'
            }
        } else {
            bit(0o010, 'x', '-')
        },
        bit(0o004, 'r', '-'),
        bit(0o002, 'w', '-'),
        if sticky {
            if mode & 0o001 != 0 {
                't'
            } else {
                'T'
            }
        } else {
            bit(0o001, 'x', '-')
        },
    )
}

/// Format a Unix timestamp (seconds since epoch) as a compact UTC string.
fn format_unix_time(secs: u32) -> String {
    // Days from epoch; compute year/month/day via the proleptic Gregorian calendar.
    let s = secs as u64;
    let (sec, s) = (s % 60, s / 60);
    let (min, s) = (s % 60, s / 60);
    let (hour, mut days) = (s % 24, s / 24);
    // Algorithm: days since 1970-01-01
    let mut year = 1970u32;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let dy = if leap { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for m in &months {
        if days < *m {
            break;
        }
        days -= m;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02}T{hour:02}:{min:02}:{sec:02}Z",
        days + 1
    )
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

pub fn info(mt: &MountArgs) -> Result<()> {
    let m = Mount::open(mt)?;
    let mut vi: fs_ext4_volume_info_t = unsafe { std::mem::zeroed() };
    let r = unsafe { fs_ext4_get_volume_info(m.fs, &mut vi) };
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
    println!(
        "inodes:         {} total, {} free",
        vi.total_inodes, vi.free_inodes
    );
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

pub fn ls(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let iter = unsafe { fs_ext4_dir_open(m.fs, cp.as_ptr()) };
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

pub fn verify_ls(
    mt: &MountArgs,
    path: &str,
    expect: &[String],
    expect_count: Option<usize>,
) -> Result<()> {
    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let iter = unsafe { fs_ext4_dir_open(m.fs, cp.as_ptr()) };
    if iter.is_null() {
        bail!("dir_open({path:?}) failed: {}", last_err());
    }
    let mut got = Vec::<String>::new();
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
        got.push(String::from_utf8_lossy(&name_bytes).into_owned());
    }
    unsafe { fs_ext4_dir_close(iter) };

    let mut errs: Vec<String> = Vec::new();

    if !expect.is_empty() {
        use std::collections::BTreeSet;
        let got_set: BTreeSet<&str> = got.iter().map(|s| s.as_str()).collect();
        let want_set: BTreeSet<&str> = expect.iter().map(|s| s.as_str()).collect();
        if got_set != want_set {
            let missing: Vec<&str> = want_set.difference(&got_set).copied().collect();
            let extra: Vec<&str> = got_set.difference(&want_set).copied().collect();
            let mut msg = format!("name-set drift at {path}:");
            if !missing.is_empty() {
                msg.push_str(&format!("\n  missing: {missing:?}"));
            }
            if !extra.is_empty() {
                msg.push_str(&format!("\n  unexpected: {extra:?}"));
            }
            errs.push(msg);
        }
    }

    if let Some(want) = expect_count {
        if got.len() != want {
            errs.push(format!(
                "count mismatch at {path}: got={} want={want}",
                got.len()
            ));
        }
    }

    if errs.is_empty() {
        return Ok(());
    }
    bail!("verify-ls {}", errs.join(" / "));
}

// ---------------------------------------------------------------------------
// stat
// ---------------------------------------------------------------------------

pub fn stat(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let r = unsafe { fs_ext4_stat(m.fs, cp.as_ptr(), &mut attr) };
    if r != 0 {
        bail!("stat({path:?}) failed: {}", last_err());
    }
    let mode_str = format_mode_str(attr.mode, &attr.file_type);
    println!("path:        {path}");
    println!("inode:       {}", attr.inode);
    println!("size:        {}", attr.size);
    println!("mode:        {mode_str}  (0o{:o})", attr.mode & 0o7777);
    println!("uid/gid:     {}/{}", attr.uid, attr.gid);
    println!("link_count:  {}", attr.link_count);
    println!("inode_flags: 0x{:08x}", attr.inode_flags);
    println!("generation:  {}", attr.generation);
    println!("blocks_512:  {}", attr.blocks_512);
    println!(
        "atime:       {}.{:09} ({})",
        attr.atime,
        attr.atime_nsec,
        format_unix_time(attr.atime)
    );
    println!(
        "mtime:       {}.{:09} ({})",
        attr.mtime,
        attr.mtime_nsec,
        format_unix_time(attr.mtime)
    );
    println!(
        "ctime:       {}.{:09} ({})",
        attr.ctime,
        attr.ctime_nsec,
        format_unix_time(attr.ctime)
    );
    println!(
        "crtime:      {}.{:09} ({})",
        attr.crtime,
        attr.crtime_nsec,
        format_unix_time(attr.crtime)
    );
    println!("type:        {:?}", attr.file_type);
    Ok(())
}

// ---------------------------------------------------------------------------
// cat
// ---------------------------------------------------------------------------

/// Resolve symlinks up to 8 hops; returns the final non-symlink path.
fn resolve_symlink(m: &Mount, path: &str) -> Result<String> {
    let mut current = path.to_owned();
    for _ in 0..8 {
        let cp = CString::new(current.as_str()).context("path contains NUL byte")?;
        let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
        if unsafe { fs_ext4_stat(m.fs, cp.as_ptr(), &mut attr) } != 0 {
            bail!("stat({current:?}) failed: {}", last_err());
        }
        if !matches!(attr.file_type, fs_ext4_file_type_t::Symlink) {
            return Ok(current);
        }
        let mut buf = vec![0u8; 4096];
        let r = unsafe { fs_ext4_readlink(m.fs, cp.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if r < 0 {
            bail!("readlink({current:?}) failed: {}", last_err());
        }
        let target = std::str::from_utf8(&buf[..r as usize]).unwrap_or("").to_owned();
        if target.starts_with('/') {
            current = target;
        } else {
            let parent = current.rfind('/').map(|i| &current[..i]).unwrap_or("/");
            current = format!("{}/{}", parent.trim_end_matches('/'), target);
        }
    }
    bail!("symlink loop or depth > 8: {path:?}")
}

pub fn cat(mt: &MountArgs, path: &str, offset: u64, length: Option<u64>) -> Result<()> {
    use std::io::Write;

    let m = Mount::open(mt)?;
    let resolved = resolve_symlink(&m, path)?;
    let cp = CString::new(resolved.as_str()).context("path contains NUL byte")?;

    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    if unsafe { fs_ext4_stat(m.fs, cp.as_ptr(), &mut attr) } != 0 {
        bail!("stat({resolved:?}) failed: {}", last_err());
    }

    let end = length
        .map(|l| (offset + l).min(attr.size))
        .unwrap_or(attr.size);
    if offset >= end {
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();
    let mut pos = offset;
    let mut buf = vec![0u8; 64 * 1024];
    while pos < end {
        let want = std::cmp::min(buf.len() as u64, end - pos);
        let n = unsafe {
            fs_ext4_read_file(
                m.fs,
                cp.as_ptr(),
                buf.as_mut_ptr() as *mut c_void,
                pos,
                want,
            )
        };
        if n < 0 {
            bail!("read_file({resolved:?}) failed: {}", last_err());
        }
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[..n as usize])?;
        pos += n as u64;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tree
// ---------------------------------------------------------------------------

pub fn tree(mt: &MountArgs, max_depth: u32) -> Result<()> {
    let m = Mount::open(mt)?;
    println!("/");
    walk(&m, "/", 0, max_depth)
}

fn walk(m: &Mount, dir: &str, depth: u32, max_depth: u32) -> Result<()> {
    if depth >= max_depth {
        return Ok(());
    }
    let cp = CString::new(dir).context("path contains NUL byte")?;
    let iter = unsafe { fs_ext4_dir_open(m.fs, cp.as_ptr()) };
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
    for (ino, ft, name) in entries {
        println!("{prefix}{:>10} {} {}", ino, ftype_str(ft), name);
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
    println!(
        "{:>3} {:>16} {:>16} {:>10} {}",
        "#", "start (LBA)", "size (sectors)", "type", "name"
    );
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

// ---------------------------------------------------------------------------
// audit — read-only fsck via fs_ext4_fsck_run
// ---------------------------------------------------------------------------

/// Wraps a finding's three on-disk fields into a printable line.
#[derive(Debug)]
struct Finding {
    kind: String,
    inode: u32,
    detail: String,
}

/// Receives findings from the C ABI via a `*mut c_void` context. The
/// outer `Mutex` is required because the FFI surface only lets us hand
/// over a raw pointer; the callback runs synchronously on the same
/// thread but we still want a non-`unsafe` body inside the lock.
struct AuditCtx {
    findings: Mutex<Vec<Finding>>,
}

extern "C" fn audit_finding_cb(
    context: *mut c_void,
    kind: *const c_char,
    inode: u32,
    detail: *const c_char,
) {
    if context.is_null() || kind.is_null() {
        return;
    }
    let ctx = unsafe { &*(context as *const AuditCtx) };
    let kind_s = unsafe { CStr::from_ptr(kind).to_string_lossy().into_owned() };
    let detail_s = if detail.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(detail).to_string_lossy().into_owned() }
    };
    if let Ok(mut v) = ctx.findings.lock() {
        v.push(Finding {
            kind: kind_s,
            inode,
            detail: detail_s,
        });
    }
}

pub fn audit(mt: &MountArgs, max_dirs: u32, max_entries_per_dir: u32) -> Result<()> {
    let m = Mount::open(mt)?;
    let ctx = Box::new(AuditCtx {
        findings: Mutex::new(Vec::new()),
    });
    let raw_ctx = Box::into_raw(ctx);

    let opts = fs_ext4_fsck_options_t {
        read_only: 1,
        // RO mount can't replay anyway; the C ABI rejects replay+RO.
        replay_journal: 0,
        max_dirs,
        max_entries_per_dir,
        on_progress: None,
        on_finding: Some(audit_finding_cb),
        context: raw_ctx as *mut c_void,
        // Read-only audit — repair pass is skipped regardless, but the
        // C ABI requires the field so we set it explicitly to 0.
        repair: 0,
    };
    let mut report: fs_ext4_fsck_report_t = unsafe { std::mem::zeroed() };

    let r = unsafe { fs_ext4_fsck_run(m.fs, &opts, &mut report) };
    // Reclaim the context box no matter what, then handle errors.
    let ctx = unsafe { Box::from_raw(raw_ctx) };

    if r != 0 {
        bail!("fs_ext4_fsck_run failed: {}", last_err());
    }

    println!("inodes_visited:      {}", report.inodes_visited);
    println!("directories_scanned: {}", report.directories_scanned);
    println!("entries_scanned:     {}", report.entries_scanned);
    println!("anomalies_found:     {}", report.anomalies_found);
    println!(
        "was_dirty:           {}",
        if report.was_dirty != 0 { "yes" } else { "no" }
    );

    if report.anomalies_found == 0 {
        println!();
        println!("clean");
        return Ok(());
    }

    println!();
    println!("anomalies:");
    let findings = ctx.findings.into_inner().unwrap_or_default();
    for f in &findings {
        println!("  ino={:<8} {:<18} {}", f.inode, f.kind, f.detail);
    }

    bail!("audit found {} anomaly(s)", report.anomalies_found);
}

// ---------------------------------------------------------------------------
// readlink
// ---------------------------------------------------------------------------

pub fn readlink(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let mut buf = vec![0u8; 4096];
    let r = unsafe { fs_ext4_readlink(m.fs, cp.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if r < 0 {
        bail!("readlink({path:?}) failed: {}", last_err());
    }
    let target = std::str::from_utf8(&buf[..r as usize]).unwrap_or("<invalid utf-8>");
    println!("{target}");
    Ok(())
}

// ---------------------------------------------------------------------------
// listxattr / getxattr
// ---------------------------------------------------------------------------

pub fn listxattr(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;

    // First call: probe required size.
    let needed = unsafe { fs_ext4_listxattr(m.fs, cp.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        bail!("listxattr({path:?}) failed: {}", last_err());
    }
    if needed == 0 {
        return Ok(()); // no xattrs
    }

    let mut buf = vec![0u8; needed as usize];
    let n = unsafe {
        fs_ext4_listxattr(
            m.fs,
            cp.as_ptr(),
            buf.as_mut_ptr() as *mut std::os::raw::c_char,
            buf.len(),
        )
    };
    if n < 0 {
        bail!("listxattr({path:?}) failed: {}", last_err());
    }

    // NUL-separated list of names.
    let mut pos = 0usize;
    while pos < n as usize {
        let end = buf[pos..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(n as usize - pos);
        if end == 0 {
            break;
        }
        println!(
            "{}",
            std::str::from_utf8(&buf[pos..pos + end]).unwrap_or("<invalid>")
        );
        pos += end + 1;
    }
    Ok(())
}

pub fn getxattr(mt: &MountArgs, path: &str, name: &str) -> Result<()> {
    use std::io::Write;

    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let cn = CString::new(name).context("name contains NUL byte")?;

    // Probe size.
    let needed =
        unsafe { fs_ext4_getxattr(m.fs, cp.as_ptr(), cn.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        bail!("getxattr({path:?}, {name:?}) failed: {}", last_err());
    }
    if needed == 0 {
        return Ok(()); // empty value
    }

    let mut buf = vec![0u8; needed as usize];
    let n = unsafe {
        fs_ext4_getxattr(
            m.fs,
            cp.as_ptr(),
            cn.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
        )
    };
    if n < 0 {
        bail!("getxattr({path:?}, {name:?}) failed: {}", last_err());
    }

    std::io::stdout().lock().write_all(&buf[..n as usize])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// setxattr / removexattr
// ---------------------------------------------------------------------------

pub fn setxattr(mt: &MountArgs, path: &str, name: &str, value: &[u8]) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let cn = CString::new(name).context("name contains NUL byte")?;
    let rc = unsafe {
        fs_ext4_setxattr(
            m.fs,
            cp.as_ptr(),
            cn.as_ptr(),
            value.as_ptr() as *const c_void,
            value.len(),
        )
    };
    if rc != 0 {
        bail!("setxattr({path:?}, {name:?}) failed: {}", last_err());
    }
    Ok(())
}

pub fn removexattr(mt: &MountArgs, path: &str, name: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let cn = CString::new(name).context("name contains NUL byte")?;
    let rc = unsafe { fs_ext4_removexattr(m.fs, cp.as_ptr(), cn.as_ptr()) };
    if rc != 0 {
        bail!("removexattr({path:?}, {name:?}) failed: {}", last_err());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// symlink / link
// ---------------------------------------------------------------------------

pub fn symlink(mt: &MountArgs, target: &str, linkpath: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let ct = CString::new(target).context("target contains NUL byte")?;
    let cl = CString::new(linkpath).context("linkpath contains NUL byte")?;
    // fs_ext4_symlink returns the new inode number (>0) on success, 0 on failure.
    let ino = unsafe { fs_ext4_symlink(m.fs, ct.as_ptr(), cl.as_ptr()) };
    if ino == 0 {
        bail!("symlink({target:?} -> {linkpath:?}) failed: {}", last_err());
    }
    Ok(())
}

pub fn link(mt: &MountArgs, src: &str, dst: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cs = CString::new(src).context("src contains NUL byte")?;
    let cd = CString::new(dst).context("dst contains NUL byte")?;
    let rc = unsafe { fs_ext4_link(m.fs, cs.as_ptr(), cd.as_ptr()) };
    if rc != 0 {
        bail!("link({src:?} -> {dst:?}) failed: {}", last_err());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// write / mkdir / rmdir / unlink / truncate  (mutating CLI ops)
// ---------------------------------------------------------------------------

pub fn write_file(mt: &MountArgs, path: &str) -> Result<()> {
    use std::io::Read;
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let mut data = Vec::new();
    std::io::stdin()
        .read_to_end(&mut data)
        .context("reading stdin")?;

    // Create the file if it doesn't exist yet (fs_ext4_truncate fails on ENOENT).
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let exists = unsafe { fs_ext4_stat(m.fs, cp.as_ptr(), &mut attr) } == 0;
    if !exists {
        let ino = unsafe { fs_ext4_create(m.fs, cp.as_ptr(), 0o644) };
        if ino == 0 {
            bail!("create({path:?}) failed: {}", last_err());
        }
    }

    let rc = unsafe { fs_ext4_truncate(m.fs, cp.as_ptr(), 0) };
    if rc != 0 {
        bail!("truncate({path:?}, 0) failed: {}", last_err());
    }
    if !data.is_empty() {
        let written = unsafe {
            fs_ext4_pwrite(
                m.fs,
                cp.as_ptr(),
                data.as_ptr() as *const c_void,
                data.len() as u64,
                0,
            )
        };
        if written < 0 {
            bail!("pwrite({path:?}) failed: {}", last_err());
        }
        if written < data.len() as i64 {
            bail!("pwrite({path:?}): short write ({written} < {})", data.len());
        }
    }
    Ok(())
}

pub fn touch(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    let exists = unsafe { fs_ext4_stat(m.fs, cp.as_ptr(), &mut attr) } == 0;
    if !exists {
        let ino = unsafe { fs_ext4_create(m.fs, cp.as_ptr(), 0o644) };
        if ino == 0 {
            bail!("create({path:?}) failed: {}", last_err());
        }
    }
    Ok(())
}

pub fn chmod(mt: &MountArgs, path: &str, mode: u32) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_chmod(m.fs, cp.as_ptr(), mode as u16) };
    if rc != 0 {
        bail!("chmod({path:?}, {mode:#o}) failed: {}", last_err());
    }
    Ok(())
}

pub fn chown(mt: &MountArgs, path: &str, uid: u32, gid: u32) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_chown(m.fs, cp.as_ptr(), uid, gid) };
    if rc != 0 {
        bail!("chown({path:?}, {uid}, {gid}) failed: {}", last_err());
    }
    Ok(())
}

pub fn setflags(mt: &MountArgs, path: &str, flags: u32) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_set_flags(m.fs, cp.as_ptr(), flags) };
    if rc != 0 {
        bail!("set_flags({path:?}, 0x{flags:08x}) failed: {}", last_err());
    }
    Ok(())
}

pub fn mknod(mt: &MountArgs, path: &str, mode: u16, major: u32, minor: u32) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_mknod(m.fs, cp.as_ptr(), mode, major, minor) };
    if rc != 0 {
        bail!("mknod({path:?}, mode=0o{mode:o}, {major}:{minor}) failed: {}", last_err());
    }
    Ok(())
}

pub fn mkdir(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_mkdir(m.fs, cp.as_ptr(), 0o755) };
    if rc != 0 {
        bail!("mkdir({path:?}) failed: {}", last_err());
    }
    Ok(())
}

pub fn rmdir(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_rmdir(m.fs, cp.as_ptr()) };
    if rc != 0 {
        bail!("rmdir({path:?}) failed: {}", last_err());
    }
    Ok(())
}

pub fn unlink(mt: &MountArgs, path: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_unlink(m.fs, cp.as_ptr()) };
    if rc != 0 {
        bail!("unlink({path:?}) failed: {}", last_err());
    }
    Ok(())
}

pub fn truncate(mt: &MountArgs, path: &str, size: u64) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_truncate(m.fs, cp.as_ptr(), size) };
    if rc != 0 {
        bail!("truncate({path:?}, {size}) failed: {}", last_err());
    }
    Ok(())
}

pub fn rename(mt: &MountArgs, src: &str, dst: &str) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cs = CString::new(src).context("src contains NUL byte")?;
    let cd = CString::new(dst).context("dst contains NUL byte")?;
    let rc = unsafe { fs_ext4_rename2(m.fs, cs.as_ptr(), cd.as_ptr(), 0) };
    if rc != 0 {
        bail!("rename({src:?} -> {dst:?}) failed: {}", last_err());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// fallocate
// ---------------------------------------------------------------------------

pub fn fallocate(mt: &MountArgs, path: &str, offset: u64, len: u64, flags: i32) -> Result<()> {
    let m = Mount::open_rw(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;
    let rc = unsafe { fs_ext4_fallocate(m.fs, cp.as_ptr(), offset, len, flags) };
    if rc != 0 {
        bail!(
            "fallocate({path:?}, offset={offset}, len={len}, flags={flags:#x}) failed: {}",
            last_err()
        );
    }
    Ok(())
}
