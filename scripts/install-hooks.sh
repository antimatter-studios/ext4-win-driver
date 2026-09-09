#!/usr/bin/env bash
#
# Install the tracked hooks from .githooks/ into this clone's .git/hooks.
# Idempotent. Run once per fresh clone, and again after editing anything
# in .githooks/.
#
# WHY .git/hooks, AND NOT `core.hooksPath=.githooks`
# ---------------------------------------------------
# This script used to point core.hooksPath at the tracked .githooks/ directory.
# Git resolves a hook path at the moment it RUNS the hook, which for a checkout
# is AFTER the working tree has been rewritten — so with the hooks inside the
# tree, checking out an untrusted branch replaces the hook that runs next:
#
#     git checkout some-forks-pr     # their .githooks/pre-commit lands in the tree
#     git commit                     # ...and runs, as you, with your credentials
#
# Reviewing a contribution locally is the normal case, not an exotic one.
# .git/hooks is per-clone, sits outside the working tree, and no ref can reach
# it, so what git executes stays whatever was last installed here.
#
# The trade: hooks no longer follow a branch switch. Re-running this script is
# how you pick up a change to .githooks/ — which is also the only moment the
# repository gets to decide what runs on your machine.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# --git-common-dir, so this works from a linked worktree (hooks are shared, not
# per-worktree). Deliberately NOT `rev-parse --git-path hooks`, which HONOURS
# core.hooksPath: while an older install's setting is still present, that would
# hand back .githooks and we would copy the hooks straight back into the tree.
hooks="$(cd "$(git rev-parse --git-common-dir)" && pwd)/hooks"
mkdir -p "$hooks"
cp -R .githooks/. "$hooks/"

# Restore exec bits from the SOURCE tree, which is the authority on which files
# are meant to be executable — cp does not always carry them across.
( cd .githooks && find . -type f -perm -u+x -print0 ) \
  | while IFS= read -r -d '' rel; do
      chmod +x "$hooks/${rel#./}" 2>/dev/null || true
    done

# core.hooksPath OVERRIDES .git/hooks entirely, so an install that leaves an
# older value in place is INERT while every surface check says it worked.
git config --unset-all core.hooksPath 2>/dev/null || true

# Assert the END STATE, not the actions. Both halves fail independently.
if [ ! -x "$hooks/pre-commit" ]; then
  echo "install-hooks: $hooks/pre-commit is missing or not executable." >&2
  exit 1
fi
still=$(git config --get core.hooksPath || true)
if [ -n "$still" ]; then
  echo "install-hooks: core.hooksPath is still '$still'." >&2
  echo "               It comes from your global or system git config and overrides" >&2
  echo "               .git/hooks, so these hooks would be installed and never run:" >&2
  echo "                 git config --global --unset core.hooksPath" >&2
  exit 1
fi

echo "Hooks installed into $hooks (outside the working tree; no branch can rewrite them)."
echo "core.hooksPath: unset — it would override .git/hooks."
echo "Bypass a single commit with: git commit --no-verify"
