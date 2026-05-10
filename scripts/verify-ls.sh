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

exec "${binary}" verify-ls "$@"
