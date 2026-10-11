# changelog.d

Changes that have not been released yet, one file per pull request. A
release gathers them into a dated section of [`CHANGELOG.md`](../CHANGELOG.md)
and deletes them, so this directory holds only what the next release will
say (agent-core v6, "Changelog entries are fragments").

A fragment is named for its branch -- `fix/run-end-bound` writes
`fix-run-end-bound.md` -- and holds one or more level-3 headings, each over
`- ` entries:

```markdown
### Fixed

- **The run-end bound is checked.** A run that ends past the device is
  refused rather than read.
```

Keep a Changelog's headings (`Added`, `Changed`, `Deprecated`, `Removed`,
`Fixed`, `Security`) come first in the release, in that order; any other
heading follows them. No `#` or `##` headings: the release adds the version.

- `../rust-fs-core/scripts/changelog-check.sh` (run in place, in `chore lint`) refuses a malformed
  fragment, and an entry written into `CHANGELOG.md` directly.
- `../rust-fs-core/scripts/changelog-gather.sh VERSION` is the release step.

A change no consumer would notice -- tests, CI, documentation -- needs no
fragment.
