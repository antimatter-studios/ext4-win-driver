#!/usr/bin/env bash
#
# Install this repo's git hooks into the clone's .git/hooks.
# Run once per fresh clone, and again after a hook change lands on the default
# branch.
# Idempotent; safe to re-run.
#
# WHY .git/hooks, AND NOT `core.hooksPath=.githooks`
# ---------------------------------------------------
# Git resolves a hook path at the moment it RUNS the hook, which for a checkout
# is AFTER the working tree has been rewritten. Point core.hooksPath at a
# directory inside the tree and checking out an untrusted branch replaces the
# hook that runs next — their code, your credentials, your checkout. Reviewing a
# contribution locally is the normal case, not an exotic one.
#
# .git/hooks is per-clone, outside the working tree, and no ref can reach it.
#
# WHY THE SOURCE IS A REF AND NOT THE CHECKOUT
# --------------------------------------------
# Copying out of the working tree would just move the same hole: install while a
# contribution branch is checked out and that branch's hook is now PERSISTENT,
# surviving the switch back to a trusted branch. So the hooks are read from the
# remote-tracking default branch — content that has been through review — never
# from whatever happens to be checked out.
#
# Editing the hooks themselves is the one case that needs the checkout:
#
#     INSTALL_HOOKS_FROM_WORKTREE=1 install-hooks
#
# Only do that on a branch you trust. It is deliberately not the default.
#
# The trade: hooks no longer follow a branch switch, and a hook change reaches
# you after it lands on the default branch and you fetch. Re-run this then.

set -euo pipefail

# Not a git checkout (tarball install, CI artifact, no git): nothing to do, and
# never fail the surrounding install for it.
command -v git >/dev/null 2>&1 || exit 0
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0

cd "$(git rev-parse --show-toplevel)"

# --git-common-dir, so this works from a linked worktree — hooks are shared, not
# per-worktree. Deliberately NOT `rev-parse --git-path hooks`, which HONOURS
# core.hooksPath: while an older install's setting is still present that would
# hand back .githooks and we would write straight back into the working tree.
hooks="$(cd "$(git rev-parse --git-common-dir)" && pwd)/hooks"
manifest="$hooks/.install-hooks.manifest"

# --- refuse to trample a deliberate hooks path ------------------------------
# Clearing only OUR legacy value. Unsetting whatever we find would silently
# discard someone's intentional setup, and this script can run unattended.
current=$(git config --get core.hooksPath || true)
case "${current:-}" in
  "" | ".githooks" | "$PWD/.githooks") ;;
  *)
    echo "install-hooks: core.hooksPath is set to '$current', which overrides .git/hooks." >&2
    echo "                   Leaving it alone and installing nothing, rather than" >&2
    echo "                   discarding a setup this script did not create. Origin:" >&2
    git config --show-origin --get core.hooksPath >&2 || true
    exit 0
    ;;
esac

# --- where the hooks come from ----------------------------------------------
if [ "${INSTALL_HOOKS_FROM_WORKTREE:-}" = "1" ]; then
  ref=""
  [ -d ".githooks" ] || { echo "install-hooks: no .githooks/ in the working tree." >&2; exit 1; }
  files=$(cd ".githooks" && find . -type f | sed 's|^\./||' | sort)
  echo "install-hooks: installing from the WORKING TREE (INSTALL_HOOKS_FROM_WORKTREE=1)."
else
  ref=""
  for candidate in origin/HEAD origin/main origin/master; do
    if git rev-parse --verify -q "$candidate" >/dev/null; then ref=$candidate; break; fi
  done
  if [ -z "$ref" ]; then
    echo "install-hooks: no origin/HEAD, origin/main or origin/master to install from," >&2
    echo "                   so nothing was installed. Fetch, or re-run with" >&2
    echo "                   INSTALL_HOOKS_FROM_WORKTREE=1 if you trust this checkout." >&2
    exit 0
  fi
  files=$(git ls-tree -r --name-only "$ref" -- ".githooks" | sed "s|^.githooks/||" | sort)
  if [ -z "$files" ]; then
    echo "install-hooks: $ref has no .githooks/ — nothing to install." >&2
    exit 0
  fi
fi

mkdir -p "$hooks"

# --- warn before displacing a hook this script did not put there -------------
# A destination file that is not in our manifest belongs to the developer or
# another tool. Say so rather than overwriting it in silence.
prev=""
[ -f "$manifest" ] && prev=$(cat "$manifest")
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  dst="$hooks/$rel"
  [ -e "$dst" ] || [ -L "$dst" ] || continue
  printf '%s\n' "$prev" | grep -qxF "$rel" && continue
  echo "install-hooks: replacing an existing $rel that this script did not install." >&2
done <<< "$files"

# --- install -----------------------------------------------------------------
# rm first: a destination SYMLINK would otherwise be followed, and the copy (and
# the mode change) would land on whatever it points at — a personal or
# tool-managed hook, silently rewritten.
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  mkdir -p "$hooks/$(dirname "$rel")"
  rm -f "$hooks/$rel"
done <<< "$files"

if [ -n "$ref" ]; then
  # git archive carries the tree's own file modes, so the exec bits come from
  # the reviewed content rather than from whatever the checkout happens to have.
  git archive "$ref" ".githooks" | tar -x --strip-components=1 -C "$hooks"
else
  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    cp -p ".githooks/$rel" "$hooks/$rel"
  done <<< "$files"
fi

# --- prune hooks we installed that no longer exist ---------------------------
# An overlay copy alone leaves a deleted or renamed hook running forever. Only
# files from OUR last manifest are removed; nothing else in .git/hooks is
# touched.
if [ -n "$prev" ]; then
  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    printf '%s\n' "$files" | grep -qxF "$rel" && continue
    rm -f "$hooks/$rel"
    echo "install-hooks: removed $rel, which is no longer part of the hook set."
  done <<< "$prev"
fi
printf '%s\n' "$files" > "$manifest"

# --- clear the legacy override and ASSERT the end state ----------------------
# core.hooksPath OVERRIDES .git/hooks entirely, so leaving the old value in place
# makes this install INERT while every surface check says it worked.
git config --unset-all core.hooksPath 2>/dev/null || true
if [ "$(git config --get extensions.worktreeConfig 2>/dev/null || true)" = "true" ]; then
  git config --worktree --unset-all core.hooksPath 2>/dev/null || true
fi

if [ ! -x "$hooks/pre-commit" ]; then
  echo "install-hooks: $hooks/pre-commit is missing or not executable." >&2
  exit 1
fi
still=$(git config --get core.hooksPath || true)
if [ -n "$still" ]; then
  echo "install-hooks: core.hooksPath is still '$still' after clearing this repo's" >&2
  echo "                   config, so the hooks just installed would never run." >&2
  echo "                   It is set here — clear it in that scope and re-run:" >&2
  git config --show-origin --get core.hooksPath >&2 || true
  exit 1
fi

echo "Hooks installed into $hooks from ${ref:-the working tree} (outside the tree; no branch can rewrite them)."
echo "Bypass a single commit with: git commit --no-verify"
