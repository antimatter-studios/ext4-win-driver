# ext4-win-driver MSI installer

WiX 4 source for the ext4-win-driver Windows installer. Produces a single
`.msi` that installs `ext4.exe`, the `Mount-Ext4.ps1` helper, an Explorer
right-click "Mount as ext4" verb on `.img` files, and Start Menu shortcuts.

## Prerequisites

1. **Rust** + the `mount` feature build chain (see top-level `README.md`).
2. **WiX 4+ toolset.** Install with one of:
   ```powershell
   # .NET global tool (recommended — current as of 2026):
   dotnet tool install --global wix
   wix extension add WixToolset.Util.wixext

   # Or via winget:
   winget install WiXToolset.WiX
   ```
3. **WinFsp 2.x** on the *target* machine where the MSI will be installed.
   The MSI does not bundle WinFsp; the installer aborts with a friendly
   message if WinFsp is missing. Get it from <https://winfsp.dev/>.

## Build

From the repo root:

```powershell
# 1. Build the binary with WinFsp support.
cargo build --release --features mount

# 2. Build the MSI.
installer\build.ps1 -ExePath target\release\ext4.exe
```

Output: `dist\ext4-win-driver-<version>.msi` (version pulled from
`Cargo.toml` unless `-Version` is passed).

`build.ps1` parameters:

| Param      | Default                                       |
|------------|-----------------------------------------------|
| `-ExePath` | *(required)* — path to the release `ext4.exe` |
| `-Version` | parsed from `Cargo.toml`                      |
| `-Output`  | `dist\ext4-win-driver-<version>.msi`          |

## What the MSI does

- Installs `ext4.exe`, `Mount-Ext4.ps1`, and `LICENSE.txt` into
  `%ProgramFiles%\ext4-win-driver\`.
- Appends the install dir to the **system** `PATH`.
- Detects WinFsp via `HKLM\SOFTWARE\WOW6432Node\WinFsp\InstallDir` (32-bit
  view) and `HKLM\SOFTWARE\WinFsp\InstallDir` (64-bit view). Aborts with a
  pointer to <https://winfsp.dev/> if neither is present.
- Adds Explorer right-click "Mount as ext4" on `.img` files (via
  `HKCR\SystemFileAssociations\.img\shell\MountAsExt4` — non-destructive;
  the built-in Disk Management "Mount" verb keeps working for VHD/ISO).
- Adds two Start Menu shortcuts under "ext4-win-driver":
  - **Mount ext4 image...** — file picker → `ext4 mount`.
  - **ext4 watch service** — runs `ext4 watch` in a console.
- Cleans up everything on uninstall (MSI infrastructure handles it).

## Uninstall

Standard "Apps & features" / Add-Remove-Programs entry, or:

```powershell
msiexec /x dist\ext4-win-driver-<version>.msi
```

## Notes

- The `UpgradeCode` GUID in `Product.wxs` is **stable** across versions —
  do not regenerate it, or major-upgrade detection breaks.
- The installer is `perMachine` and 64-bit-only (`-arch x64`). ARM64
  support requires re-building with `-arch arm64` and a matching
  `ext4.exe` build.
- `Mount-Ext4.ps1` is also usable stand-alone if you don't want to install
  the MSI — copy it next to a dev `ext4.exe` and run it.
