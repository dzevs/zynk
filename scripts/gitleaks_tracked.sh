#!/usr/bin/env bash
# Content gate (gitleaks) over ONLY the git-tracked tree, via a clean `git archive` export.
#
# WHY: `gitleaks detect --no-git --source .` scans the whole working directory, including
# git-ignored/untracked local state (`.git/`, `target/`, private dev files), which false-fails
# the gate on a normal checkout even when every tracked public path is clean. Tracked private
# paths are caught structurally by scripts/check_public_tree.py; this scans the same tracked set
# for secret/string content. Pre-commit uses `gitleaks protect --staged` for staged changes.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

git archive --format=tar HEAD | tar -x -C "$tmp"
gitleaks detect --no-git --config .gitleaks.toml --source "$tmp" --redact
