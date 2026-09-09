"""Licensing/provenance accuracy guard for the release-facing docs.

zynk is AGPL-3.0-or-later; upstream is not. Upstream code through upstream tag v0.7.1 arrived under
AGPL-3.0-or-later, and upstream code from upstream commit `cd5ea1be` onward under Apache-2.0, redistributed
inside the AGPL combined work as Apache-2.0 section 4 permits. `NOTICE` is the authoritative record. This
guard pins that story mechanically so the pre-relicense claim that zynk carries the same license as the
upstream project (Gate-3 B1: WARDEN-R13-PROVENANCE-001) cannot return to CONTRIBUTING/README/NOTICE.

It also enforces the section 4(b) per-file duty in both directions: every file the `NOTICE` index
lists carries the standard top-of-file notice, and no other tracked file carries it (WARDEN-R14-APACHE-4B-001).

unittest style (run via `python3 -m unittest`)."""
import pathlib
import subprocess
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]

RELICENSE_COMMIT = "cd5ea1be"
APACHE_TEXT_FILE = "LICENSE-APACHE-2.0.upstream"
# The retired pre-relicense sentence, lowercased for a case-insensitive search.
FALSE_CLAIM = "same license as upstream"
# Release-facing docs only: the append-only ledger quotes the retired sentence on purpose.
GUARDED = ("CONTRIBUTING.md", "README.md", "NOTICE")

LIST_BEGIN = "<!-- apache-modified-files: begin -->"
LIST_END = "<!-- apache-modified-files: end -->"
# The two lines of the standard Apache-2.0 4(b) per-file notice, without any comment prefix, so the
# same text is checked in Rust (`//`), TOML/Python (`#`) and any future syntax.
NOTICE_LINE_1 = "Modified by the zynk project: this file differs from the upstream version it was derived from."
NOTICE_LINE_2 = 'See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.'
# The notice must be the first thing in the file: allow only a shebang/encoding line ahead of it.
NOTICE_WITHIN_LINES = 5
# This guard necessarily spells the notice out; it is not a listed file.
MARKER_SOURCE_FILES = ("scripts/test_license_docs.py",)


def _read(rel):
    return (ROOT / rel).read_text()


def _listed_files():
    """The `NOTICE` *Modified files (Apache-2.0 provenance)* index, in file order."""
    notice = _read("NOTICE")
    body = notice.split(LIST_BEGIN, 1)[1].split(LIST_END, 1)[0]
    return [line[2:].strip() for line in body.splitlines() if line.startswith("- ")]


def _tracked_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout
    return [rel for rel in out.split("\0") if rel]


def _text_or_none(rel):
    """File text, or None when the file is binary/unreadable (nothing to scan for a text marker)."""
    try:
        return (ROOT / rel).read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return None


class LicenseDocTests(unittest.TestCase):
    def test_no_same_license_as_upstream_claim(self):
        offenders = [rel for rel in GUARDED if FALSE_CLAIM in _read(rel).lower()]
        self.assertEqual(offenders, [], f"pre-relicense claim {FALSE_CLAIM!r} is back in: {offenders}")

    def test_notice_records_the_upstream_relicense(self):
        notice = _read("NOTICE")
        for needle in ("AGPL-3.0-or-later", "Apache", RELICENSE_COMMIT, APACHE_TEXT_FILE):
            self.assertIn(needle, notice, f"NOTICE no longer records {needle!r}")

    def test_contributing_and_readme_name_the_apache_provenance(self):
        for rel in ("CONTRIBUTING.md", "README.md"):
            doc = _read(rel)
            self.assertIn(RELICENSE_COMMIT, doc, f"{rel} does not name the upstream relicense commit")
            self.assertIn("Apache", doc, f"{rel} does not name upstream's Apache-2.0 provenance")

    def test_apache_text_is_shipped_and_packaged(self):
        self.assertTrue((ROOT / APACHE_TEXT_FILE).is_file(), f"{APACHE_TEXT_FILE} is missing")
        self.assertIn(f'"/{APACHE_TEXT_FILE}"', _read("Cargo.toml"), "Cargo `include` drops the Apache text")


class ApacheModifiedFileNoticeTests(unittest.TestCase):
    """Apache-2.0 section 4(b): the listed files themselves must say they were changed."""

    def test_listed_files_exist_are_tracked_and_carry_the_notice(self):
        listed = _listed_files()
        self.assertTrue(listed, f"the {LIST_BEGIN!r} index in NOTICE is empty")
        tracked = set(_tracked_files())
        missing, untracked, unmarked = [], [], []
        for rel in listed:
            if not (ROOT / rel).is_file():
                missing.append(rel)
                continue
            if rel not in tracked:
                untracked.append(rel)
                continue
            head = "\n".join(_read(rel).splitlines()[:NOTICE_WITHIN_LINES])
            if NOTICE_LINE_1 not in head or NOTICE_LINE_2 not in head:
                unmarked.append(rel)
        self.assertEqual(missing, [], f"NOTICE lists files that do not exist: {missing}")
        self.assertEqual(untracked, [], f"NOTICE lists untracked files: {untracked}")
        self.assertEqual(
            unmarked,
            [],
            "these NOTICE-listed files lack the Apache-2.0 4(b) notice in their first "
            f"{NOTICE_WITHIN_LINES} lines: {unmarked}",
        )

    def test_no_unlisted_tracked_file_carries_the_notice(self):
        exempt = set(_listed_files()) | set(MARKER_SOURCE_FILES)
        offenders = [
            rel
            for rel in _tracked_files()
            if rel not in exempt and NOTICE_LINE_1 in (_text_or_none(rel) or "")
        ]
        self.assertEqual(
            offenders,
            [],
            "these files carry the Apache-2.0 4(b) notice but are not in the NOTICE index "
            f"(add them there, or drop the stale notice): {offenders}",
        )


if __name__ == "__main__":
    unittest.main()
