//! ext4-win-driver CLI entry point.
//!
//! Thin clap dispatcher; real work lives in [`cmd`].

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Parse `0x...` hex or plain decimal u32 for clap `--value-parser`.
fn parse_hex_or_dec(s: &str) -> Result<u32, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u32>().map_err(|e| e.to_string())
    }
}

use winfsp_fs_skeleton::FsBackend;

mod cmd;
mod mount;
mod probe;

/// Plugs ext4 detection into [`winfsp_fs_skeleton`]'s SCM service +
/// foreground watcher. The four constants identify our consumer to
/// Windows + WinFsp.Launcher; `detect` is the byte-slice predicate
/// from [`crate::probe::is_ext4`].
struct Ext4Backend;

impl FsBackend for Ext4Backend {
    const FS_NAME: &'static str = "ext4";
    const SERVICE_NAME: &'static str = "ExtFsWatcher";
    const LAUNCHER_SERVICE_CLASS: &'static str = "ext4-mount";
    const FILE_EXTENSION: &'static str = "img";

    fn detect(bytes: &[u8]) -> bool {
        probe::is_ext4(bytes)
    }
}

#[derive(Parser)]
#[command(
    name = "ext4",
    about = "Browse and (eventually) mount ext4 volumes on Windows",
    version,
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Shared mount-source flags. `image` is the file or device path; `part`
/// optionally selects the Nth (1-indexed) partition in a whole-disk image.
///
/// `--part 0` is treated as "no partition" (i.e. the same as omitting
/// the flag). The ExtFsWatcher service relies on this when a disk
/// arrives without a partition table -- it always passes `--part`
/// because the WinFsp.Launcher CommandLine template is fixed, and
/// uses 0 to mean "open the whole device as the ext4 fs".
#[derive(Args, Clone)]
struct MountArgs {
    /// Disk image, ext4 filesystem image, or (Windows) raw device.
    image: PathBuf,
    /// 1-indexed partition number when `image` is a whole-disk image.
    /// See `ext4 parts <image>` for the partition list. `0` is treated
    /// the same as omitting the flag.
    #[arg(long, short = 'p')]
    part: Option<usize>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print volume info (label, block size, free space, ...).
    Info {
        #[command(flatten)]
        mt: MountArgs,
    },
    /// List directory entries.
    Ls {
        #[command(flatten)]
        mt: MountArgs,
        #[arg(default_value = "/")]
        path: String,
    },
    /// Verifier shape of `ls` for v2 recipe steps. Reads `path`, compares
    /// the resulting entry set against any combination of `--expect-name`
    /// (set membership, repeatable) and `--expect-count` (cardinality).
    /// Exits 0 on all-match, non-zero on any drift; drift detail goes to
    /// stderr so the harness's per-step `stderr.txt` carries enough to
    /// triage without re-running. With no flags, just confirms the path
    /// is a readable directory (open + close succeed) and exits 0.
    VerifyLs {
        #[command(flatten)]
        mt: MountArgs,
        #[arg(default_value = "/")]
        path: String,
        /// Expected directory entry name. Repeat for each name. Order
        /// doesn't matter; the comparison is set-based.
        #[arg(long = "expect-name")]
        expect_names: Vec<String>,
        /// Expected total entry count (including `.` and `..`).
        #[arg(long = "expect-count")]
        expect_count: Option<usize>,
    },
    /// Stat a single path.
    Stat {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Print a file's contents to stdout. Follows symlinks.
    Cat {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        /// Byte offset to start reading from (default: 0).
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// Maximum number of bytes to read (default: read to EOF).
        #[arg(long)]
        length: Option<u64>,
    },
    /// Recursive tree listing from /.
    Tree {
        #[command(flatten)]
        mt: MountArgs,
        #[arg(long, default_value_t = 64)]
        max_depth: u32,
    },
    /// Inspect partition table (MBR/GPT) of a disk image or raw device.
    Parts { image: PathBuf },
    /// Read-only filesystem audit. Walks every directory, compares each
    /// inode's link count to observed dirent references, and reports
    /// link-count drift / dangling entries / wrong `..` / etc. Exits 0
    /// if clean, non-zero if any anomaly is found.
    Audit {
        #[command(flatten)]
        mt: MountArgs,
        /// Cap directories visited (0 = unbounded). Useful for huge
        /// volumes where an exhaustive walk would take too long.
        #[arg(long, default_value_t = 0)]
        max_dirs: u32,
        /// Cap entries scanned per directory (0 = unbounded).
        #[arg(long, default_value_t = 0)]
        max_entries_per_dir: u32,
    },
    /// Print the target of a symbolic link.
    Readlink {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// List extended attribute names for a path (one per line).
    Listxattr {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Print the raw bytes of one extended attribute to stdout.
    Getxattr {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        name: String,
    },
    /// Set (create or replace) an extended attribute. The value is taken from
    /// the `--value` flag (UTF-8 string) or from stdin when `--stdin` is set.
    /// The name must include its namespace prefix (e.g. `user.myattr`).
    Setxattr {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        name: String,
        /// Attribute value as a UTF-8 string.
        #[arg(long, conflicts_with = "stdin")]
        value: Option<String>,
        /// Read attribute value from stdin (binary-safe).
        #[arg(long)]
        stdin: bool,
    },
    /// Remove an extended attribute. The name must include its namespace prefix.
    Removexattr {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        name: String,
    },
    /// Create a symbolic link. `target` is the link content (what the symlink
    /// points at); `linkpath` is the path of the new symlink inode.
    Symlink {
        #[command(flatten)]
        mt: MountArgs,
        /// What the symlink points at (the stored target string).
        target: String,
        /// Path of the new symlink inode to create.
        linkpath: String,
    },
    /// Create a hard link: `dst` becomes a new directory entry pointing at
    /// the same inode as `src`.
    Link {
        #[command(flatten)]
        mt: MountArgs,
        /// Existing file path (the inode to link to).
        src: String,
        /// New path to create.
        dst: String,
    },
    /// Write stdin to a file (creates or overwrites). Reads until EOF,
    /// truncates the file to the written length.
    Write {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Create a directory (mode 0755).
    Mkdir {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Remove an empty directory.
    Rmdir {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Remove a file (not a directory).
    Unlink {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Truncate a file to the given byte length.
    Truncate {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        size: u64,
    },
    /// Rename / move `src` to `dst`. Destination must not already exist;
    /// use `--replace` to atomically overwrite an existing destination.
    Rename {
        #[command(flatten)]
        mt: MountArgs,
        src: String,
        dst: String,
    },
    /// Create a file if it doesn't exist (like `touch`). Does not modify
    /// an existing file.
    Touch {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Change file permissions. `mode` is an octal integer (e.g. 644).
    Chmod {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        /// Octal mode (e.g. 755). Parsed as decimal by the shell; prefix
        /// with `0o` for an explicit octal literal if your shell allows it,
        /// or pass the decimal equivalent (e.g. 493 for 0755).
        mode: u32,
    },
    /// Change file owner / group.
    Chown {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        uid: u32,
        gid: u32,
    },
    /// Set inode flags (FS_IOC_SETFLAGS). `flags` is the full new flags word
    /// in decimal or hex (prefix `0x`). Common values:
    ///   0x10 = IMMUTABLE, 0x20 = APPEND_ONLY, 0x40 = NODUMP, 0x200 = NOATIME.
    Setflags {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        /// New flags value (hex with 0x prefix or decimal).
        #[arg(value_parser = parse_hex_or_dec)]
        flags: u32,
    },
    /// Create a special file: FIFO (named pipe), socket, char device, or
    /// block device. `mode` includes type bits + permissions in octal
    /// (e.g. 0o10644 for a FIFO with 0644 perms). `major` and `minor` are
    /// device numbers for char/block devices; omit (or pass 0) for FIFO/socket.
    Mknod {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        /// File type + permission bits in octal (e.g. 0o10644 = FIFO|0644).
        /// Type bits: FIFO=0o10000, socket=0o140000, char=0o20000, blk=0o60000.
        #[arg(value_parser = parse_hex_or_dec)]
        mode: u32,
        /// Major device number (0 for FIFO / socket).
        #[arg(default_value_t = 0)]
        major: u32,
        /// Minor device number (0 for FIFO / socket).
        #[arg(default_value_t = 0)]
        minor: u32,
    },
    /// Pre-allocate or punch a hole in a file.
    /// Flags: 0 = pre-allocate (may extend size), 1 = keep-size,
    /// 3 = punch-hole+keep-size, 16 = zero-range.
    Fallocate {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
        offset: u64,
        len: u64,
        #[arg(long, default_value_t = 0)]
        flags: i32,
    },
    /// Mount the filesystem on a Windows drive letter via WinFsp.
    /// Defaults to read-write; pass `--ro` for read-only. Requires the
    /// `mount` feature and a Windows host.
    #[cfg(all(windows, feature = "mount"))]
    Mount {
        #[command(flatten)]
        mt: MountArgs,
        /// Drive letter (`X:`) or empty directory to mount on.
        #[arg(long)]
        drive: String,
        /// Mount read-only.
        #[arg(long, conflicts_with = "rw")]
        ro: bool,
        /// Explicit read-write opt-in. Now the default; accepted for
        /// back-compat with scripts and harness configs that pre-date
        /// the flip.
        #[arg(long, conflicts_with = "ro")]
        rw: bool,
    },
    /// Watch for ext4 volumes plugging in (SD cards, USB drives) and
    /// auto-mount them by spawning `ext4 mount` as a child process.
    /// Windows-only; on other targets prints a hint and exits.
    Watch,
    /// Run as a Windows Service (SCM dispatcher). Same behaviour as
    /// `watch`, but mounts are launched through WinFsp.Launcher's
    /// `launchctl-<arch>.exe` so they appear in the active console
    /// session instead of session 0. Intended to be invoked by the
    /// SCM, not run interactively. Windows-only; non-Windows builds
    /// print a hint and exit.
    #[cfg(windows)]
    Service,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info { mt } => cmd::info(&mt),
        Cmd::Ls { mt, path } => cmd::ls(&mt, &path),
        Cmd::VerifyLs {
            mt,
            path,
            expect_names,
            expect_count,
        } => cmd::verify_ls(&mt, &path, &expect_names, expect_count),
        Cmd::Stat { mt, path } => cmd::stat(&mt, &path),
        Cmd::Cat { mt, path, offset, length } => cmd::cat(&mt, &path, offset, length),
        Cmd::Tree { mt, max_depth } => cmd::tree(&mt, max_depth),
        Cmd::Parts { image } => cmd::parts(&image),
        Cmd::Audit {
            mt,
            max_dirs,
            max_entries_per_dir,
        } => cmd::audit(&mt, max_dirs, max_entries_per_dir),
        Cmd::Readlink { mt, path } => cmd::readlink(&mt, &path),
        Cmd::Listxattr { mt, path } => cmd::listxattr(&mt, &path),
        Cmd::Getxattr { mt, path, name } => cmd::getxattr(&mt, &path, &name),
        Cmd::Setxattr { mt, path, name, value, stdin } => {
            let bytes = if stdin {
                use std::io::Read;
                let mut buf = Vec::new();
                std::io::stdin().read_to_end(&mut buf)?;
                buf
            } else {
                value.unwrap_or_default().into_bytes()
            };
            cmd::setxattr(&mt, &path, &name, &bytes)
        }
        Cmd::Removexattr { mt, path, name } => cmd::removexattr(&mt, &path, &name),
        Cmd::Symlink { mt, target, linkpath } => cmd::symlink(&mt, &target, &linkpath),
        Cmd::Link { mt, src, dst } => cmd::link(&mt, &src, &dst),
        Cmd::Write { mt, path } => cmd::write_file(&mt, &path),
        Cmd::Mkdir { mt, path } => cmd::mkdir(&mt, &path),
        Cmd::Rmdir { mt, path } => cmd::rmdir(&mt, &path),
        Cmd::Unlink { mt, path } => cmd::unlink(&mt, &path),
        Cmd::Truncate { mt, path, size } => cmd::truncate(&mt, &path, size),
        Cmd::Rename { mt, src, dst } => cmd::rename(&mt, &src, &dst),
        Cmd::Touch { mt, path } => cmd::touch(&mt, &path),
        Cmd::Chmod { mt, path, mode } => cmd::chmod(&mt, &path, mode),
        Cmd::Chown { mt, path, uid, gid } => cmd::chown(&mt, &path, uid, gid),
        Cmd::Setflags { mt, path, flags } => cmd::setflags(&mt, &path, flags),
        Cmd::Mknod { mt, path, mode, major, minor } => cmd::mknod(&mt, &path, mode as u16, major, minor),
        Cmd::Fallocate { mt, path, offset, len, flags } => cmd::fallocate(&mt, &path, offset, len, flags),
        #[cfg(all(windows, feature = "mount"))]
        Cmd::Mount {
            mt,
            drive,
            ro,
            rw: _, // accepted for back-compat; RW is now the default
        } => {
            let m = if ro {
                mount::Mount::open(&mt)?
            } else {
                mount::Mount::open_rw(&mt)?
            };
            mount::run(m, &drive)
        }
        Cmd::Watch => winfsp_fs_skeleton::watch::run::<Ext4Backend>(),
        #[cfg(windows)]
        Cmd::Service => winfsp_fs_skeleton::service::run::<Ext4Backend>(),
    }
}
