# Post-verify hook for ext4-win-driver

Status: **not implemented**. Tracked here + via the
`audit-post-verify-marker` scenario in `test-matrix.json` (status
`blocked-needs-audit-cli`).

This doc records the design so the next agent who picks it up does not
have to re-derive it. Three architectures are viable; option (c) is the
cheapest near-term win.

## What the harness already supports

The shared `fs-test-harness` runner (see
`../fs-test-harness/scripts/run-scenario.ps1`, stage E) already wires a
post-verify hook through the matrix. There is nothing to change in the
harness — the consumer just has to populate two fields:

1. **`harness.toml [post_verify]`** — default command for every scenario
   that does not override it.
2. **Per-scenario `post_verify` block** — override or opt-in for a
   specific scenario.

The schema (`../fs-test-harness/schemas/test-matrix.schema.json`)
defines:

```json
"post_verify": {
  "command":     "<template, with {image} and {drive} substitution>",
  "expect_exit": 0
}
```

`run-scenario.ps1` runs this command **after** stage D (mount teardown)
and **only** when the scenario otherwise passed. Output is captured to
`post-verify-stdout.txt` / `post-verify-stderr.txt` under the diag
directory; non-zero exit (or mismatch with `expect_exit`) flips the
verdict from `passed` to `failed`.

So the missing piece is purely "what command do we run."

## Why we don't currently invoke `fsck.ext4`

Two friction points block the obvious choice:

- `fsck.ext4` is **not on the Windows VM** that runs scenarios. e2fsprogs
  is a Linux-native package; on Windows it would have to come via WSL or
  a hand-built MSYS2 / Cygwin port, both of which add a non-trivial
  dependency to every contributor's VM.
- Even if installed, the post-mount image lives on the VM's disk inside
  `harness.toml [vm.workdir]`. `fsck.ext4` would need direct file-mode
  access to the image after WinFsp has fully released it. WinFsp on
  forced-teardown can leave the host process in TIME_WAIT briefly, so
  this is not a blocker but it is a sequencing concern.

## Two viable "real fsck" architectures

### (a) Mac-side `fsck.ext4 -fn` after diag pull

Workflow:

1. The Windows VM finishes the scenario, leaves the post-RW image in
   `vm.workdir`.
2. `scripts/test-windows-matrix.sh` (Mac orchestrator) `scp`s the image
   back to the Mac.
3. Mac runs `fsck.ext4 -fn <image>` (Homebrew `e2fsprogs`,
   `brew install e2fsprogs`; the binary lives at
   `/opt/homebrew/opt/e2fsprogs/sbin/fsck.ext4`).
4. Verdict is added to the diag bundle and reported alongside the
   scenario's runner verdict.

Pros: real upstream `fsck.ext4`, runs Mac-side so no VM bloat. Cons:
**doesn't fit the harness's stage-E hook** — that runs on the VM, not
the orchestrator. Implementing this means a separate "post-pull verify"
step in `test-windows-matrix.sh`, not a `[post_verify]` template. It
also requires every contributor to have e2fsprogs installed locally.

### (b) Sidecar Linux container/VM accessible from the Mac orchestrator

Workflow:

1. Mac orchestrator pulls the post-RW image (same as option a).
2. Hands the image to a small Linux VM (Lima, OrbStack, qemu-user-static
   container, etc.).
3. That VM runs `fsck.ext4 -fn` and returns exit code + log.

Pros: hermetic, deterministic, identical fsck binary across
contributors. Cons: significant infrastructure — adds a VM/container
dependency to bootstrap. Same architectural mismatch as (a): not a
`[post_verify]` template, lives in the orchestrator script.

### (c) Interim: extend ext4-win-driver itself with an `audit` subcommand

This is the cheapest near-term option and the one we are tracking.

The underlying `rust-fs-ext4` library already exposes a read-only audit:
see `Filesystem::audit` in `../rust-fs-ext4/src/fsck.rs` (around line
446). It is a subset of `e2fsck` — walks the inode/extent/dir-block
tree and surfaces structural anomalies. Not as thorough as upstream
`fsck.ext4`, but cheap, in-tree, and runs on Windows because
`rust-fs-ext4` is pure Rust.

Once a tiny `ext4 audit <image>` clap subcommand is added to
`src/main.rs` that wraps `Filesystem::audit(...)` and exits non-zero on
any reported anomaly, the post-verify hook is a one-liner in
`harness.toml`:

```toml
[post_verify]
command     = "{binary} audit {image}"
expect_exit = 0
```

That's it. The harness already does the rest — runs the command after
mount teardown, captures output, flips the verdict on non-zero exit.

The matrix scenario `audit-post-verify-marker` (status
`blocked-needs-audit-cli`) carries the per-scenario shape of the same
hook for reviewers re-reading the matrix later:

```json
"post_verify": {
  "command": "{binary} audit {image}",
  "expect_exit": 0
}
```

When the `audit` subcommand lands, three follow-up edits unblock the
hook:

1. Add `[post_verify]` block above to `harness.toml`.
2. Flip `audit-post-verify-marker.status` from `blocked-needs-audit-cli`
   to `pending`.
3. Decide whether every RW scenario should inherit the global
   `[post_verify]` (probably yes).

## Why this is parked, not done now

The task that produced this doc explicitly carved the `audit` CLI out
as a follow-up commit ("Do not add any new CLI subcommand to
ext4-win-driver in this commit. The `audit` command is an explicit
follow-up; document it without implementing."). This file plus the
marker scenario keep the design visible until that follow-up lands.
