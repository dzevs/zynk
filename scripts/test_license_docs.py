"""Licensing/provenance accuracy guard for the release-facing docs.

zynk is AGPL-3.0-or-later; upstream is not. Upstream code through upstream tag v0.7.1 arrived under
AGPL-3.0-or-later, and upstream code from upstream commit `cd5ea1be` onward under Apache-2.0, redistributed
inside the AGPL combined work as Apache-2.0 section 4 permits. `NOTICE` is the authoritative record. This
guard pins that story mechanically so the pre-relicense claim that zynk carries the same license as the
upstream project (Gate-3 B1: WARDEN-R13-PROVENANCE-001) cannot return to CONTRIBUTING/README/NOTICE.

unittest style (run via `python3 -m unittest`)."""
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]

RELICENSE_COMMIT = "cd5ea1be"
APACHE_TEXT_FILE = "LICENSE-APACHE-2.0.upstream"
# The retired pre-relicense sentence, lowercased for a case-insensitive search.
FALSE_CLAIM = "same license as upstream"
# Release-facing docs only: the append-only ledger quotes the retired sentence on purpose.
GUARDED = ("CONTRIBUTING.md", "README.md", "NOTICE")


def _read(rel):
    return (ROOT / rel).read_text()


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


if __name__ == "__main__":
    unittest.main()
