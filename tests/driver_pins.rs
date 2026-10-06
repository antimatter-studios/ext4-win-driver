//! The driver crates this binary links are pinned at or after the
//! releases that carry the 2026-09-06 hardening wave.
//!
//! `am-fs-ext4` 0.5.1 refuses journal entries that name a block outside
//! the filesystem, bounds the superblock fields a mount sizes itself
//! from, and keeps the allocator and journal writer inside the volume.
//! `am-fs-core` 0.2.7 stops a slice rebasing a read off the end of its
//! parent. Both crates are published as `rust-fs-ext4` (from 0.8.0) and
//! `rust-fs-core` (from 0.3.0) now, so the floors are the first release
//! under each new name, which carries all of it.
//!
//! Two places have to agree: `chores.yml`, which decides which tag of
//! each sibling is checked out, and `Cargo.lock`, which records the
//! version cargo actually resolved from that checkout. Both are read as
//! text, so this runs on any host.

use std::fs;
use std::path::Path;

/// The first release of each crate that carries the hardening wave.
const FLOORS: &[(&str, &str, (u64, u64, u64))] = &[
    ("FS_CORE_REF", "rust-fs-core", (0, 3, 0)),
    ("FS_EXT4_REF", "rust-fs-ext4", (0, 8, 0)),
];

fn read(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn parse_version(text: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = text
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|p| {
            p.parse()
                .unwrap_or_else(|_| panic!("not a version: {text:?}"))
        })
        .collect();
    match parts.as_slice() {
        [major, minor, patch] => (*major, *minor, *patch),
        _ => panic!("not a three-part version: {text:?}"),
    }
}

/// The value of `  KEY: value` under `vars:` in chores.yml.
fn chores_var(chores: &str, key: &str) -> String {
    let prefix = format!("  {key}:");
    chores
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| panic!("chores.yml declares no {key}"))
}

/// The version of the single `[[package]]` named `crate_name` in Cargo.lock.
fn locked_version(lock: &str, crate_name: &str) -> String {
    let wanted = format!("name = \"{crate_name}\"");
    let lines: Vec<&str> = lock.lines().collect();
    let mut found: Vec<String> = lines
        .windows(2)
        .filter(|pair| pair[0].trim() == wanted)
        .map(|pair| {
            pair[1]
                .trim()
                .strip_prefix("version = \"")
                .and_then(|l| l.strip_suffix('"'))
                .unwrap_or_else(|| panic!("Cargo.lock: {crate_name} has no version line"))
                .to_string()
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "Cargo.lock should hold exactly one {crate_name}, found {found:?}"
    );
    found.remove(0)
}

fn show((major, minor, patch): (u64, u64, u64)) -> String {
    format!("{major}.{minor}.{patch}")
}

#[test]
fn sibling_pins_carry_the_hardening_wave() {
    let chores = read("chores.yml");
    for (key, crate_name, floor) in FLOORS {
        let pinned = parse_version(&chores_var(&chores, key));
        assert!(
            pinned >= *floor,
            "chores.yml pins {crate_name} at {} ({key}); the hardening wave starts at {}",
            show(pinned),
            show(*floor)
        );
    }
}

#[test]
fn cargo_lock_agrees_with_the_sibling_pins() {
    let chores = read("chores.yml");
    let lock = read("Cargo.lock");
    for (key, crate_name, floor) in FLOORS {
        let pinned = parse_version(&chores_var(&chores, key));
        let locked = parse_version(&locked_version(&lock, crate_name));
        assert_eq!(
            locked,
            pinned,
            "Cargo.lock resolved {crate_name} {} but chores.yml checks out {}",
            show(locked),
            show(pinned)
        );
        assert!(
            locked >= *floor,
            "Cargo.lock resolved {crate_name} {}; the hardening wave starts at {}",
            show(locked),
            show(*floor)
        );
    }
}
