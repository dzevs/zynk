#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path

ALLOWED_TYPES = {
    "feat",
    "fix",
    "perf",
    "docs",
    "ci",
    "test",
    "refactor",
    "chore",
    "release",
}
SUBJECT_RE = re.compile(r"^(?P<kind>[a-z]+)(?:\([^)]+\))?!?:\s+\S")


_ALL_ZEROS = re.compile(r"^0+$")


def _rev_exists(rev: str) -> bool:
    """True when `rev` resolves to a commit object in the current repo."""
    return (
        subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}"],
            capture_output=True,
        ).returncode
        == 0
    )


def git_subjects(rev_range: str) -> list[str]:
    # GitHub sends `before..after` on push. Validate the commits reachable from <after>, but
    # handle the cases where <before> cannot anchor the range:
    #   - all-zero <after>: a ref deletion -> nothing to validate.
    #   - all-zero <before>: a new ref / first push (the fresh public repo) -> validate <after>.
    #   - missing/unreachable <before>: a force-push to an unrelated history (e.g. replacing an
    #     orphan) leaves the old SHA out of the checkout -> validate <after> instead of crashing.
    if ".." in rev_range:
        before, _, after = rev_range.partition("..")
        before, after = before.strip(), after.strip()
        if _ALL_ZEROS.fullmatch(after):
            return []
        if _ALL_ZEROS.fullmatch(before) or not _rev_exists(before):
            rev_range = after
    output = subprocess.check_output(
        ["git", "log", "--pretty=format:%s", rev_range], text=True
    ).strip()
    return [line.strip() for line in output.splitlines() if line.strip()]


def valid_subject(subject: str) -> bool:
    match = SUBJECT_RE.match(subject)
    return bool(match and match.group("kind") in ALLOWED_TYPES)


def commit_message_subject(path: Path) -> str | None:
    for line in path.read_text(encoding="utf-8").splitlines():
        subject = line.strip()
        if subject and not subject.startswith("#"):
            return subject
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate conventional commit subjects")
    parser.add_argument("subjects", nargs="*")
    parser.add_argument("--range", dest="rev_range")
    parser.add_argument("--message-file")
    args = parser.parse_args()

    subjects = list(args.subjects)
    if args.rev_range:
        subjects.extend(git_subjects(args.rev_range))
    if args.message_file:
        subject = commit_message_subject(Path(args.message_file))
        if subject:
            subjects.append(subject)

    invalid = [subject for subject in subjects if not valid_subject(subject)]
    if invalid:
        print("invalid commit subject(s):")
        for subject in invalid:
            print(f"  {subject}")
        print(
            "commit subjects must use conventional commits because preview notes are generated from them."
        )
        print("example: fix(update): install selected channel")
        print("expected: type(optional-scope): subject")
        print("allowed types: " + ", ".join(sorted(ALLOWED_TYPES)))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
