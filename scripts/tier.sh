#!/usr/bin/env bash
# tier.sh TIER -- COMMAND [ARG...]
#
# One tier, run QUIETLY and under a budget. The whole run goes to
# tmp/logs/<TIER>.log. A pass prints one verdict line naming the log. A
# failure prints the verdict, the command's status and the log's path, and a
# run that passed but printed more than its budget fails with status 65, which
# tells it apart from a failing command (that exits with the command's status).
#
# The work is done by rust-fs-core's canonical scripts/output-budget.sh. This
# file asks cargo where core is, copies that script for the run, and owns only
# the part that is ours: which tiers exist and how much each may print.
# chores.yml and ci.yml both run through here, so a developer and CI are held
# to the same number.
#
# VERBOSE. `OUTPUT_BUDGET_VERBOSE=1`, or `--verbose`/`-v` in a chore
# invocation's CLI_ARGS (`chore matrix -- --verbose`), streams the run as it
# happens as well as logging it. It does NOT lift the budget: the log is the
# same size either way. The harness's own FWTH_VERBOSE is not read by core's
# wrapper; core names the replacement on stderr when it sees it set.
#
# A FAILURE PRINTS NO TAIL by default. `OUTPUT_BUDGET_FAIL_TAIL=N` brings the
# last N lines back for whoever is watching.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[ $# -ge 3 ] || { echo "tier.sh: usage: tier.sh TIER -- COMMAND [ARG...]" >&2; exit 2; }
TIER="$1"; shift
[ "$1" = "--" ] || { echo "tier.sh: expected -- after the tier name, got '$1'" >&2; exit 2; }
shift

# Lines / bytes each tier may print on a PASSING run. Every number here was
# measured, and each budget is the measurement plus roughly a third, so
# ordinary growth fits and a change in kind (a test that starts printing, a
# flag that turns on per-step output) does not. Raise one deliberately, with
# the measurement that justifies it, in the same edit.
#
#   tier           measured (lines / bytes)                         budget
#   check          36 / 1,828 cold, 5 / 480 warm (CI ubuntu)        48 / 2,450
#   check-windows  239 / 12,643 cold, 157 / 9,000 warm (CI)         320 / 16,900
#   build-windows  229 / 12,138 cold (CI windows-latest)            305 / 16,200
#   clippy         2 / 187 (CI ubuntu and macos, cold and warm)     3 / 250
#   test           44 / 2,291 cold, 22 / 1,333 warm (CI ubuntu)     59 / 3,050
#   matrix         910 / 40,147 on a RED run (see below)            1,220 / 53,600
#
# Measured on GitHub's runners from the verdict lines of runs 37071187610,
# 37071802059 and 37074571843 (2026-10-02). The cold figures are the first
# run after Cargo.lock changed: a line per crate compiled, plus, on Windows,
# the driver's own warnings.
#
# CLIPPY RUNS AFTER CHECK in the same job, so it prints only its own crate's
# line and cargo's `Finished`: two lines cold or warm. A third of two is not
# a line, so the budget is the measurement plus one line; any warning fails
# the step under -D warnings anyway.
#
# THE MATRIX ROW IS PROVISIONAL, and the only one not measured on a passing
# run. The first CI run of test-matrix.json (run 37074571843, 35 scenarios
# dispatched in 71 s) failed 34 of them for reasons of the CI shape -- the
# fixes are in this change -- and printed 910 lines / 40,147 bytes, every
# failure carrying its step's stderr. A green run prints less than a red one,
# so a budget a third above the red run fits a green run with room, and it
# still catches a run that starts printing per step. Replace it with a green
# run's measurement plus a third once the matrix passes. It is also the only
# tier whose length depends on a machine (the VM, its SSH banner, its mount
# timing) rather than on this repository, so it is the one most likely to
# need raising -- with a measurement.
case "$TIER" in
    check)         MAX_LINES=48;   MAX_BYTES=2450 ;;
    check-windows) MAX_LINES=320;  MAX_BYTES=16900 ;;
    build-windows) MAX_LINES=305;  MAX_BYTES=16200 ;;
    clippy)        MAX_LINES=3;    MAX_BYTES=250 ;;
    test)          MAX_LINES=59;   MAX_BYTES=3050 ;;
    matrix)        MAX_LINES=1220; MAX_BYTES=53600 ;;
    *)
        echo "tier.sh: '$TIER' has no budget. Add a measured row to scripts/tier.sh." >&2
        exit 2
        ;;
esac

# THE WRAPPER IS COPIED FROM rust-fs-core FOR THIS RUN, AND DELETED AFTER IT.
# It belongs to core and is deliberately not committed here: a committed copy
# drifts, and `scripts/core.sh family-check` fails a repository that keeps one.
#
# CARGO IS ASKED WHERE CORE IS. am-fs-core reaches this crate through
# am-fs-ext4's `path = "../rust-fs-core"`, so cargo's answer is the sibling
# checkout at FS_CORE_REF; with a registry dependency it would be the
# registry's copy of the pinned release. The wrapper needs core v0.2.13 or
# later. Compilation flags belong to the wrapped command, not to this probe.
set +e
CORE_MANIFEST="$(RUSTFLAGS= RUSTDOCFLAGS= \
    cargo metadata --format-version 1 --locked --manifest-path "$REPO/Cargo.toml" \
    2>/dev/null | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
print(next((p["manifest_path"]
            for p in packages if p["name"] == "am-fs-core"), ""))
' 2>/dev/null)"
metadata_status=$?
set -e
if [ -n "$CORE_MANIFEST" ] && command -v cygpath >/dev/null 2>&1; then
    # Cargo returns a Windows path on Git Bash runners; shell file tests need
    # the corresponding POSIX path.
    CORE_MANIFEST="$(cygpath -u "$CORE_MANIFEST")"
fi
CORE_DIR="$(dirname "${CORE_MANIFEST:-.}")"
if [ "$metadata_status" -ne 0 ] || [ -z "$CORE_MANIFEST" ] || [ ! -f "$CORE_DIR/scripts/output-budget.sh" ]; then
    echo "tier.sh: cargo could not say where am-fs-core is, or its copy has no" >&2
    echo "         scripts/output-budget.sh. The wrapper lives in rust-fs-core;" >&2
    echo "         run 'chore siblings' so ../rust-fs-core is at FS_CORE_REF" >&2
    echo "         (v0.2.13 or later ships it)." >&2
    exit 1
fi

# WHAT CARGO POINTED AT IS ASKED TO IDENTIFY ITSELF. A path that exists is not
# the same as core's wrapper; `--version` is the contract core publishes for
# exactly this check.
EXPECTED_API="rust-fs-core-output-budget 1"
if [ "$(bash "$CORE_DIR/scripts/output-budget.sh" --version 2>/dev/null || true)" != "$EXPECTED_API" ]; then
    echo "tier.sh: $CORE_DIR/scripts/output-budget.sh is there, but does not" >&2
    echo "         answer --version with '$EXPECTED_API'. That is a broken or" >&2
    echo "         far too old rust-fs-core, not an absent one." >&2
    exit 1
fi

BUDGET="$REPO/tmp/output-budget.$$.sh"
mkdir -p "$REPO/tmp"
trap 'rm -f "$BUDGET"' EXIT
cp "$CORE_DIR/scripts/output-budget.sh" "$BUDGET"

case " ${CLI_ARGS:-} " in
    *" --verbose "*|*" -v "*) export OUTPUT_BUDGET_VERBOSE=1 ;;
esac

LOG="$REPO/tmp/logs/$TIER.log"

set +e
bash "$BUDGET" \
    --log "$LOG" \
    --max-lines "$MAX_LINES" \
    --max-bytes "$MAX_BYTES" \
    --label "$TIER" \
    -- "$@"
status=$?
set -e

# A TEST THAT DECIDED NOT TO RUN IS NOT A TEST THAT PASSED. libtest's (and the
# matrix runner's libtest-mimic's) `N ignored` is reported here; a `SKIP:` line
# from a test that returned early fails the tier.
if [ -f "$LOG" ]; then
    skips=$(grep -ac '^SKIP:' "$LOG" || true)
    ignored=$( (grep -aoE '[0-9]+ ignored' "$LOG" || true) | awk '{s+=$1} END{print s+0}')
    if [ "${ignored:-0}" -gt 0 ]; then
        echo "$TIER: $ignored test(s) ignored"
    fi
    if [ "${skips:-0}" -gt 0 ]; then
        echo "::error::$TIER: $skips test(s) printed SKIP and were counted as passing. A skipped test is not a passing test; see $LOG" >&2
        grep -a '^SKIP:' "$LOG" | sed 's/^/    /' >&2
        exit 66
    fi
fi

exit "$status"
