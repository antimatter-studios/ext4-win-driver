#!/usr/bin/env bash
#
# Point git at the tracked hooks in .githooks/. Idempotent. Run once per
# fresh clone. `core.hooksPath` is a local-only config value, so it
# doesn't propagate automatically — hence this script + a README note.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

git config core.hooksPath .githooks
chmod +x .githooks/* 2>/dev/null || true

echo "Hooks installed. pre-commit will run cargo fmt --check + cargo clippy."
echo "Bypass a single commit with: git commit --no-verify"
