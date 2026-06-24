#!/usr/bin/env python3
"""Forbid product-specific reference terms in COPIED tooling/docs that gitleaks won't flag.

SCOPED to the paths we copy/adapt from the reference repos — NOT the whole tree, because
zynk's own code legitimately contains `pnpm`/`discord` (src/detect/mod.rs detects pnpm-installed agents;
src/integration/mod.rs has discord integration fixtures). gitleaks still covers the WHOLE tree for private
strings. Run with --staged in pre-commit, or no arg in CI (all tracked files). Exit 1 on any hit."""
import re
import subprocess
import sys

SCOPE_PREFIX = (".agents/", ".claude/", "docs/styleguides/", "docs/styles/")
SCOPE_FILE = ("AGENTS.md", "CLAUDE.md", "WORKFLOW.md")

# Product-specific reference terms. Case-insensitive so Mastra/Studio/Discord are caught too.
TERMS = [r"\bmastracode\b", r"\bmastra\b", r"\bstudio\b", r"\bcoderabbit\b", r"\bdiscord\b",
         r"\bpnpm\b", r"\bchangeset\b", r"\$?MASTRA_[A-Z0-9_]+"]
PAT = re.compile("|".join(TERMS), re.IGNORECASE)


def hits(text):
    return PAT.findall(text)


def in_scope(f):
    return f in SCOPE_FILE or any(f.startswith(p) for p in SCOPE_PREFIX)


def tracked_files(staged):
    if staged:
        args = ["git", "diff", "--cached", "--name-only", "--diff-filter=ACMR"]
    else:
        args = ["git", "ls-files"]
    out = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    return [f for f in out.splitlines() if f and in_scope(f)]


def main():
    files = tracked_files("--staged" in sys.argv)
    bad = []
    for f in files:
        try:
            text = open(f, encoding="utf-8", errors="ignore").read()
        except (IsADirectoryError, FileNotFoundError):
            continue
        if hits(text):
            bad.append(f)
    if bad:
        print("scrub gate: FAILED — product-specific reference terms in copied tooling/docs:", file=sys.stderr)
        for f in bad:
            print(f"  {f}", file=sys.stderr)
        sys.exit(1)
    print(f"scrub gate: clean ({len(files)} scoped files checked)")


if __name__ == "__main__":
    main()
