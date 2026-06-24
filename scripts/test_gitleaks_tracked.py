"""Regression tests for the tracked-tree content gate (scripts/gitleaks_tracked.sh).

Two Gate-2 adversarial findings these pin down:
- (1) a private string added to a TRACKED file is caught even before commit — the helper scans the
  working-tree content, not just HEAD.
- (2) only the ROOT `.gitleaks.toml` is allowlisted; a nested `.gitleaks.toml` is still scanned.

These run the real `gitleaks` binary inside a throwaway git repo, so they skip if it is not installed. The
leak fixture is assembled from fragments so THIS test file does not itself trip the tracked-tree gate.
unittest style (`python3 -m unittest`)."""
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "gitleaks_tracked.sh"
CONFIG = ROOT / ".gitleaks.toml"
LEAK = "/home" + "/zeus/secret"  # split so this file stays gate-clean; matches zynk-private-strings


@unittest.skipUnless(shutil.which("gitleaks"), "gitleaks not installed")
class GitleaksTrackedTests(unittest.TestCase):
    def _repo(self, files):
        d = pathlib.Path(tempfile.mkdtemp())
        subprocess.run(["git", "init", "-q", str(d)], check=True)
        subprocess.run(["git", "-C", str(d), "config", "user.email", "t@example.com"], check=True)
        subprocess.run(["git", "-C", str(d), "config", "user.name", "t"], check=True)
        (d / "scripts").mkdir()
        shutil.copy(HELPER, d / "scripts" / "gitleaks_tracked.sh")
        shutil.copy(CONFIG, d / ".gitleaks.toml")
        for rel, text in files.items():
            p = d / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        subprocess.run(["git", "-C", str(d), "add", "-A"], check=True)
        subprocess.run(["git", "-C", str(d), "commit", "-qm", "init"], check=True)
        return d

    def _run(self, d):
        return subprocess.run(["bash", "scripts/gitleaks_tracked.sh"], cwd=d).returncode

    def test_clean_tracked_tree_passes(self):
        d = self._repo({"README.md": "clean public readme for zynk\n"})
        self.assertEqual(self._run(d), 0)

    def test_uncommitted_leak_in_tracked_file_fails(self):
        d = self._repo({"README.md": "clean\n"})
        (d / "README.md").write_text(f"clean\nleaked {LEAK} here\n")  # modify, do NOT commit
        self.assertNotEqual(self._run(d), 0, "uncommitted leak in a tracked file must fail the gate")

    def test_nested_gitleaks_toml_not_exempted(self):
        d = self._repo({"sub/.gitleaks.toml": f"# nested config with {LEAK}\n"})
        self.assertNotEqual(self._run(d), 0, "only the root .gitleaks.toml is allowlisted (tracked-tree scan)")

    def test_staged_nested_gitleaks_toml_not_exempted(self):
        # pre-commit path: `gitleaks protect --staged` must scan a staged nested .gitleaks.toml (relative
        # paths, so the root-anchored allowlist exempts only the root config).
        d = self._repo({"README.md": "clean\n"})
        nested = d / "sub" / ".gitleaks.toml"
        nested.parent.mkdir()
        nested.write_text(f"# nested config with {LEAK}\n")
        subprocess.run(["git", "-C", str(d), "add", "-A"], check=True)
        rc = subprocess.run(["gitleaks", "protect", "--staged", "--config", ".gitleaks.toml"], cwd=d).returncode
        self.assertNotEqual(rc, 0, "staged nested .gitleaks.toml must not be exempted by protect --staged")


if __name__ == "__main__":
    unittest.main()
