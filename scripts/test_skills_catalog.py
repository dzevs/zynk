"""Catalog-completeness guard: every committed dev skill is listed in its owner doc.

`.agents/skills/<name>` (co-authors') must be listed in AGENTS.md; `.claude/skills/<name>` (Claude's) in
CLAUDE.md. One-direction substring check (a stricter exact-list parser could be a follow-up). The root
`/SKILL.md` is the installed zynk-control skill — it must NOT be duplicated as a repo dev skill.

unittest style (run via `python3 -m unittest`)."""
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


def _skill_dirs(rel):
    d = ROOT / rel
    return sorted(p.name for p in d.glob("*") if p.is_dir()) if d.exists() else []


class CatalogTests(unittest.TestCase):
    def test_agents_skills_listed_in_agents_md(self):
        doc = (ROOT / "AGENTS.md").read_text()
        missing = [s for s in _skill_dirs(".agents/skills") if s not in doc]
        self.assertEqual(missing, [], f"uncataloged .agents/skills in AGENTS.md: {missing}")

    def test_claude_skills_listed_in_claude_md(self):
        doc = (ROOT / "CLAUDE.md").read_text()
        missing = [s for s in _skill_dirs(".claude/skills") if s not in doc]
        self.assertEqual(missing, [], f"uncataloged .claude/skills in CLAUDE.md: {missing}")

    def test_root_skill_is_not_a_dev_skill(self):
        # root /SKILL.md = the installed zynk-control skill; never a stale duplicate dev skill.
        self.assertFalse((ROOT / ".claude/skills/zynk").exists())
        self.assertFalse((ROOT / ".agents/skills/zynk").exists())
        self.assertTrue((ROOT / "SKILL.md").is_file())


if __name__ == "__main__":
    unittest.main()
