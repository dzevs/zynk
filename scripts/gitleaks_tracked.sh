#!/usr/bin/env bash
# Content gate (gitleaks) over the git-tracked WORKING TREE — a clean copy of every tracked file with
# its CURRENT working-tree content (including uncommitted edits), NOT just the committed HEAD.
#
# WHY copy-tracked-files instead of `--source .`: `gitleaks detect --no-git --source .` scans the whole
# working directory, including git-ignored/untracked local state (`.git/`, `target/`, private dev files),
# which false-fails the gate on a normal checkout even when every tracked public path is clean. Copying
# exactly the tracked set (`git ls-files`) with their live content scans the public surface only, and a leak
# added to a tracked file is caught even before it is committed. Pre-commit also runs `gitleaks protect
# --staged` for staged hunks. Used by both `just gate` and the CI content-gate step so the two stay identical.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Materialize the current working-tree content of every tracked file (-z handles spaces/newlines).
git ls-files -z | tar --null --files-from=- --create --file - | tar --extract --directory "$tmp"
# The ROOT config legitimately contains every forbidden string as its own rule regex, so drop just that one
# file from the scanned copy (gitleaks reports absolute paths under --source, which a path allowlist can't
# anchor reliably). A NESTED `.gitleaks.toml` is kept and scanned — only the root config is exempt.
rm -f "$tmp/.gitleaks.toml"
gitleaks detect --no-git --config .gitleaks.toml --source "$tmp" --redact
