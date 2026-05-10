#!/usr/bin/env bash
# verify-cat.sh -- v2 op: read a file and assert against expectations.
#
# Usage:
#   verify-cat.sh <image> <path> [--expect-size N] [--expect-sha256 HEX]
#                                [--expect-content STR]
#                                [--expect-stdout-sha256 HEX]
#
# `--expect-stdout-sha256` is just an alias for `--expect-sha256` for v1
# parity (the v1 matrix uses both interchangeably for cat). Substitution
# from the matrix is via `{step.expect_size?}` / `{step.expect_sha256?}`
# / `{step.expect_content?}`; missing fields collapse to empty and the
# corresponding check is skipped.
#
# Exit:
#   0 = all configured expectations satisfied
#   1 = mismatch (drift dumped to stderr; harness captures it)
#   2 = invocation error

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${repo_root}/target/release/ext4"

if [[ $# -lt 2 ]]; then
    echo "verify-cat.sh: missing <image> <path>" >&2
    exit 2
fi
image="$1"; path="$2"; shift 2

want_size=""; want_sha256=""; want_content=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-size)              want_size="$2"; shift 2 ;;
        --expect-sha256|--expect-stdout-sha256) want_sha256="$2"; shift 2 ;;
        --expect-content)           want_content="$2"; shift 2 ;;
        *) echo "verify-cat.sh: unknown flag: $1" >&2; exit 2 ;;
    esac
done

# Capture cat output to a tempfile — bash's $(...) strips trailing
# newlines, which would skew both the byte count and the sha256 for
# any file ending in \n (basically all text files).
tmp=$(mktemp -t verify-cat.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" cat "${image}" "${path}" > "${tmp}"
fail=0

if [[ -n "${want_size}" ]]; then
    got_size=$(wc -c < "${tmp}" | tr -d ' ')
    if [[ "${got_size}" != "${want_size}" ]]; then
        echo "verify-cat: size mismatch at ${path}: got=${got_size} want=${want_size}" >&2
        fail=1
    fi
fi
if [[ -n "${want_sha256}" ]]; then
    got_sha256=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got_sha256}" != "${want_sha256}" ]]; then
        echo "verify-cat: sha256 mismatch at ${path}: got=${got_sha256} want=${want_sha256}" >&2
        fail=1
    fi
fi
if [[ -n "${want_content}" ]]; then
    # Read the file losslessly into a variable for byte-exact compare.
    # mapfile/bash <(...) would also work but `$(<file)` is portable
    # back to bash 3.2 (macOS default). Trailing newlines are still
    # stripped by command substitution; for content checks we use cmp
    # against a tempfile holding the expected payload.
    expect_tmp=$(mktemp -t verify-cat-expect.XXXXXX)
    printf '%s' "${want_content}" > "${expect_tmp}"
    if ! cmp -s "${tmp}" "${expect_tmp}"; then
        echo "verify-cat: content mismatch at ${path}:" >&2
        echo "  got (first 200B):  $(head -c 200 "${tmp}")" >&2
        echo "  want:              ${want_content}" >&2
        fail=1
    fi
    rm -f "${expect_tmp}"
fi

exit "${fail}"
