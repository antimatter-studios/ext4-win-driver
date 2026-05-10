#!/usr/bin/env bash
# verify-stat.sh -- v2 op: assert against `ext4 stat` output for a path.
#
# Usage:
#   verify-stat.sh <image> <path>
#                  [--expect-size N]
#                  [--expect-mode 0o644]
#                  [--expect-stdout-contains "STR"]
#
# Substitution from the matrix is via `{step.expect_size?}` /
# `{step.expect_mode?}` / `{step.expect_stdout_contains?}`; missing
# fields collapse to empty and the corresponding check is skipped.
#
# Exit:
#   0 = all configured expectations satisfied
#   1 = mismatch (drift dumped to stderr)
#   2 = invocation error

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${repo_root}/target/release/ext4"

if [[ $# -lt 2 ]]; then
    echo "verify-stat.sh: missing <image> <path>" >&2
    exit 2
fi
image="$1"; path="$2"; shift 2

want_size=""; want_mode=""; want_contains=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-size)             want_size="$2";     shift 2 ;;
        --expect-mode)             want_mode="$2";     shift 2 ;;
        --expect-stdout-contains)  want_contains="$2"; shift 2 ;;
        *) echo "verify-stat.sh: unknown flag: $1" >&2; exit 2 ;;
    esac
done

out="$("${binary}" stat "${image}" "${path}")"
fail=0

if [[ -n "${want_size}" ]]; then
    got_size=$(echo "${out}" | awk '/^size:/ {print $2; exit}')
    if [[ "${got_size}" != "${want_size}" ]]; then
        echo "verify-stat: size mismatch at ${path}: got=${got_size} want=${want_size}" >&2
        fail=1
    fi
fi
if [[ -n "${want_mode}" ]]; then
    got_mode=$(echo "${out}" | awk '/^mode:/ {print $2; exit}')
    if [[ "${got_mode}" != "${want_mode}" ]]; then
        echo "verify-stat: mode mismatch at ${path}: got=${got_mode} want=${want_mode}" >&2
        fail=1
    fi
fi
if [[ -n "${want_contains}" ]]; then
    if ! echo "${out}" | grep -qF "${want_contains}"; then
        echo "verify-stat: stdout missing expected substring at ${path}: '${want_contains}'" >&2
        echo "got stdout:" >&2
        echo "${out}" | sed 's/^/  /' >&2
        fail=1
    fi
fi

exit "${fail}"
