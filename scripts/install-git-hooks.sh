#!/usr/bin/env bash
# Points this clone's git hooks at the tracked .githooks/ directory, so the
# pre-commit gate in .githooks/pre-commit actually runs. .git/hooks/ itself is
# never versioned, so this is opt-in per clone — run it once after cloning:
#   bash scripts/install-git-hooks.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

git config core.hooksPath .githooks
echo "Installed: git hooks now run from .githooks/ (core.hooksPath set for this clone)."
