#!/bin/bash
# Install the pre-commit drift gates.
#
# Uses core.hooksPath so the hook is version-controlled rather than living as an
# untracked copy in .git/hooks/ that every fresh clone silently lacks. That gap
# is why no guard ran on the primary dev checkout for an unknown period (96E-29).
#
#   bash scripts/install-hooks.sh
#
# To uninstall:  git config --unset core.hooksPath

set -e
cd "$(dirname "$0")/.."

mkdir -p .githooks
cp scripts/pre-commit.hook .githooks/pre-commit
chmod +x .githooks/pre-commit 2>/dev/null || true
git config core.hooksPath .githooks

echo "installed: core.hooksPath -> .githooks (pre-commit)"
echo "verify with: git config core.hooksPath"
