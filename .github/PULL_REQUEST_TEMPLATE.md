## Summary

<1-3 bullets describing what changed and why>

## Test plan

- [ ] `cargo build --release --features mount,service` clean on Windows host
- [ ] `cargo test` passes (incl. partition + probe unit tests)
- [ ] If touching auto-mount: VHD smoke test on the dev VM (attach -> drive letter appears -> detach -> drive letter gone)
- [ ] If touching installer: Setup.exe install + uninstall on a clean Windows VM
- [ ] If touching the skeleton boundary: `winfsp-fs-skeleton` submodule pointer updated and matching commit pushed there too

## Skeleton coupling

- [ ] No new `crate::` imports of code that should live in `winfsp-fs-skeleton` (drive-letter / partition / device / disk-arrival / WM_DEVICECHANGE patterns belong upstream)
- [ ] If a skeleton fix is needed, it lands first; this PR bumps the submodule

## Notes

<anything reviewers should know -- breaking changes, follow-ups, etc>
