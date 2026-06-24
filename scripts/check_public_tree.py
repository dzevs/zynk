#!/usr/bin/env python3
"""Structural gate for the single public repo: fail if any private/forbidden path is TRACKED in git.

Pairs with .gitleaks.toml (content gate). `.gitignore` only stops accidental adds; this catches `git add -f`
or a gitignore gap. Run with --staged in pre-commit (checks staged adds, including rename/copy destinations),
or no arg in CI (checks all tracked files via `git ls-files`)."""
import subprocess
import sys

FORBIDDEN = [
    ".codex", ".pi", ".zed", ".local", "backlog",
    "website", "docs/next", "docs/superpowers", "public",
    "CLAUDE.local.md", ".claude/settings.local.json", "scripts/export-public.sh",
    "docs/zynk/plans", "docs/zynk/release-3.0.0-prep.md",
    "docs/zynk/cutover-readiness.md", "docs/zynk/dev-ux.md",
    "scripts/preview.py", "scripts/changelog.py",
    "scripts/test_preview.py", "scripts/test_changelog.py",
    ".github/workflows/release.yml", ".github/workflows/preview.yml",
    ".github/workflows/approve-contributor.yml",
    ".github/workflows/approve-merged-contributor.yml",
    ".github/workflows/pr-gate.yml", ".github/workflows/issue-gate.yml",
    ".github/workflows/label-next-release-issues.yml",
]


def tracked_files(staged):
    if staged:
        # ACMR (not just AM): also catch a staged RENAME/COPY into a forbidden path (e.g.
        # `git mv README.md .codex/skill.md`); exclude D so removing a forbidden path is allowed.
        args = ["git", "diff", "--cached", "--name-only", "--diff-filter=ACMR"]
    else:
        args = ["git", "ls-files"]
    out = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    return [line for line in out.splitlines() if line]


def violations(files):
    bad = []
    for f in files:
        # Forbid TRACKED Python bytecode/cache anywhere in the tree. This PAIRS with the `__pycache__`
        # allowlist in .gitleaks.toml: exempting those paths from the CONTENT scan is only safe because this
        # structural gate guarantees they can never be tracked in the first place — closing the force-add
        # (`git add -f pkg/__pycache__/leak.pyc`) bypass where a private string in bytecode would slip both gates.
        if f.endswith((".pyc", ".pyo")) or "__pycache__" in f.split("/"):
            bad.append((f, "python bytecode/cache (.pyc/__pycache__)"))
            continue
        for p in FORBIDDEN:
            if f == p or f.startswith(p + "/"):
                bad.append((f, p))
                break
    return bad


def main():
    files = tracked_files("--staged" in sys.argv)
    bad = violations(files)
    if bad:
        print("tracked-path gate: FAILED — forbidden private paths are tracked:", file=sys.stderr)
        for f, p in bad:
            print(f"  {f}  (matches {p})", file=sys.stderr)
        sys.exit(1)
    print(f"tracked-path gate: clean ({len(files)} files checked)")


if __name__ == "__main__":
    main()
