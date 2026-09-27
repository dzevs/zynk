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

        stale_grouped_claims = (
            "Wheel, scrollbar, and modifier-right-click passthrough over an unfocused pane "
            "first focus it through the runtime-authoritative path.",
            "Wheel, scrollbar, and modifier-right-click passthrough over an unfocused pane "
            "now focus it through runtime authority before acting.",
        )
        for path, required_clauses in documents.items():
            with self.subTest(path=path):
                text = normalized(path)
                for stale in stale_grouped_claims:
                    self.assertNotIn(stale, text)
                for clause in required_clauses:
                    self.assertEqual(
                        text.count(clause),
                        1,
                        f"{path} must contain exactly one canonical clause: {clause}",
                    )

    def test_native_f4_identity_verification_shape_is_documented_exactly(self) -> None:
        required = {
            "README.md": (
                "Native F4 JSON responses add an optional `identity_verification` field",
                "top level of `zynk whoami --json` and implicit `zynk inbox --json`",
                "under `from` for native `zynk send` and `zynk reply`",
                "`verified` or `unverified` and is omitted when no Codex hint is present",
                "never grants routing or receipt authority",
            ),
            "SKILL.md": (
                "`zynk whoami --json` may include `identity_verification: \"verified\" | \"unverified\"`",
                "omitted when no Codex hint is present",
                "grants no authority",
            ),
            "CHANGELOG.md": (
                "protocol 20 and the socket API method and field shapes remain unchanged",
                "Native F4 JSON responses add an optional `identity_verification` field",
                "top level of `whoami` and implicit `inbox`, and under `from` for native `send` and `reply`",
                "`verified` or `unverified` and is omitted without a Codex hint",
            ),
            "docs/zynk/SPEC.md": (
                "final composed frame is the authority for committed local animation demand",
                "a full-frame layer replaces every animation contribution beneath it",
                "Native F4 JSON responses add optional Codex-hint verification metadata",
                "top level of `zynk whoami --json` and implicit `zynk inbox --json`",
                "under `from` for native `zynk send` and `zynk reply`",
            ),
            "docs/zynk/decisions/0014-receipt-principals-same-uid-pane-tree.md": (
                "supersedes the broad request/response-shape sentence above for native F4 CLI JSON",
                "socket API remains on protocol 20 with unchanged method and field shapes",
                "`identity_verification` is omitted without a Codex hint",
            ),
            "docs/zynk/fork-patch-ledger.md": (
                "G3-DOC-IDENTITY-JSON-001",
                "supersedes the earlier broad native-JSON shape claim",
                "`identity_verification` on `whoami`, implicit `inbox`, and native `send`/`reply`",
            ),
        }

        for path, clauses in required.items():
            with self.subTest(path=path):
                text = normalized(path)
                for clause in clauses:
                    self.assertEqual(
                        text.count(clause),
                        1,
                        f"{path} must contain exactly one canonical clause: {clause}",
                    )

        current_contracts = {
            "CHANGELOG.md": (
                "JSON method or field shapes",
                "method and field shapes, persistence schema",
            ),
            "docs/zynk/SPEC.md": (
                "protocol 20, method and field shapes",
            ),
        }
        for path, stale_clauses in current_contracts.items():
            with self.subTest(path=path):
                text = normalized(path)
                for clause in stale_clauses:
                    self.assertNotIn(clause, text)


if __name__ == "__main__":
    unittest.main()
