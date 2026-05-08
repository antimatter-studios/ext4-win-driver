//! ext4-win-driver CLI entry point.
//!
//! Thin clap dispatcher; real work lives in [`cmd`].

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
mod device;
mod mount;
mod partition;
mod probe;
mod service;
mod watch;

#[derive(Parser)]
#[command(name = "ext4", about = "Browse and (eventually) mount ext4 volumes on Windows")]
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
    /// Stat a single path.
    Stat {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
    },
    /// Print a file's contents to stdout.
    Cat {
        #[command(flatten)]
        mt: MountArgs,
        path: String,
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
    /// Mount the filesystem on a Windows drive letter via WinFsp.
    /// Defaults to read-only; pass `--rw` for read-write. Requires the
    /// `mount` feature and a Windows host.
    #[cfg(all(windows, feature = "mount"))]
    Mount {
        #[command(flatten)]
        mt: MountArgs,
        /// Drive letter (`X:`) or empty directory to mount on.
        #[arg(long)]
        drive: String,
        /// Mount read-write (default = read-only).
        #[arg(long)]
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
        Cmd::Stat { mt, path } => cmd::stat(&mt, &path),
        Cmd::Cat { mt, path } => cmd::cat(&mt, &path),
        Cmd::Tree { mt, max_depth } => cmd::tree(&mt, max_depth),
        Cmd::Parts { image } => cmd::parts(&image),
        Cmd::Audit {
            mt,
            max_dirs,
            max_entries_per_dir,
        } => cmd::audit(&mt, max_dirs, max_entries_per_dir),
        #[cfg(all(windows, feature = "mount"))]
        Cmd::Mount { mt, drive, rw } => {
            let m = if rw {
                mount::Mount::open_rw(&mt)?
            } else {
                mount::Mount::open(&mt)?
            };
            mount::run(m, &drive)
        }
        Cmd::Watch => watch::run(),
        #[cfg(windows)]
        Cmd::Service => service::run(),
    }
}
