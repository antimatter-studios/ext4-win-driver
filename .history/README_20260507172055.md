# ext4-win-driver

Windows-first userspace tooling for ext4 volumes, built on the `fs-ext4` Rust
library.

Scope:

1. **CLI browser** — open an ext4 image or raw device, list/stat/read files
   without root mounting anything. Cross-platform (works on macOS/Linux too,
   useful for testing).
2. **WinFsp driver** *(planned)* — mount an ext4 partition as a real Windows
   drive letter via [WinFsp](https://github.com/winfsp/winfsp).

The library lives at `../rust-fs-ext4` and is path-depended; this crate is the
distribution unit.

## Status

- [x] CLI scaffold (`info`, `ls`, `stat`, `cat`, `tree`, `parts`)
- [x] MBR/GPT partition parsing
- [x] `--part N` mounts a partition via the C ABI's callback mount
- [x] Win32 raw-device support (`\\.\X:`, `\\.\PhysicalDriveN`) — cfg-gated,
      validated via `cargo check --target x86_64-pc-windows-gnu`
- [ ] WinFsp read-only mount
- [ ] WinFsp read-write mount
- [ ] MSI installer (bundles WinFsp)

## Usage (current)

```
ext4 info  <image>
ext4 ls    <image> <path>
ext4 stat  <image> <path>
ext4 cat   <image> <path>
ext4 tree  <image>
```

`<image>` is a path to a raw ext4 filesystem image (no partition table). On
Windows, raw-device support (`\\.\X:`, `\\.\PhysicalDriveN`) and partition
selection land in subsequent iterations.
