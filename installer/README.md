# ext4-win-driver installer

WiX 7 source for the ext4-win-driver Windows installer. Two artefacts:

> **Upstream:** these wxs files are an ext4-customised copy of the
> templates shipped in
> [winfsp-fs-skeleton/templates/installer/](https://github.com/antimatter-studios/winfsp-fs-skeleton/tree/main/templates/installer).
> Future filesystem drivers (qcow2 / ntfs / ...) start from the same
> templates with their own product name + GUIDs substituted in. If
> you fix a structural bug here, propagate it back to the skeleton's
> templates so other consumers benefit.

| File                                          | Audience                | What it does                                              |
|-----------------------------------------------|-------------------------|-----------------------------------------------------------|
| `ext4-win-driver-<ver>-<arch>-Setup.exe`      | **end users (default)** | Burn bootstrapper: installs WinFsp first, then the MSI.   |
| `ext4-win-driver-<ver>-<arch>.msi`            | IT admins (SCCM/Intune) | Plain MSI; assumes WinFsp is already deployed separately. |

`<arch>` is `x64` or `arm64` — must match the `ext4.exe` you embed.

Both ship `ext4.exe`, `Mount-Ext4.ps1`, an Explorer right-click "Mount as
ext4" verb on `.img` files, and Start Menu shortcuts.

## Prerequisites

1. **Rust** + the `mount` feature build chain (see top-level `README.md`).
2. **WiX 4+ toolset:**
   ```powershell
   dotnet tool install --global wix
   wix extension add WixToolset.Util.wixext
   wix extension add WixToolset.BootstrapperApplications.wixext   # bundle UI (WiX 7+; was WixToolset.Bal.wixext in WiX 4)
   ```
   Or via winget: `winget install WiXToolset.WiX`.
3. **Internet access on first build** — `build.ps1` downloads the WinFsp
   redistributable MSI into `installer\redist\` and caches it. Subsequent
   builds are offline.

## Build

From the repo root:

```powershell
# 1. Build the binary with WinFsp support.
cargo build --release --features mount

# 2a. x64 host: build x64 MSI + Setup.exe (default).
installer\build.ps1 -ExePath target\release\ext4.exe

# 2b. arm64 host: build arm64 MSI + Setup.exe.
installer\build.ps1 -ExePath target\release\ext4.exe -Arch arm64
```

Outputs in `dist\`:

- `ext4-win-driver-<ver>-<arch>-Setup.exe` — ship this.
- `ext4-win-driver-<ver>-<arch>.msi`        — admin channel only.

`build.ps1` parameters:

| Param         | Default                                                      |
|---------------|--------------------------------------------------------------|
| `-ExePath`    | *(required)* — path to the release `ext4.exe`                |
| `-Version`    | parsed from `Cargo.toml`                                     |
| `-OutputDir`  | `<repo>\dist`                                                |
| `-MsiOnly`    | switch — skip the bundle stage                               |
| `-Arch`       | `x64` (default) or `arm64` — must match `ext4.exe`           |

`-Arch` must match the embedded `ext4.exe`'s Rust target:

| `-Arch` | Rust target                          | PE Machine |
|---------|--------------------------------------|------------|
| `x64`   | `x86_64-pc-windows-msvc`             | `0x8664`   |
| `arm64` | `aarch64-pc-windows-gnullvm`         | `0xAA64`   |

The script PE-sniffs `$ExePath` at startup and prints a warning if its
`IMAGE_FILE_HEADER.Machine` doesn't agree with `-Arch`. The mismatched
MSI still builds — Burn will install it cleanly — but the binary will
not run on the host CPU.

## Bumping the WinFsp pin

```sh
installer/update-winfsp-pin.sh           # check for drift (exit 1 if stale)
installer/update-winfsp-pin.sh --apply   # rewrite build.ps1 in place
```

The script queries the WinFsp GitHub releases via `gh`, picks the latest
non-prerelease tag, finds the `winfsp-<ver>.msi` asset, reads its SHA256
from the asset metadata, and updates the four `$WinFsp*` constants near
the top of [`build.ps1`](build.ps1). Requires `gh` (authenticated) and
`jq`.

`$WinFspVersion` feeds `Bundle.wxs`'s `DetectCondition` so end users
already running a newer WinFsp don't get downgraded.

## What the MSI does

- Installs `ext4.exe`, `Mount-Ext4.ps1`, and `LICENSE.txt` into
  `%ProgramFiles%\ext4-win-driver\`.
- Appends the install dir to the **system** `PATH`.
- Adds Explorer right-click "Mount as ext4" on `.img` files (via
  `HKCR\SystemFileAssociations\.img\shell\MountAsExt4` — non-destructive;
  the built-in Disk Management "Mount" verb keeps working for VHD/ISO).
- Adds two Start Menu shortcuts under "ext4-win-driver":
  - **Mount ext4 image...** — file picker → `ext4 mount`.
  - **ext4 watch service** — runs `ext4 watch` in a console.
- Cleans up everything on uninstall.

## What the bundle does

`Setup.exe` runs the WinFsp MSI first (skipped if a WinFsp ≥
`$WinFspVersion` is already installed — detected via
`HKLM\SOFTWARE\WOW6432Node\WinFsp\Version`), then chains the
ext4-win-driver MSI. WinFsp is left in place on uninstall
(`Permanent="yes"`) because other apps (sshfs-win, rclone, …) may rely
on it.

## Uninstall

Standard "Apps & features" / Add-Remove-Programs entry, or:

```powershell
# Bundle install:
"%ProgramData%\Package Cache\{b6d4e8a1-3f29-4c7d-9e22-1a8c5d6f7e34}\ext4-win-driver-<ver>-Setup.exe" /uninstall

# MSI-only install:
msiexec /x dist\ext4-win-driver-<ver>.msi
```

## Troubleshooting

### `wix extension list -g` reports `WixToolset.Bal.wixext 7.0.0 (damaged)`

The Bal extension was renamed in WiX 7 to `WixToolset.BootstrapperApplications.wixext`.
The old `WixToolset.Bal.wixext` package on NuGet is now an empty shim
that always reports `(damaged)`. The fix is to install the **new** name
instead:

```powershell
wix extension remove -g WixToolset.Bal.wixext            # may error; ignore
Remove-Item -Recurse -Force "$env:USERPROFILE\.wix\extensions\WixToolset.Bal.wixext" -ErrorAction SilentlyContinue
wix extension add -g WixToolset.BootstrapperApplications.wixext
wix extension list -g                                     # should show "WixToolset.BootstrapperApplications.wixext 7.0.0" with no "(damaged)"
```

`build.ps1` already passes `-ext WixToolset.BootstrapperApplications.wixext`
to the bundle stage, so once the extension is installed under the new
name the build picks it up automatically.

The `bal:` xmlns + element names in `Bundle.wxs` are unchanged — the
schema namespace URL is the same; only the NuGet package + DLL name
moved.

### Generic `(damaged)` on any extension

If a different WiX extension goes corrupt, the standard repair is:

```powershell
wix extension remove -g <name>
Remove-Item -Recurse -Force "$env:USERPROFILE\.wix\extensions\<name>"
wix extension add -g <name>
```

If global re-add still produces `(damaged)`, do a per-project install
from inside `installer\`:

```powershell
Push-Location installer
wix extension add <name>      # no -g — drops a .wix folder next to the WXS sources
Pop-Location
```

`build.ps1` already runs `wix build` from `installer\` (`Push-Location
$scriptDir`), so a per-project extension is picked up without code
changes.

### `build.ps1` complains about a string literal / `'` mismatch

PowerShell 5.1 reads `.ps1` files as Windows-1252 unless they have a
UTF-8 BOM. The script contains em-dashes in comments, which mojibake
without the BOM and turn into broken string literals. Save the file as
UTF-8 with BOM (or use PowerShell 7+, which defaults to UTF-8).

## Notes

- The `UpgradeCode` GUIDs in `Product.wxs` and `Bundle.wxs` are **stable**
  across versions — do not regenerate them.
- The MSI is per-arch (`-Arch x64` or `-Arch arm64`). Build both arches
  separately if you need to ship both — they install side-by-side via
  the arch suffix in the artefact name.
- `Mount-Ext4.ps1` is also usable stand-alone — copy it next to a dev
  `ext4.exe` and run it.
