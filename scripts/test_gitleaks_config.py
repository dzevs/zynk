"""Gitleaks config: catches private strings EVERYWHERE (never masked), leaves legit content clean.

unittest style to match the repo's maintenance-test convention (run via `python3 -m unittest`).

The matched private-string fixtures are assembled at runtime (f-strings over single-token vars) so THIS test
file — and its compiled .pyc — do not themselves contain the literal patterns. That keeps the gate's content
coverage maximal: no allowlist exemption is needed for this file."""
import subprocess, tempfile, os, pathlib, shutil, unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]

_ZEUS, _FORK = "zeus", "zynk"
P_HOME = f"/home/{_ZEUS}"            # the private home path, assembled at runtime (no literal in source/.pyc)
P_FORKLINK = f"ogulcancelik/{_FORK}"  # the wrong-fork link, assembled at runtime


def _scan_named(name, content):
    """Return gitleaks exit code (1 = leak found, 0 = clean) for a file named `name` with `content`."""
    with tempfile.TemporaryDirectory() as d:
        shutil.copy(ROOT / ".gitleaks.toml", os.path.join(d, ".gitleaks.toml"))
        pathlib.Path(d, name).write_text(content)
        return subprocess.run(["gitleaks", "detect", "--no-git", "--config", ".gitleaks.toml",
                               "--source", "."], cwd=d, capture_output=True, text=True).returncode


@unittest.skipUnless(shutil.which("gitleaks"), "gitleaks not installed")
class GitleaksConfigTests(unittest.TestCase):
    def test_private_string_caught(self):
        self.assertEqual(_scan_named("f.txt", f"path {P_HOME}/secret"), 1)

    def test_private_string_in_NOTICE_still_caught(self):   # invariant: no path exemption masks the rule
        self.assertEqual(_scan_named("NOTICE", f"see {P_HOME}/x"), 1)

    def test_wrong_fork_link_caught(self):
        self.assertEqual(_scan_named("f.txt", f"https://github.com/{P_FORKLINK}"), 1)

    def test_upstream_herdr_attribution_clean(self):        # bare ogulcancelik (not /zynk) -> clean
        self.assertEqual(_scan_named("NOTICE", "Copyright ogulcancelik (herdr upstream)"), 0)

    def test_public_identity_clean(self):
        self.assertEqual(_scan_named("f.txt", "maintainer dzevs <hi@zevs.gg>"), 0)


if __name__ == "__main__":
    unittest.main()
