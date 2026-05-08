# winget manifests

Per-version submission manifests for [winget-pkgs](https://github.com/microsoft/winget-pkgs).
One subdirectory per release tag.

## Structure

`v<X.Y.Z>/` mirrors the on-disk layout that winget-pkgs expects under
`manifests/a/AntimatterStudios/ext4-win-driver/<X.Y.Z>/`:

- `AntimatterStudios.ext4-win-driver.yaml` -- version manifest (points
  at the locale + installer files).
- `AntimatterStudios.ext4-win-driver.locale.en-US.yaml` -- description,
  license, tags.
- `AntimatterStudios.ext4-win-driver.installer.yaml` -- per-arch
  installer URL + SHA256.

## Submission flow

1. Tag the release; the GH Actions `release.yml` workflow uploads
   per-arch Setup.exe artefacts to the GitHub Release.
2. Compute SHA256 of each Setup.exe:
   `shasum -a 256 dist/ext4-win-driver-<ver>-<arch>-Setup.exe`
3. Update the matching `Architecture` block in
   `installer.yaml` (replace `TBD-AFTER-CI-UPLOAD`).
4. Validate locally:
   `winget validate --manifest .\winget\v<X.Y.Z>\`
5. Fork [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs),
   copy the three YAML files into
   `manifests/a/AntimatterStudios/ext4-win-driver/<X.Y.Z>/`, push, and
   open a PR.

## Notes

- `InstallerType: burn` matches our Setup.exe (a WiX Burn bootstrapper).
- `Scope: machine` -- the MSI is per-machine, requires admin elevation.
- `UpgradeBehavior: install` -- newer versions invoke the new installer
  and our MSI's MajorUpgrade rule replaces the old install in-place.
- Code-signing is intentionally not done. winget reviewers may require
  some additional disclosure for unsigned Burn bundles; if the
  submission is rejected on that ground, the installer will need a
  signing cert before resubmitting.
