import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "conventional_commits.py"
ZEROS = "0" * 40


def _init_repo(d: str, subject: str) -> str:
    def run(*args):
        subprocess.run(["git", "-C", d, *args], check=True, capture_output=True)

    run("init", "-q", "-b", "main")
    run("config", "user.email", "t@example.com")
    run("config", "user.name", "Test")
    (Path(d) / "f.txt").write_text("x")
    run("add", "f.txt")
    run("commit", "-q", "-m", subject)
    return subprocess.check_output(
        ["git", "-C", d, "rev-parse", "HEAD"], text=True
    ).strip()


def _run_validator(repo: str, rev_range: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["python3", str(SCRIPT), "--range", rev_range],
        cwd=repo,
        capture_output=True,
        text=True,
    )


class ConventionalCommitsAllZeroTest(unittest.TestCase):
    def test_all_zero_before_validates_initial_commit(self):
        # First push to a fresh repo: GitHub sends an all-zero "before" SHA. The validator must
        # validate the initial commit instead of crashing on `git log <zeros>..<after>`.
        with tempfile.TemporaryDirectory() as d:
            head = _init_repo(d, "chore: initial public release")
            res = _run_validator(d, f"{ZEROS}..{head}")
            self.assertEqual(res.returncode, 0, res.stdout + res.stderr)

    def test_all_zero_before_rejects_non_conventional_initial_commit(self):
        with tempfile.TemporaryDirectory() as d:
            head = _init_repo(d, "initial commit")  # not a conventional subject
            res = _run_validator(d, f"{ZEROS}..{head}")
            self.assertEqual(res.returncode, 1, res.stdout + res.stderr)

    def test_all_zero_after_is_noop(self):
        # A ref deletion (<x>..<zeros>) has nothing to validate and must not crash.
        with tempfile.TemporaryDirectory() as d:
            head = _init_repo(d, "chore: initial public release")
            res = _run_validator(d, f"{head}..{ZEROS}")
            self.assertEqual(res.returncode, 0, res.stdout + res.stderr)


if __name__ == "__main__":
    unittest.main()
