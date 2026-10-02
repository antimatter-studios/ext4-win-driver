#!/usr/bin/env bash
# core.sh NAME [ARG...] -- run rust-fs-core's family script NAME against the
# repository this file sits in.
#
#   scripts/core.sh test-floor TIER FLOOR      the tier ran at least FLOOR tests
#   scripts/core.sh semver-check               no undeclared public-API break
#   scripts/core.sh family-check               this repository keeps no copy of these
#
# THIS FILE IS THE SAME IN EVERY REPOSITORY, byte for byte, and its canonical
# copy is rust-fs-core's scripts/core.sh. It is the one piece a repository has
# to carry itself, because it is what finds rust-fs-core; everything it runs
# lives there and only there. The scripts it runs were once a separate copy in
# each repository, and the copies drifted: a fix made in one reached none of
# the others. Do not edit this copy -- change rust-fs-core's and copy it.
#
# WHERE CORE IS, in this order:
#   1. FS_CORE_ROOT, when set, and ONLY there: an override that falls through
#      to something else is not an override. A relative value is also tried
#      against this repository's root, decided by looking rather than by the
#      string's shape, which on Windows starts with a drive letter.
#   2. The sibling checkout ../rust-fs-core. Its path is built from this
#      file's own location, so it is POSIX on every runner, Git Bash
#      included, and a local change to core is exercised, not shadowed.
#   3. Wherever cargo resolved the am-fs-core dependency -- the registry copy
#      of the pinned release. Reading cargo's JSON needs python3.
#
# A COPY IS ACCEPTED ON ITS ANSWER TO --version AND NOTHING ELSE. A script
# that answers something else is fatal, not a reason to look elsewhere:
# "core is broken" reported as "core is missing" is the quieter failure.
#
# The script runs with FS_CORE_CALLER set to this repository's root, which is
# where it reads logs and manifests from.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    echo "usage: scripts/core.sh test-floor|semver-check|family-check [ARG...]" >&2
    exit 2
}
[ $# -ge 1 ] || usage
NAME="$1"
shift
case "$NAME" in
    test-floor|semver-check|family-check) ;;
    *) echo "core.sh: rust-fs-core has no family script named '$NAME'." >&2; usage ;;
esac
SCRIPT_REL="scripts/$NAME.sh"
EXPECTED="rust-fs-core-$NAME 1"

die() {
    echo "core.sh: $*" >&2
    echo "         $NAME is rust-fs-core's, at $SCRIPT_REL; this repository keeps no copy." >&2
    echo "         Expected: bash \$core/$SCRIPT_REL --version  ->  $EXPECTED" >&2
    echo "         Set FS_CORE_ROOT to a checkout of rust-fs-core, or check one out beside this one." >&2
    exit 1
}

if [ -n "${FS_CORE_ROOT:-}" ]; then
    SOURCE="$FS_CORE_ROOT/$SCRIPT_REL"
    [ -f "$SOURCE" ] || [ ! -f "$REPO/$SOURCE" ] || SOURCE="$REPO/$SOURCE"
elif [ -f "$REPO/../rust-fs-core/$SCRIPT_REL" ]; then
    SOURCE="$REPO/../rust-fs-core/$SCRIPT_REL"
else
    command -v python3 >/dev/null 2>&1 || \
        die "no sibling rust-fs-core, and python3 is needed to read cargo metadata."
    METADATA="$(cargo metadata --format-version 1 --locked \
        --manifest-path "$REPO/Cargo.toml" 2>/dev/null || true)"
    CORE_DIR="$(printf '%s' "$METADATA" | python3 -c '
import json, sys
try:
    packages = json.load(sys.stdin)["packages"]
except Exception:
    sys.exit(0)
print(next((p["manifest_path"].rsplit("/", 1)[0]
            for p in packages if p["name"] == "am-fs-core"), ""))
' || true)"
    [ -n "$CORE_DIR" ] || die "cargo could not say where am-fs-core is."
    SOURCE="$CORE_DIR/$SCRIPT_REL"
fi

[ -f "$SOURCE" ] || die "$SOURCE does not exist."
FOUND="$(bash "$SOURCE" --version 2>/dev/null || true)"
[ "$FOUND" = "$EXPECTED" ] || die "$SOURCE answered --version with '$FOUND', not '$EXPECTED'."

FS_CORE_CALLER="$REPO" exec bash "$SOURCE" "$@"
