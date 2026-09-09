"""Contained Git commands for maintenance-test fixtures, never for the real checkout."""

import os
from pathlib import Path
import subprocess


def git_env():
    # Git configuration can redirect repositories, indexes, hooks and signing.
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
    return env


def run_git(repo, *args):
    repo = Path(repo)
    assert (repo / ".git").exists(), f"fixture has no .git: {repo}"
    return subprocess.run(
        ["git", "-C", str(repo), *args], env=git_env(),
        check=True, capture_output=True, text=True,
    )


def init_repo(repo, *args):
    repo = Path(repo)
    repo.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "-C", str(repo), "init", "--quiet", *args],
        env=git_env(), check=True, capture_output=True,
    )
    assert (repo / ".git").is_dir(), f"fixture init left no .git: {repo}"
    common = run_git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
    config = str(Path(common.stdout.strip()) / "config")
    for key, value in [("user.email", "test@example.invalid"), ("user.name", "Test")]:
        run_git(repo, "config", "--file", config, key, value)
