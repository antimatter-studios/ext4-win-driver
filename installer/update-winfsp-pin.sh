#!/usr/bin/env bash
# Refresh the WinFsp pin in build.ps1 to the latest stable release.
#
# Queries github.com/winfsp/winfsp via `gh`, picks the newest non-prerelease
# tag, finds the `winfsp-<ver>.msi` asset, reads its sha256 from the asset
# digest field, and rewrites the four `$WinFsp*` constants near the top of
# build.ps1.
#
# Usage:
#   installer/update-winfsp-pin.sh           # show drift, do not modify
#   installer/update-winfsp-pin.sh --apply   # rewrite build.ps1 in place
#
# Requires: gh (authenticated), jq, sed, awk.

set -euo pipefail

apply=0
case "${1:-}" in
    --apply) apply=1 ;;
    "")      ;;
    *)       echo "usage: $0 [--apply]" >&2; exit 2 ;;
esac

script_dir="$(cd "$(dirname "$0")" && pwd)"
build_ps1="$script_dir/build.ps1"

[ -f "$build_ps1" ] || { echo "build.ps1 not found at $build_ps1" >&2; exit 1; }
command -v gh >/dev/null || { echo "gh CLI not found in PATH" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq not found in PATH"      >&2; exit 1; }

# --- Discover latest stable release ----------------------------------------
tag=$(gh release list --repo winfsp/winfsp --exclude-pre-releases --limit 1 \
        --json tagName --jq '.[0].tagName')
[ -n "$tag" ] || { echo "could not determine latest WinFsp tag" >&2; exit 1; }

read -r name sha256 < <(
    gh release view "$tag" --repo winfsp/winfsp --json assets --jq '
        .assets[]
        | select(.name | test("^winfsp-[0-9.]+\\.msi$"))
        | "\(.name) \(.digest | sub("^sha256:"; ""))"
    ' | head -n1
)
[ -n "$name" ] || { echo "no winfsp-*.msi asset on release $tag" >&2; exit 1; }

# winfsp-2.1.25156.msi → 2.1.25156
version=$(printf '%s' "$name" | sed -E 's/^winfsp-([0-9.]+)\.msi$/\1/')

# Reconstructed URL — keeps $WinFspMsiName interpolation in build.ps1.
url_template="https://github.com/winfsp/winfsp/releases/download/$tag/\$WinFspMsiName"

# --- Read current pin from build.ps1 ---------------------------------------
cur_version=$(awk -F"'" '/^\$WinFspVersion[[:space:]]*=/ {print $2; exit}' "$build_ps1")
cur_sha256=$( awk -F"'" '/^\$WinFspSha256[[:space:]]*=/  {print $2; exit}' "$build_ps1")

printf 'current pin: %s  sha256=%s\n' "${cur_version:-<unset>}" "${cur_sha256:-<unset>}"
printf 'latest pin:  %s  sha256=%s  (%s)\n' "$version" "$sha256" "$tag"

if [ "$cur_version" = "$version" ] && [ "$cur_sha256" = "$sha256" ]; then
    echo 'up to date — nothing to do.'
    exit 0
fi

if [ "$apply" -eq 0 ]; then
    echo 'drift detected — re-run with --apply to rewrite build.ps1.'
    exit 1
fi

# --- Rewrite build.ps1 in place --------------------------------------------
# Use a portable sed — `sed -i` differs between BSD and GNU, so write to a
# temp and move.
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

sed -E \
    -e "s|^(\\\$WinFspVersion[[:space:]]*=[[:space:]]*)'[^']*'|\\1'$version'|" \
    -e "s|^(\\\$WinFspMsiName[[:space:]]*=[[:space:]]*)'[^']*'|\\1'$name'|" \
    -e "s|^(\\\$WinFspUrl[[:space:]]*=[[:space:]]*)\"[^\"]*\"|\\1\"$url_template\"|" \
    -e "s|^(\\\$WinFspSha256[[:space:]]*=[[:space:]]*)'[^']*'|\\1'$sha256'|" \
    "$build_ps1" > "$tmp"

# Sanity-check: all four lines must have changed exactly as expected.
for var in WinFspVersion WinFspMsiName WinFspUrl WinFspSha256; do
    if ! grep -q "^\\\$$var[[:space:]]*=" "$tmp"; then
        echo "rewrite failed: \$$var line missing in output" >&2
        exit 1
    fi
done

mv "$tmp" "$build_ps1"
trap - EXIT

echo "build.ps1 updated:"
echo "  \$WinFspVersion = '$version'"
echo "  \$WinFspMsiName = '$name'"
echo "  \$WinFspUrl     = \"$url_template\""
echo "  \$WinFspSha256  = '$sha256'"
