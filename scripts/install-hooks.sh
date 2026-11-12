#!/usr/bin/env bash
# Install the repo's git hooks (P0-008). Run once per checkout:
#   ./scripts/install-hooks.sh
# Points core.hooksPath at the checked-in .githooks/ directory so the
# pre-commit secret scan runs for every commit.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

git config core.hooksPath .githooks
echo "hooks installed: core.hooksPath=.githooks"
echo "pre-commit runs: gitleaks protect --staged"
