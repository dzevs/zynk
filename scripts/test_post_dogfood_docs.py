#!/usr/bin/env python3
"""Parity checks for public post-dogfood interaction contracts."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]


def normalized(path: str) -> str:
    return re.sub(r"\s+", " ", (ROOT / path).read_text(encoding="utf-8")).strip()


class PostDogfoodDocsTest(unittest.TestCase):
    def test_right_click_order_is_consistent_in_every_public_contract(self) -> None:
        documents = {
            "README.md": (
                "Wheel forwarding and left-button scrollbar actions over an unfocused pane focus it "
                "through the runtime-authoritative path before acting.",
                "Accepted modifier-right-click passthrough sends the Down event to the explicit pane "
                "first and focuses it afterward",
                "a rejected send keeps focus unchanged and opens the ordinary pane menu",
            ),
            "CHANGELOG.md": (
                "Wheel forwarding and left-button scrollbar actions over an unfocused pane focus it "
                "through runtime authority before acting.",
                "Accepted modifier-right-click passthrough instead delivers its Down bytes to the "
                "explicit pane first, then focuses it",
                "rejected delivery leaves focus unchanged and opens the ordinary pane menu",
            ),
            "docs/zynk/SPEC.md": (
                "Wheel events over pane content or its scrollbar and left-button scrollbar clicks "
                "first focus an unfocused pane through runtime authority",
                "right-click passthrough uses deliver-then-focus ordering",
                "it falls through to the ordinary pane menu",
            ),
            "docs/zynk/fork-patch-ledger.md": (
                "wheel/scrollbar focus before forwarding",
                "an accepted modifier-right-click Down reaches its explicit pane before runtime focus",
                "Rejected delivery retains the ordinary menu",
            ),
        }

        stale_grouped_claim = re.compile(
            r"Wheel, scrollbar, and modifier-right-click[^.]*focus[^.]*before acting",
            re.IGNORECASE,
        )
        for path, required_clauses in documents.items():
            with self.subTest(path=path):
                text = normalized(path)
                self.assertNotRegex(text, stale_grouped_claim)
                for clause in required_clauses:
                    self.assertIn(clause, text)


if __name__ == "__main__":
    unittest.main()
