#!/usr/bin/env bash
# SSH to the Alpine ext4 builder VM.
#
# Separate from the Windows VM SSH mux: this always connects to the local
# QEMU Alpine VM on localhost:$EXT4_BUILDER_PORT (2222 when unset; run-matrix.sh
# exports 2223) using the auto-generated builder key, and offers only that key
# (IdentitiesOnly) so a full ssh agent cannot exhaust the server's auth
# attempts first. Uses EXT4_REAL_SSH (exported by run-matrix.sh before the
# mux wrapper is added to PATH) so the mux ControlMaster is never involved.
#
# Usage: bash scripts/builder-ssh.sh "<remote command>"

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
builder_key="$repo_root/../rust-fs-ext4/test-disks/.vm-cache/builder-key"
builder_port="${EXT4_BUILDER_PORT:-2222}"

exec "${EXT4_REAL_SSH:-ssh}" \
    -p "$builder_port" \
    -i "$builder_key" \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes \
    -o ControlPath=none \
    root@localhost \
    "$@"
