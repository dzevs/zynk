"""Keep the operator-approved upstream-port policy synchronized across guidance."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
GUIDANCE = ("AGENTS.md", "CLAUDE.md", "WORKFLOW.md")
CLAUSES = (
    "FORK-OWNED user-visible behavior wins by default in any upstream port. This includes UI, glyphs, "
    "animation, layout, interaction, config effect, integration behavior, and CLI/agent surfaces.",
    "Every user-visible removal, replacement, or behavior change, whether fork-owned or upstream-origin, "
    "requires an explicit operator decision at Gate-1.",
    "A ledger entry alone is never sufficient evidence or approval for a user-visible change.",
)
AUDIT_HEADING = "#### operator-accepted 2026-09-25, post-release audit"
AUDIT_MARKERS = (
    "M5-22 theme highlight colors",
    "linked-worktree `├─`/`└─` connectors",
    "centered tab labels",
    "mobile-header roll-up",
    "navigator selection style",
    "Navigator Escape behavior",
    "copy-mode word motions and prefix handling",
    "PageUp/PageDown pager",
    "focus returning to the previous pane on close",
    "the Ctrl+/ byte",
    "keybinding-conflict precedence",
    "lone-Escape 150 ms hold",
    "remote-only clipboard-image bridge",
    "local Ctrl+V image paste into Claude Code",
    "Settings Experiments tab",
    "shortened config banner",
    "fail-closed database open",
    "1 MiB fragmented-paste cap",
    "SSH ControlMaster",
    "remote attach limited to Linux x86_64",
    "deferred new-tab rejection",
    "removed Pi debounce environment vars",
    "ADR 0015 agent-start form",
    "retirement of `custom_status`",
    "sibling panes marked seen on API focus",
    "upstream hook-release changes",
)
RESTORATION_MARKERS = (
    "FORK_DEVIATION: effective agent-panel scope, header, and spacing",
    "FORK_DEVIATION: working animation",
    "M5-21 (`0b659af`, upstream `81f355fa`)",
    "M2 `2b0838f` / upstream `4421c0f`",
    "`005b5ce` / upstream `350f0013`",
    "M5-01 `f79434b` / upstream `1a4e94e5`",
    "Restore Navigate Tab and Shift-Tab",
    "Restore `agent wait --status S`",
    "checked protocol mismatch errors",
    "Pin the root `SKILL.md` protocol sentence",
    "Port-process correction",
)


class PortPolicyDocsTest(unittest.TestCase):
    def test_every_guidance_file_carries_the_three_clause_port_policy(self) -> None:
        for relative in GUIDANCE:
            text = " ".join((ROOT / relative).read_text().split())
            with self.subTest(path=relative):
                for clause in CLAUSES:
                    normalized_clause = " ".join(clause.split())
                    self.assertEqual(text.count(normalized_clause), 1)

    def test_ledger_retains_the_dated_audit_and_restoration_provenance(self) -> None:
        ledger = (ROOT / "docs/zynk/fork-patch-ledger.md").read_text()
        self.assertEqual(ledger.count(AUDIT_HEADING), 1)

        audit = " ".join(ledger.split(AUDIT_HEADING, 1)[1].split())
        for marker in AUDIT_MARKERS:
            with self.subTest(audit_marker=marker):
                self.assertEqual(audit.count(marker), 1)

        normalized_ledger = " ".join(ledger.split())
        for marker in RESTORATION_MARKERS:
            with self.subTest(restoration_marker=marker):
                self.assertIn(marker, normalized_ledger)


if __name__ == "__main__":
    unittest.main()
