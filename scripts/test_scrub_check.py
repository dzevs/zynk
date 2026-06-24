"""Tests for the scoped scrub gate (scripts/scrub_check.py).

Scoped to copied tooling/docs paths; case-insensitive; the CLI integration tests use a real temp git repo
so the path scoping cannot silently regress (Codex R1). unittest style (run via `python3 -m unittest`)."""
import importlib.util, pathlib, subprocess, tempfile, unittest

_s = importlib.util.spec_from_file_location(
    "scrub_check", pathlib.Path(__file__).parent / "scrub_check.py")
m = importlib.util.module_from_spec(_s); _s.loader.exec_module(m)
SCRUB = str(pathlib.Path(__file__).parent / "scrub_check.py")


class ScrubHitsTests(unittest.TestCase):
    def test_flags_product_terms_case_insensitive(self):
        for s in ["create-mastra and Mastracode Studio with pnpm", "see $MASTRA_DB_URL and the changeset",
                  "Mastra", "Discord", "discord.com/channels/123", "MASTRA_DISCORD_BOT_TOKEN", "CodeRabbit"]:
            self.assertTrue(m.hits(s), s)

    def test_clean_zynk_text_passes(self):
        self.assertFalse(m.hits("build zynk with cargo + just check; ratatui TUI"))

    def test_scope(self):
        self.assertTrue(m.in_scope(".agents/skills/x/SKILL.md"))
        self.assertTrue(m.in_scope("AGENTS.md"))
        self.assertFalse(m.in_scope("src/detect/mod.rs"))            # zynk code w/ legit pnpm — out of scope
        self.assertFalse(m.in_scope("tests/fixtures/update/latest.json"))


class ScrubCliTests(unittest.TestCase):  # real-git integration so path scoping can't silently regress
    def _run(self, files):
        d = tempfile.mkdtemp()
        subprocess.run(["git", "init", "-q", d], check=True)
        for path, text in files.items():
            p = pathlib.Path(d) / path
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        subprocess.run(["git", "-C", d, "add", "-A"], check=True)
        return subprocess.run(["python3", SCRUB, "--staged"], cwd=d).returncode

    def test_in_scope_term_fails(self):
        self.assertEqual(self._run({".agents/skills/x/SKILL.md": "adapted from Mastra\n"}), 1)

    def test_out_of_scope_legit_pnpm_passes(self):
        self.assertEqual(self._run({"src/detect/mod.rs": "// detects pnpm-installed opencode\n"}), 0)


if __name__ == "__main__":
    unittest.main()
