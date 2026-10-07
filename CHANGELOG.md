# Changelog

Notable changes to `ext4-win-driver`, newest first. Sections for releases before this
file existed were drawn from the commits between their tags; from here on each
release's section is written before it is tagged, and the GitHub release's
notes are that section (rust-fs-core's `release-notes`).

## [Unreleased]

### Added

- Scheduled-task drive-letter mount via InteractiveToken.

### Fixed

- **A directory entry that cannot be stat'ed fails the listing rather than vanishing.** A directory whose children could not be read listed to Windows as short, or empty, with no error; the listing now fails with the entry's error. A symlink whose target does not resolve is still left out.
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

## [0.2.4] — 2026-05-28

### Added

- Add setxattr, removexattr, symlink, link subcommands.
- Expose ext4 xattrs as Windows EAs (get/set_extended_attributes).
- Add write, mkdir, rmdir, unlink, truncate, rename subcommands.
- Add touch, chmod, chown; fix write to create file if missing.
- Improve stat output with mode string and UTC timestamps.
- Add fallocate subcommand (preallocate, punch-hole, zero-range).
- Add --offset / --length to cat for partial file reads.
- Use nsec timestamps, inode_flags, blocks_512 from new attr fields.
- Add setflags subcommand (FS_IOC_SETFLAGS wrapper).
- Add mknod subcommand for special files.

### Fixed

- Sub-second timestamp precision + accurate hard_links count.
- Review fixes — mknod check, ea_size probe, removexattr error, partial write.

### Changed

- Extract apply_ea_buffer; wire EA data at file creation.
- Bump rust-fs-ext4 to 5078331.
- Bump rust-fs-ext4 to c43aa4c.
- Bump rust-fs-ext4 to b896b9b.
- Update Cargo.lock for caseless + unicode-normalization deps.
- Bump rust-fs-ext4 to 191a5e4 (changelog).
- Bump rust-fs-ext4 to ce55e99 (mknod).

## [0.2.3] — 2026-05-28

### Added

- Per-recipe ext4 image builds via persistent Alpine VM.
- Follow symlinks transparently in open and stat.
- Add listxattr/getxattr subcommands; unblock xattr-getxattr scenario.
- Add readlink subcommand.
- Add readlink subcommand and basic-readlink scenario.

### Fixed

- Expand VM_HOST/SSH_KEY/workdir/image_dir from .test-env.
- Implement GetDirInfoByName to fix Remove-Item on directories.
- Cat now dereferences symlinks (up to 8 hops).
- Tree now recurses depth-first, fixing visually misleading output.
- Use readlink return value as length; remove dead delete field.

### Changed

- Replace serialize_mounts=true with max_parallel=1 (v3.7.0 compat).
- Bump all submodules to latest.
- Bump vendors to latest tags + add run-matrix.sh.
- Rename harness.toml → fs-test-harness.toml.
- Add 3 new RW scenarios for subdirectory operations.
- Add write-durability and symlink-read scenarios; fix stale rw-mkdir-rmdir status.
- Add WinFsp-side tests for htree, inline-data, and deep-extents images.
- Add rw-overwrite-durability scenario.
- Add verify-cat step to basic-stat-symlink for CLI symlink coverage.
- Gitignore *.img disk image build artifacts.
- Add builder-ssh, test-mount, test-unmount dev helpers.
- Bump rust-fs-ext4 to 05d1b48 (htree hash_seed + loop-device + openssh batch).

## [0.2.2] — 2026-05-13

### Changed

- Bump to v0.2.2 -- fix x64 CI regression (winfsp bindgen).

## [0.2.1] — 2026-05-13

### Added

- Adopt run-tests.sh + pilot the v2 recipe shape.
- Phase A — migrate 17 host-side scenarios from v1 ops to v2 recipes.
- Phase B — migrate 8 RW scenarios to v2 vm-side recipes; full v2 matrix.

### Fixed

- Drop maintainer-specific defaults; review fixes.

### Changed

- Self-create the GitHub Release if absent.
- Pin real InstallerSha256 from GH release artefacts.
- Bump schema 1.10.0 -> 1.12.0.
- Bump all five submodules to upstream tip.
- Retarget fs-test-harness submodule onto post-3.0.0 main.
- Track fs-test-harness main post-PR#6 merge.
- Bump fs-test-harness to v3.3.0 + adopt harness-shipped verifier scripts.

## [0.2.0] — 2026-05-09

### Added

- Flip default to read-write; add --ro toggle.
- Map FILE_ATTRIBUTE_READONLY to POSIX 0o222 write bits.
- Pwrite streaming writes + expanded errno + rename2 replace.

### Changed

- Build x64 + arm64 Setup.exe per release tag (GH Actions).
- Stage v0.1.0 submission manifests.
- Drop choco LLVM, point LIBCLANG_PATH at the LLVM-MinGW bundle.
- Locate libclang.dll dynamically inside the LLVM-MinGW archive.
- Commit GPL-3 LICENSE text + the README screenshot.
- Install libclang via KyleMayes/install-llvm-action; defang AVX-512.
- Add -U_ReadWriteBarrier to bindgen + re-add WiX extensions before build.
- Patch fsctl.h around _ReadWriteBarrier; consolidate wix extension add.
- Finalise v0.1.0 SHA256s for both architectures.
- Consume winfsp-fs-skeleton; delete duplicated platform plumbing.
- Explain the skeleton split + point installer/ at the upstream.
- Bump windows-sys 0.59 -> 0.61 to dedupe with skeleton.
- Re-add .github/PULL_REQUEST_TEMPLATE.md.
- Drop dead probe_path + SB_PROBE_LEN from src/probe.rs.
- Cfg-gate RW mount openers + write/flush callbacks.
- Gate WinFsp mutating callbacks on Mount.writable.
- Bump rust-fs-ext4 submodule to pwrite/rename2/long-symlinks.
- Bump winfsp-fs-skeleton submodule to dd75206.
- V0.2.0 -- winget-validation hardening + Bundle.wxs publisher fix.

## [0.1.0] — 2026-05-08

### Added

- --part N mounts a GPT/MBR partition via callback shim.
- WinFsp read-only mount via patched winfsp-rs fork.
- WiX 4 MSI + .img right-click verb.
- Auto-mount ext4 volumes on Windows volume arrival.
- WinFsp read-write mount via --rw flag.
- Read-only fsck CLI + post-verify hook wiring.
- Windows Service + WinFsp.Launcher auto-mount path.
- WiX 4 wiring for ExtFsWatcher + WinFsp.Launcher + .img verb.

### Fixed

- Use HTTPS URLs for fs-test-harness + winfsp-rs.
- Use DEV_BROADCAST_DEVICEINTERFACE_W as the RegisterDeviceNotificationW filter.
- Pin crt-static + panic=abort so SCM can load the gnullvm service binary.
- Listen for disk arrivals, walk partitions, mount per partition.
- Sector-align FileSource reads on Windows raw devices.
- Write WinFsp.Launcher service-class to WOW6432Node + 3-arg template.
- Start ExtFsWatcher service immediately after install.

### Changed

- Initial scaffold: ext4 CLI on top of fs-ext4.
- BlockSource trait — Win32 raw-device support via FileSource.
- README — WinFsp RO mount working, build prereqs.
- Mount status via stdout, drop wrapper Tee.
- Consume shared fs-test-harness instead of in-tree scaffold.
- Ship whole-disk fixture builder + wire --part scenario.
- Add `{extra}` plumbing + `info` op template.
- Expand to 26 scenarios — RO + RW depth coverage.
- Document the hook + add audit-cli marker scenario.
- Switch fs-test-harness to a git submodule at ./harness.
- Bump harness submodule to 7e3ebb1 (CI severity + canonicalize binary path).
- Bump harness submodule to d89d8b3 (CI green: 4/4).
- Move third-party deps under vendor/.
- Bump submodules + add rust-fs-core.
- Extract probe + drive-letter helpers into src/probe.rs.
- -Arch param, BootstrapperApplications rename, arch-suffix artefacts.
- Ignore installer dist/ build artefacts.
- Keep PowerShell scripts ASCII + CRLF for PS 5.1 parseability.
- WiX 7 schema migration for MSI + Burn bundle.

[Unreleased]: https://github.com/antimatter-studios/ext4-win-driver/compare/v0.2.4...HEAD
[0.2.4]: https://github.com/antimatter-studios/ext4-win-driver/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/antimatter-studios/ext4-win-driver/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/antimatter-studios/ext4-win-driver/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/antimatter-studios/ext4-win-driver/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/antimatter-studios/ext4-win-driver/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/antimatter-studios/ext4-win-driver/releases/tag/v0.1.0
