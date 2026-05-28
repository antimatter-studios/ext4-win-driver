#!/usr/bin/env bash
# SSH to the Alpine ext4 builder VM.
#
# Separate from the Windows VM SSH mux: this always connects to the local
# QEMU Alpine VM on localhost:2222 using the auto-generated builder key.
# Uses EXT4_REAL_SSH (exported by run-matrix.sh before the mux wrapper is
# added to PATH) so the mux ControlMaster is never involved.
#
# Usage: bash scripts/builder-ssh.sh "<remote command>"

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
builder_key="$repo_root/vendor/rust-fs-ext4/test-disks/.vm-cache/builder-key"

exec "${EXT4_REAL_SSH:-ssh}" \
    -p 2222 \
    -i "$builder_key" \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes \
    -o ControlPath=none \
    root@localhost \
    "$@"
