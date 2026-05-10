//! Subcommand implementations.
//!
//! Filesystem access goes through `Mount` (in [`crate::mount`]), which
//! wraps the `fs_ext4_*` C ABI. Each subcommand opens a `Mount`, calls a
//! few C ABI functions, prints, and drops.

use anyhow::{Context, Result, bail};
use fs_ext4::capi::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::sync::Mutex;

use crate::MountArgs;
use crate::mount::Mount;
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

pub fn verify_ls(mt: &MountArgs, path: &str, expect: &[String]) -> Result<()> {
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

    use std::collections::BTreeSet;
    let got_set: BTreeSet<&str> = got.iter().map(|s| s.as_str()).collect();
    let want_set: BTreeSet<&str> = expect.iter().map(|s| s.as_str()).collect();

    if got_set == want_set {
        return Ok(());
    }

    let missing: Vec<&str> = want_set.difference(&got_set).copied().collect();
    let extra: Vec<&str> = got_set.difference(&want_set).copied().collect();
    let mut msg = format!("verify-ls drift at {path}:");
    if !missing.is_empty() {
        msg.push_str(&format!("\n  missing: {missing:?}"));
    }
    if !extra.is_empty() {
        msg.push_str(&format!("\n  unexpected: {extra:?}"));
    }
    bail!(msg);
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

pub fn cat(mt: &MountArgs, path: &str) -> Result<()> {
    use std::io::Write;

    let m = Mount::open(mt)?;
    let cp = CString::new(path).context("path contains NUL byte")?;

    let mut attr: fs_ext4_attr_t = unsafe { std::mem::zeroed() };
    if unsafe { fs_ext4_stat(m.fs, cp.as_ptr(), &mut attr) } != 0 {
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
                m.fs,
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
