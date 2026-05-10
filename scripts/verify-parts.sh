#!/usr/bin/env bash
# verify-parts.sh -- v2 op: assert against `ext4 parts <image>` output.
#
# `ext4 parts` exits non-zero on whole-disk-no-table images (where the
# image is one ext4 volume directly, not a partitioned disk). Scenarios
# that test that case set `expect_exit = 1` on the v2 op-def and skip
# stdout assertions; this script returns whatever exit code the binary
# returned in the no-stdout-check case so the harness's expect_exit
# comparison sees it.
#
# Usage:
#   verify-parts.sh <image> [--expect-stdout-contains "STR"]

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${repo_root}/target/release/ext4"

if [[ $# -lt 1 ]]; then
    echo "verify-parts.sh: missing <image>" >&2
    exit 2
fi
image="$1"; shift

want_contains=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-stdout-contains) want_contains="$2"; shift 2 ;;
        *) echo "verify-parts.sh: unknown flag: $1" >&2; exit 2 ;;
    esac
done

# Capture both stdout and exit code. Don't fail the script on non-zero
# exit — the harness's expect_exit field handles that judgement.
out="$("${binary}" parts "${image}" 2>&1)"
rc=$?

if [[ -n "${want_contains}" ]]; then
    if ! echo "${out}" | grep -qF "${want_contains}"; then
        echo "verify-parts: stdout missing expected substring: '${want_contains}'" >&2
        echo "got stdout (rc=${rc}):" >&2
        echo "${out}" | sed 's/^/  /' >&2
        exit 1
    fi
fi

exit "${rc}"
