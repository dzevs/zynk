import subprocess
import tempfile
import unittest
from pathlib import Path
from scripts.git_test_support import git_env, init_repo, run_git

SCRIPT = Path(__file__).resolve().parent / "conventional_commits.py"
ZEROS = "0" * 40


def _init_repo(d: str, subject: str) -> str:
    def run(*args):
        return run_git(d, *args)

    init_repo(d, "-b", "main")
    (Path(d) / "f.txt").write_text("x")
    run("add", "f.txt")
    run("commit", "-q", "-m", subject)
    return run("rev-parse", "HEAD").stdout.strip()


def _run_validator(repo: str, rev_range: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["python3", str(SCRIPT), "--range", rev_range],
        cwd=repo,
        capture_output=True,
        text=True,
        env=git_env(),
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

    def test_unreachable_before_validates_after(self):
        # A force-push to an unrelated history (e.g. replacing an orphan) leaves the old `before`
        # SHA out of the checkout; the validator must validate `after` instead of crashing.
        with tempfile.TemporaryDirectory() as d:
            head = _init_repo(d, "chore: initial public release")
            missing = "deadbeef" * 5  # 40-hex, well-formed but not a real commit
            res = _run_validator(d, f"{missing}..{head}")
            self.assertEqual(res.returncode, 0, res.stdout + res.stderr)

    def test_unreachable_before_rejects_bad_after_subject(self):
        with tempfile.TemporaryDirectory() as d:
            head = _init_repo(d, "no type prefix here")
            missing = "deadbeef" * 5
            res = _run_validator(d, f"{missing}..{head}")
            self.assertEqual(res.returncode, 1, res.stdout + res.stderr)


if __name__ == "__main__":
    unittest.main()
