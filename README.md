# ext4-win-driver

Windows-first userspace tooling for ext4 volumes, built on the `fs-ext4` Rust
library.

Scope:

1. **CLI browser** — open an ext4 image or raw device; list/stat/read files
   without mounting. Cross-platform (works on macOS/Linux too — useful for
   testing).
2. **WinFsp driver** — mount an ext4 partition as a real Windows drive letter
   via [WinFsp](https://github.com/winfsp/winfsp). Read-only today,
   read-write to follow.

The `fs-ext4` library lives at [`vendor/rust-fs-ext4/`](./vendor/rust-fs-ext4)
(git submodule from [christhomas/rust-fs-ext4](https://github.com/christhomas/rust-fs-ext4))
and is path-depended; this crate is the distribution unit.

## Status

- [x] CLI: `info`, `ls`, `stat`, `cat`, `tree`, `parts`
- [x] MBR/GPT partition parsing (5 unit tests)
- [x] `--part N` mounts a partition slice via the C ABI's callback mount
- [x] Win32 raw-device support (`\\.\X:`, `\\.\PhysicalDriveN`)
- [x] **WinFsp read-only mount** — verified end-to-end on Windows 11 ARM64
- [x] **WinFsp read-write mount** (`--rw`) — create/write/truncate/rename/
      unlink/rmdir/mkdir/utimens wired through to the C ABI. v1 caveat:
      `write` round-trips the whole file (the underlying ABI is a "save-as"
      replace), so large-file workloads are slow until a positional
      `pwrite` lands in `fs-ext4`.
- [ ] MSI installer (bundles WinFsp)

## Usage

CLI (works on any host):

```
ext4 info   <image>                        # volume label, sizes, features
ext4 ls     <image> [path]                 # directory listing
ext4 stat   <image> <path>
ext4 cat    <image> <path>
ext4 tree   <image>
ext4 parts  <image>                        # MBR/GPT partition table
ext4 ls     <whole-disk.img> --part 1 /    # browse partition 1
```

WinFsp mount (Windows + `mount` feature):

```
ext4 mount <image> --drive X:           # read-only (default)
ext4 mount <image> --drive X: --rw      # read-write
```

Then browse `X:` in Explorer, or `Get-ChildItem X:\`, etc. Ctrl-C to unmount.

## Build

CLI only (any platform):

```
cargo build --release
```

With WinFsp mount (Windows only):

```
cargo build --release --features mount
```

### WinFsp build prerequisites

- **WinFsp 2.1+** installed on the build/run machine
  ([winfsp.dev](https://winfsp.dev/) → MSI, or `winget install WinFsp.WinFsp`).
- For ARM64 / `aarch64-pc-windows-gnullvm` targets, a forked
  [winfsp-rs](https://github.com/SnowflakePowered/winfsp-rs) is path-depended
  at `../winfsp-rs` (gnullvm support patches; upstream PR pending). The fork
  also requires:
  - `LLVM` for `libclang.dll` (`winget install LLVM.LLVM`)
  - LLVM-MinGW (`winget install MartinStorsjo.LLVM-MinGW.UCRT`)
- `LIBCLANG_PATH=C:\Program Files\LLVM\bin` so bindgen can find `libclang.dll`.

## Testing

Scenarios live in [`test-matrix.json`](./test-matrix.json) and the per-project
adapter config in [`harness.toml`](./harness.toml). Both are consumed by the
shared [`fs-test-harness`](https://github.com/antimatter-studios/fs-test-harness),
vendored as a git submodule at [`vendor/fs-test-harness/`](./vendor/fs-test-harness).

After cloning, initialise the submodules:

```sh
git submodule update --init --recursive
```

One-time VM setup, on the Mac:

```sh
bash vendor/fs-test-harness/scripts/setup-local.sh        # writes .test-env
```

Run a scenario end-to-end (Mac → SSH → Windows VM → diag pull):

```sh
bash vendor/fs-test-harness/scripts/test-windows-matrix.sh basic-ro-list
```

Diagnostics land under `test-diagnostics/run-<UTC>/`. See the harness's
[`docs/triage-protocol.md`](./vendor/fs-test-harness/docs/triage-protocol.md)
for how to read a failure, and
[`docs/multi-agent-protocol.md`](./vendor/fs-test-harness/docs/multi-agent-protocol.md)
for running multiple agents against the same matrix.

To update a vendored submodule when its upstream releases:

```sh
git submodule update --remote --merge vendor/fs-test-harness
git add vendor/fs-test-harness && git commit -m "chore: bump fs-test-harness submodule"
```

## License

GPL-3.0 — inherited from the WinFsp Rust bindings. The CLI alone (without the
`mount` feature) doesn't link winfsp and could be relicensed if split out, but
the single-license declaration keeps the distribution unit simple.
