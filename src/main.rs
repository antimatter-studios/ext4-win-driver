//! ext4-win-driver CLI entry point.
//!
//! Thin clap dispatcher; real work lives in [`cmd`].

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
mod device;
mod partition;

#[derive(Parser)]
#[command(name = "ext4", about = "Browse and (eventually) mount ext4 volumes on Windows")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Shared mount-source flags. `image` is the file or device path; `part`
/// optionally selects the Nth (1-indexed) partition in a whole-disk image.
#[derive(Args, Clone)]
struct MountArgs {
    /// Disk image, ext4 filesystem image, or (Windows) raw device.
    image: PathBuf,
    /// 1-indexed partition number when `image` is a whole-disk image.
    /// See `ext4 parts <image>` for the partition list.
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
    }
}
