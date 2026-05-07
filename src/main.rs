//! ext4-win-driver CLI entry point.
//!
//! Thin clap dispatcher; real work lives in [`cmd`].

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
mod partition;

#[derive(Parser)]
#[command(name = "ext4", about = "Browse and (eventually) mount ext4 volumes on Windows")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print volume info (label, block size, free space, ...).
    Info {
        image: PathBuf,
    },
    /// List directory entries.
    Ls {
        image: PathBuf,
        #[arg(default_value = "/")]
        path: String,
    },
    /// Stat a single path.
    Stat {
        image: PathBuf,
        path: String,
    },
    /// Print a file's contents to stdout.
    Cat {
        image: PathBuf,
        path: String,
    },
    /// Recursive tree listing from /.
    Tree {
        image: PathBuf,
        #[arg(long, default_value_t = 64)]
        max_depth: u32,
    },
    /// Inspect partition table (MBR/GPT) of a disk image or raw device.
    Parts {
        image: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info { image } => cmd::info(&image),
        Cmd::Ls { image, path } => cmd::ls(&image, &path),
        Cmd::Stat { image, path } => cmd::stat(&image, &path),
        Cmd::Cat { image, path } => cmd::cat(&image, &path),
        Cmd::Tree { image, max_depth } => cmd::tree(&image, max_depth),
        Cmd::Parts { image } => cmd::parts(&image),
    }
}
