#!/usr/bin/env bash
# verify-ls.sh -- v2 op: assert that the dirent name-set at <path> on
# <image> matches the expected set.
#
# Invoked by the fs-test-harness v2 dispatcher per recipe step. The
# harness expands `{scenario.image}` to an absolute path (under
# `[vm].image_dir`), `{step.path}` to the directory under test, and
# `{step.expect_names_args}` to the pre-formatted `--expect-name X
# --expect-name Y ...` argv slice.
#
# Wraps the consumer binary's `verify-ls` subcommand so the per-
# consumer adapter contract (host-side bash) is consistent across
# ops. The binary does the actual comparison and exits 0/1; we just
# resolve its host-side path (no .exe) and forward argv.
#
# Usage:
#   verify-ls.sh <image> <path> --expect-name N1 [--expect-name N2 ...]
#
# Exit:
#   0 = name-set matches
#   1 = drift (binary prints diff to stderr; harness captures to
#       <step>/stderr.txt for triage)

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${repo_root}/target/release/ext4"

if [[ ! -x "${binary}" ]]; then
    echo "verify-ls.sh: ${binary} not found or not executable" >&2
    echo "verify-ls.sh: build with 'cargo build --release --bin ext4' first" >&2
    exit 2
fi

# If the caller asked for --expect-stdout-sha256, that's a textual
# assertion against `ext4 ls` output — handled here in shell rather
# than baked into the binary's verify-ls subcommand. Strip it from
# argv, run the real `ls`, hash, compare. Forward everything else to
# `verify-ls`.
expect_stdout_sha256=""
binary_args=()
i=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-stdout-sha256)
            expect_stdout_sha256="$2"; shift 2 ;;
        *)
            binary_args+=("$1"); shift ;;
    esac
done

if [[ -n "${expect_stdout_sha256}" ]]; then
    # Need image and path to do the textual ls. Convention: image is
    # binary_args[0], path is binary_args[1] (matches binary's
    # verify-ls positional order).
    if [[ "${#binary_args[@]}" -lt 2 ]]; then
        echo "verify-ls.sh: --expect-stdout-sha256 requires <image> <path>" >&2
        exit 2
    fi
    img="${binary_args[0]}"
    pth="${binary_args[1]}"
    extra=("${binary_args[@]:2}")
    tmp=$(mktemp -t verify-ls.XXXXXX)
    trap 'rm -f "${tmp}"' EXIT
    "${binary}" ls "${img}" "${pth}" "${extra[@]}" > "${tmp}"
    got=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got}" != "${expect_stdout_sha256}" ]]; then
        echo "verify-ls: stdout sha256 mismatch at ${pth}:" >&2
        echo "  got:  ${got}" >&2
        echo "  want: ${expect_stdout_sha256}" >&2
        exit 1
    fi
fi

# Remaining structural assertions (--expect-name / --expect-count) go
# to the binary's verify-ls subcommand. If no remaining flags AND no
# stdout-sha256 was requested, we'd be running verify-ls with just
# image+path, which the binary now treats as "open + close, exit 0".
exec "${binary}" verify-ls "${binary_args[@]}"
