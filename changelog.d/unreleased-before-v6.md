### Changed

- **The licence is GPL-3.0-or-later**, as for every Windows driver in the
  family and winfsp-fs-skeleton. `Cargo.toml` declared GPL-3.0 only; it,
  the README and the winget manifest now say GPL-3.0-or-later. The licence
  text in LICENSE is unchanged.


### Added

- Scheduled-task drive-letter mount via InteractiveToken.

### Fixed

- **The WinFsp install fails its own step when nothing was installed.** `choco install winfsp` passes having installed nothing when the Chocolatey feed cannot serve the package, and the build then failed later in winfsp-sys; `scripts/install-winfsp.ps1` retries four times and fails unless WinFsp's headers are on disk (#49).
- **A directory entry that cannot be stat'ed fails the listing rather than vanishing.** A directory whose children could not be read listed to Windows as short, or empty, with no error; the listing now fails with the entry's error. A symlink whose target does not resolve is still left out.
- **A transient HTTP 5xx from the chore release download no longer fails a CI job.** Every chore download in the workflows retries up to five times on any error.
- Self-healing test-mount/test-unmount.
- Mode format order, data.len() u64 cast, clippy fixes.
- MacOS fallback for open_rw.
- Update stat/parts expectations for new CLI output format.
- Nothing is a submodule any more, and the release patch step said otherwise ([#19](https://github.com/antimatter-studios/ext4-win-driver/pull/19)).
- Each scenario's image type lives in volume_params, where the matrix schema puts it ([#31](https://github.com/antimatter-studios/ext4-win-driver/pull/31)).
- The builder VM's SSH port follows EXT4_BUILDER_PORT ([#32](https://github.com/antimatter-studios/ext4-win-driver/pull/32)).
- The driver is pinned to am-fs-ext4 0.7.0 and am-fs-core 0.2.19 ([#33](https://github.com/antimatter-studios/ext4-win-driver/pull/33)).
- Whole-disk-parts expects the GPT:linux label ext4 parts prints ([#34](https://github.com/antimatter-studios/ext4-win-driver/pull/34)).
- Basic-tree-hash pins the canonical tree, which carries no inode numbers ([#35](https://github.com/antimatter-studios/ext4-win-driver/pull/35)).
- The skeleton is pinned at v0.2.2, which lists logical partitions and mounts disks present at start ([#36](https://github.com/antimatter-studios/ext4-win-driver/pull/36)).
- The matrix's image builder is its own sibling, pinned at EXT4_IMAGE_BUILDER_REF ([#38](https://github.com/antimatter-studios/ext4-win-driver/pull/38)).

### Changed

- Add pre-commit hook (fmt + clippy) and install script.
- Named intermediates in unix_to_filetime_nsec + fmt.
- Document that MinGW GNU linker must not be installed.
- Bump rust-fs-ext4 + winfsp-rs submodule pointers.
- Format_mode_str named constants and dedup dir-name conversion.
- Bump rust-fs-ext4 to d312ea8 (helper extraction cleanup).
- Bump fs-test-harness to v3.10.0 (15f50b5).
- Bump winfsp-rs to upstream 0.13.0; drop fork.
- Cargo fmt.
- Bump rust-fs-core to v0.2.1.
- Bump winfsp-fs-skeleton to v0.1.1.
- Bump rust-fs-ext4 to v0.3.0 (fb9cda6).
- Siblings, CI, and the reader bump they were hiding ([#12](https://github.com/antimatter-studios/ext4-win-driver/pull/12)).
- No submodules left, and the release step that was missing ([#13](https://github.com/antimatter-studios/ext4-win-driver/pull/13)).
- Move to winfsp-rs 0.13.0, with the portability fix ([#15](https://github.com/antimatter-studios/ext4-win-driver/pull/15)).
- The harness is fs-windows-test-harness now (needs harness v4.0.0) ([#18](https://github.com/antimatter-studios/ext4-win-driver/pull/18)).
- Github-guard's hooks replace the in-tree .githooks ([#21](https://github.com/antimatter-studios/ext4-win-driver/pull/21)).
- The Windows test harness is pinned at v4.2.0, its current release ([#27](https://github.com/antimatter-studios/ext4-win-driver/pull/27)).
- The Windows matrix runs in CI through rust-fs-core's output budget ([#29](https://github.com/antimatter-studios/ext4-win-driver/pull/29)).
- A release is tested installed before it is published, and submitted to winget after ([#40](https://github.com/antimatter-studios/ext4-win-driver/pull/40)).
- The driver builds on rust-fs-ext4 0.8 and runs the family's scripts in place ([#41](https://github.com/antimatter-studios/ext4-win-driver/pull/41)).
- Wip.
