"""Guard: the pre-release-audit skill/prompt must not drift back to the retired private release flow.

The single-repo public model (since 2026-06-24) has no `docs/next`, no `website/` docs mirror, and no
`just release-docs-check` / `just release` recipes; the canonical branch is `main`. Gate-2/Gate-3 caught the
committed skill still describing all of those, so this pins the rewrite. unittest style (`python3 -m unittest`)."""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]

FILES = [
    ".agents/skills/zynk-pre-release-audit/SKILL.md",
    ".agents/skills/zynk-pre-release-audit/references/pre-release-audit.md",
    ".pi/prompts/pre-release-audit.md",
]

# (pattern, why) — retired private-flow references that must never reappear in the audit skill.
FORBIDDEN = [
    (r"docs/next", "retired private next-release dir"),
    (r"website/src", "retired website docs mirror"),
    (r"release-docs-check", "non-existent just recipe"),
    (r"\bjust release\b", "non-existent just recipe (use just check / just gate)"),
    (r"\bmaster\b", "wrong branch — canonical branch is main"),
]


class ReleaseAuditRefsTests(unittest.TestCase):
    def test_no_retired_flow_references(self):
        bad = []
        for rel in FILES:
            text = (ROOT / rel).read_text()
            for pat, why in FORBIDDEN:
                m = re.search(pat, text, re.IGNORECASE)
                if m:
                    bad.append(f"{rel}: {m.group(0)!r} ({why})")
        self.assertEqual(bad, [], f"retired release-flow references in pre-release-audit: {bad}")

    def test_anchors_present(self):
        # The rewrite must point at the real public anchors so it stays runnable.
        ref = (ROOT / ".agents/skills/zynk-pre-release-audit/references/pre-release-audit.md").read_text()
        for anchor in ["CHANGELOG.md", "README.md", "Cargo.toml", "cargoHash", "main"]:
            self.assertIn(anchor, ref, f"pre-release-audit reference missing anchor: {anchor}")


if __name__ == "__main__":
    unittest.main()
