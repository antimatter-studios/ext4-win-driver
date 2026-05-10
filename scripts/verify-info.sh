#!/usr/bin/env bash
# verify-info.sh -- v2 op: assert against `ext4 info <image>` output.
#
# Usage:
#   verify-info.sh <image> [--expect-stdout-contains "STR"]
#
# Repeatable via separate recipe steps when multiple substrings need
# to be asserted (one verify-info step per substring keeps each step
# independently failable + per-step diag).

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${repo_root}/target/release/ext4"

if [[ $# -lt 1 ]]; then
    echo "verify-info.sh: missing <image>" >&2
    exit 2
fi
image="$1"; shift

want_contains=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-stdout-contains) want_contains="$2"; shift 2 ;;
        *) echo "verify-info.sh: unknown flag: $1" >&2; exit 2 ;;
    esac
done

out="$("${binary}" info "${image}")"
fail=0

if [[ -n "${want_contains}" ]]; then
    if ! echo "${out}" | grep -qF "${want_contains}"; then
        echo "verify-info: stdout missing expected substring: '${want_contains}'" >&2
        echo "got stdout:" >&2
        echo "${out}" | sed 's/^/  /' >&2
        fail=1
    fi
fi

exit "${fail}"
