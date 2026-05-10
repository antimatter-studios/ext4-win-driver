#!/usr/bin/env bash
# verify-tree.sh -- v2 op: assert sha256 of the `ext4 tree` output.
#
# Tree output is volume-stable when the matrix image is built
# deterministically (build-ext4-feature-images.sh in the rust-fs-ext4
# project ensures this). A drifted hash means either the image
# regenerated differently or the tree-walk implementation changed
# its output format.
#
# Usage:
#   verify-tree.sh <image> [--expect-stdout-sha256 HEX]

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${repo_root}/target/release/ext4"

if [[ $# -lt 1 ]]; then
    echo "verify-tree.sh: missing <image>" >&2
    exit 2
fi
image="$1"; shift

want_sha256=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-stdout-sha256) want_sha256="$2"; shift 2 ;;
        *) echo "verify-tree.sh: unknown flag: $1" >&2; exit 2 ;;
    esac
done

# tempfile capture — bash's $(...) strips trailing newlines, which
# changes the sha256 for any output ending in \n.
tmp=$(mktemp -t verify-tree.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" tree "${image}" > "${tmp}"
fail=0

if [[ -n "${want_sha256}" ]]; then
    got_sha256=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got_sha256}" != "${want_sha256}" ]]; then
        echo "verify-tree: stdout sha256 mismatch:" >&2
        echo "  got:  ${got_sha256}" >&2
        echo "  want: ${want_sha256}" >&2
        fail=1
    fi
fi

exit "${fail}"
