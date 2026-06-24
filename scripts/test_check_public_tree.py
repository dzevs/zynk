"""Tests for the tracked-path structural gate (scripts/check_public_tree.py).

unittest style to match the repo's maintenance-test convention (run via `python3 -m unittest`)."""
import importlib.util, pathlib, subprocess, tempfile, os, unittest

_spec = importlib.util.spec_from_file_location(
    "check_public_tree", pathlib.Path(__file__).parent / "check_public_tree.py")
m = importlib.util.module_from_spec(_spec); _spec.loader.exec_module(m)
SCRIPT = str(pathlib.Path(__file__).parent / "check_public_tree.py")


class ViolationsLogicTests(unittest.TestCase):
    def test_clean_tree_has_no_violations(self):
        self.assertEqual(m.violations(["src/main.rs", "README.md", "docs/zynk/SPEC.md",
                                        "docs/zynk/decisions/0001-x.md"]), [])

    def test_forbidden_dir_is_caught(self):
        self.assertEqual(m.violations(["src/main.rs", "website/index.html"]),
                         [("website/index.html", "website")])

    def test_forbidden_root_file_is_caught(self):
        self.assertEqual(m.violations(["CLAUDE.local.md"]),
                         [("CLAUDE.local.md", "CLAUDE.local.md")])

    def test_internal_docs_zynk_caught_design_law_ok(self):
        bad = m.violations(["docs/zynk/SPEC.md", "docs/zynk/decisions/0001.md",
                            "docs/zynk/plans/x.md", "docs/zynk/release-3.0.0-prep.md"])
        self.assertIn(("docs/zynk/plans/x.md", "docs/zynk/plans"), bad)
        self.assertIn(("docs/zynk/release-3.0.0-prep.md", "docs/zynk/release-3.0.0-prep.md"), bad)
        self.assertFalse(any(f.startswith("docs/zynk/SPEC") or "decisions" in f for f, _ in bad))

    def test_tracked_pyc_in_pycache_caught(self):  # Codex: closes the force-add __pycache__ two-gate bypass
        self.assertEqual([f for f, _ in m.violations(["pkg/__pycache__/leak.pyc"])],
                         ["pkg/__pycache__/leak.pyc"])

    def test_tracked_bare_pyc_caught(self):
        self.assertEqual([f for f, _ in m.violations(["pkg/foo.pyc"])], ["pkg/foo.pyc"])

    def test_tracked_pycache_component_caught_even_non_pyc(self):
        self.assertTrue(m.violations(["a/__pycache__/x.bin"]))

    def test_dotted_names_not_over_matched(self):  # a .py/.pyx is fine — only .pyc/.pyo/__pycache__ forbidden
        self.assertEqual(m.violations(["scripts/check_public_tree.py", "src/a.pyx"]), [])


class StagedGateIntegrationTests(unittest.TestCase):
    def _run(self, setup):
        with tempfile.TemporaryDirectory() as d:
            env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
                   "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
            subprocess.run(["git", "init", "-q"], cwd=d, check=True)
            setup(d, env)
            return subprocess.run(["python3", SCRIPT, "--staged"], cwd=d,
                                  capture_output=True, text=True)

    def test_planted_forbidden_path_fails_exit1(self):
        def setup(d, env):
            os.makedirs(os.path.join(d, ".codex"))
            pathlib.Path(d, ".codex", "skill.md").write_text("x")
            subprocess.run(["git", "add", ".codex/skill.md"], cwd=d, check=True)
        r = self._run(setup)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(".codex", r.stderr)

    def test_clean_repo_passes_exit0(self):
        def setup(d, env):
            pathlib.Path(d, "README.md").write_text("hi")
            subprocess.run(["git", "add", "README.md"], cwd=d, check=True)
        self.assertEqual(self._run(setup).returncode, 0)

    def test_staged_rename_into_forbidden_fails(self):
        # Codex blocker 1: a staged RENAME into a forbidden path must also fail (ACMR, not just AM).
        def setup(d, env):
            pathlib.Path(d, "README.md").write_text("hi")
            subprocess.run(["git", "add", "README.md"], cwd=d, check=True)
            subprocess.run(["git", "commit", "-qm", "x"], cwd=d, check=True, env=env)
            os.makedirs(os.path.join(d, ".codex"))
            subprocess.run(["git", "mv", "README.md", ".codex/skill.md"], cwd=d, check=True)
        r = self._run(setup)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(".codex", r.stderr)

    def test_force_added_pyc_in_pycache_fails(self):
        # Codex: a force-added __pycache__/*.pyc must FAIL the structural gate, so the gitleaks __pycache__
        # content-allowlist can never be exploited by a tracked bytecode artifact carrying a private string.
        def setup(d, env):
            os.makedirs(os.path.join(d, "pkg", "__pycache__"))
            pathlib.Path(d, "pkg", "__pycache__", "leak.pyc").write_text("/home/" + "zeus/secret")
            subprocess.run(["git", "add", "-f", "pkg/__pycache__/leak.pyc"], cwd=d, check=True)
        r = self._run(setup)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("pyc", (r.stdout + r.stderr).lower())


class ReorgPathPolicyTests(unittest.TestCase):
    """docs+skills reorg: WORKFLOW.md becomes committed; committed agent tooling is allowed; only
    settings.local.json is newly forbidden; all private state stays forbidden."""

    def test_workflow_md_now_allowed(self):
        self.assertEqual(m.violations(["WORKFLOW.md"]), [])

    def test_settings_local_forbidden(self):
        self.assertEqual([f for f, _ in m.violations([".claude/settings.local.json"])],
                         [".claude/settings.local.json"])

    def test_committed_agent_tooling_allowed(self):
        self.assertEqual(m.violations([
            ".claude/skills/x/SKILL.md", ".claude/commands/pr.md", ".claude/settings.json",
            ".agents/skills/y/SKILL.md", ".agents/agents/code-reviewer.md", ".agents/references/z.md",
            "docs/styleguides/STYLEGUIDE.md", "docs/styles/zynk/x.yml",
            ".pi/prompts/p.md", ".pi/extensions/x/index.ts", ".zed/settings.json"]), [])

    def test_private_state_still_forbidden(self):
        for p in [".codex/sessions/a.jsonl", ".pi/cache/refs.json", ".local/x",
                  "CLAUDE.local.md", "docs/superpowers/specs/x.md", "docs/next/x.md", "website/i.html",
                  "docs/zynk/plans/p.md"]:
            self.assertTrue(m.violations([p]), p)


if __name__ == "__main__":
    unittest.main()
