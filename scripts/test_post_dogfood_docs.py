#!/usr/bin/env python3
"""Parity checks for public post-dogfood interaction contracts."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
SEND_COMMAND_SOURCE = "src/zynk/message.rs"
F4_COMMAND_CASE_SOURCE = "tests/cli_wrapper.rs"
F4_ROSTER_MARKER = "The complete F4 SendOutcome command roster is:"
F4_CONTRACT_DOCUMENTS = (
    "CHANGELOG.md",
    "README.md",
    "SKILL.md",
    "docs/zynk/SPEC.md",
    "docs/zynk/decisions/0014-receipt-principals-same-uid-pane-tree.md",
    "docs/zynk/fork-patch-ledger.md",
)


def normalized(path: str) -> str:
    return re.sub(r"\s+", " ", (ROOT / path).read_text(encoding="utf-8")).strip()


def matching_delimiter(text: str, opening: int, left: str, right: str) -> int:
    depth = 0
    quote: str | None = None
    line_comment = False
    block_depth = 0
    index = opening
    while index < len(text):
        if line_comment:
            if text[index] == "\n":
                line_comment = False
            index += 1
            continue
        if block_depth:
            if text.startswith("/*", index):
                block_depth += 1
                index += 2
            elif text.startswith("*/", index):
                block_depth -= 1
                index += 2
            else:
                index += 1
            continue
        if quote:
            if text[index] == "\\":
                index += 2
            elif text[index] == quote:
                quote = None
                index += 1
            else:
                index += 1
            continue
        if text.startswith("//", index):
            line_comment = True
            index += 2
        elif text.startswith("/*", index):
            block_depth = 1
            index += 2
        elif text[index] == "'":
            lifetime = re.match(r"'[A-Za-z_][A-Za-z0-9_]*", text[index:])
            if lifetime and text[index + len(lifetime.group(0)) :].startswith("'") is False:
                index += len(lifetime.group(0))
            else:
                quote = text[index]
                index += 1
        elif text[index] == '"':
            quote = text[index]
            index += 1
        elif text[index] == left:
            depth += 1
            index += 1
        elif text[index] == right:
            depth -= 1
            if depth == 0:
                return index
            index += 1
        else:
            index += 1
    raise ValueError(f"unclosed {left}{right} body")


def declaration_body(text: str, pattern: str) -> str:
    match = re.search(pattern, text)
    if match is None:
        raise ValueError(f"missing declaration matching {pattern!r}")
    opening = text.find("{", match.start(), match.end())
    if opening < 0:
        raise ValueError(f"declaration has no body: {pattern!r}")
    closing = matching_delimiter(text, opening, "{", "}")
    return text[opening + 1 : closing]


def without_rust_comments(text: str) -> str:
    result: list[str] = []
    index = 0
    quote: str | None = None
    block_depth = 0
    while index < len(text):
        if block_depth:
            if text.startswith("/*", index):
                block_depth += 1
                result.extend("  ")
                index += 2
            elif text.startswith("*/", index):
                block_depth -= 1
                result.extend("  ")
                index += 2
            else:
                result.append("\n" if text[index] == "\n" else " ")
                index += 1
            continue
        if quote:
            result.append(text[index])
            if text[index] == "\\" and index + 1 < len(text):
                result.append(text[index + 1])
                index += 2
            elif text[index] == quote:
                quote = None
                index += 1
            else:
                index += 1
            continue
        if text.startswith("//", index):
            end = text.find("\n", index)
            if end < 0:
                result.extend(" " * (len(text) - index))
                break
            result.extend(" " * (end - index))
            index = end
        elif text.startswith("/*", index):
            block_depth = 1
            result.extend("  ")
            index += 2
        else:
            result.append(text[index])
            if text[index] == "'":
                lifetime = re.match(r"'[A-Za-z_][A-Za-z0-9_]*", text[index:])
                if lifetime and text[index + len(lifetime.group(0)) :].startswith("'") is False:
                    result.extend(lifetime.group(0)[1:])
                    index += len(lifetime.group(0)) - 1
                else:
                    quote = text[index]
            elif text[index] == '"':
                quote = text[index]
            index += 1
    if block_depth or quote:
        raise ValueError("unterminated Rust comment or string")
    return "".join(result)


def parse_send_command_source(text: str) -> tuple[list[str], list[str]]:
    enum_body = without_rust_comments(
        declaration_body(text, r"pub\s+enum\s+SendCommand\s*\{")
    )
    variants: list[str] = []
    for raw in enum_body.splitlines():
        line = raw.strip()
        if not line:
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)\s*,", line)
        if match is None:
            raise ValueError(f"unsupported SendCommand variant syntax: {line}")
        variants.append(match.group(1))
    if not variants or len(set(variants)) != len(variants):
        raise ValueError("SendCommand variants must be unique and nonempty")

    impl_body = declaration_body(text, r"impl\s+SendCommand\s*\{")
    function_body = declaration_body(
        impl_body, r"pub\s+fn\s+as_str\s*\([^)]*\)[^{]*\{"
    )
    match_body = without_rust_comments(
        declaration_body(function_body, r"match\s+self\s*\{")
    )
    arms: dict[str, str] = {}
    for raw in match_body.splitlines():
        line = raw.strip()
        if not line:
            continue
        if re.match(r"_\s*=>", line):
            raise ValueError("SendCommand::as_str wildcard arm is unsupported")
        match = re.fullmatch(
            r'Self::([A-Za-z_][A-Za-z0-9_]*)\s*=>\s*"([^"\\]+)"\s*,',
            line,
        )
        if match is None:
            raise ValueError(f"unsupported SendCommand::as_str arm: {line}")
        variant, label = match.groups()
        if variant in arms:
            raise ValueError(f"duplicate SendCommand::as_str arm: {variant}")
        arms[variant] = label
    if set(variants) != set(arms) or len(set(arms.values())) != len(arms):
        raise ValueError("SendCommand variants and as_str labels must form a bijection")
    envelopes = [arms[variant] for variant in variants]
    public = [label if label.startswith("zynk ") else f"zynk {label}" for label in envelopes]
    return envelopes, public


def parse_f4_command_cases(text: str) -> tuple[list[str], list[list[str]]]:
    match = re.search(
        r"const\s+POST_DOGFOOD_F4_COMMAND_CASES\b[^=]*=\s*&\[(.*?)\n\];",
        text,
        flags=re.DOTALL,
    )
    if match is None:
        raise ValueError("missing POST_DOGFOOD_F4_COMMAND_CASES")
    body = match.group(1)
    entry_pattern = re.compile(r"PostDogfoodF4CommandCase\s*\{(.*?)\}", re.DOTALL)
    entries = list(entry_pattern.finditer(body))
    if not entries or re.sub(entry_pattern, "", body).strip(" ,\n\t"):
        raise ValueError("unsupported F4 command case-table syntax")
    envelopes: list[str] = []
    argvs: list[list[str]] = []
    for entry in entries:
        value = entry.group(1)
        envelope = re.findall(r'envelope:\s*"([^"\\]+)"', value)
        argv = re.findall(r"cli_argv:\s*&\[(.*?)\]", value, flags=re.DOTALL)
        if len(envelope) != 1 or len(argv) != 1:
            raise ValueError("F4 command case needs one envelope and cli_argv")
        strings = re.findall(r'"([^"\\]+)"', argv[0])
        if not strings:
            raise ValueError("F4 command case cli_argv must not be empty")
        envelopes.append(envelope[0])
        argvs.append(strings)
    return envelopes, argvs


def canonical_f4_contract(path: str) -> str:
    paragraphs = re.split(
        r"\n\s*\n", (ROOT / path).read_text(encoding="utf-8")
    )
    normalized_paragraphs = [
        re.sub(r"\s+", " ", paragraph).strip() for paragraph in paragraphs
    ]
    matches = [
        paragraph
        for paragraph in normalized_paragraphs
        if F4_ROSTER_MARKER in paragraph
    ]
    if len(matches) != 1:
        raise AssertionError(f"{path} must contain exactly one canonical F4 roster")
    return matches[0]


def documented_f4_roster(path: str) -> list[str]:
    contract = canonical_f4_contract(path)
    match = re.search(
        rf"{re.escape(F4_ROSTER_MARKER)}\s*(.*?)\.", contract,
    )
    if match is None:
        raise AssertionError(f"{path} has a malformed canonical F4 roster")
    return re.findall(r"`(zynk [^`]+)`", match.group(1))


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

    def test_f4_identity_verification_roster_is_source_derived_and_complete(self) -> None:
        source = (ROOT / SEND_COMMAND_SOURCE).read_text(encoding="utf-8")
        envelopes, public = parse_send_command_source(source)
        test_envelopes, test_argvs = parse_f4_command_cases(
            (ROOT / F4_COMMAND_CASE_SOURCE).read_text(encoding="utf-8")
        )
        self.assertEqual(len(envelopes), 6, "SendCommand cardinality is a contract change")
        self.assertEqual(test_envelopes, envelopes)
        for envelope, argv in zip(envelopes, test_argvs, strict=True):
            cli_prefix = envelope.removeprefix("zynk ").split()
            self.assertEqual(argv[: len(cli_prefix)], cli_prefix, (envelope, argv))
        for path in F4_CONTRACT_DOCUMENTS:
            with self.subTest(path=path):
                self.assertEqual(documented_f4_roster(path), public)

    def test_f4_identity_verification_shape_is_documented_exactly(self) -> None:
        required = {
            path: (
                "`from.identity_verification`",
                "`verified`",
                "`unverified`",
                "omitted without a Codex hint",
                "no routing or receipt authority",
                "top-level `identity_verification` on `zynk whoami --json` and implicit `zynk inbox --json`",
            )
            for path in F4_CONTRACT_DOCUMENTS
        }
        required["CHANGELOG.md"] += (
            "protocol 20 and the socket API method and field shapes remain unchanged",
        )
        for path, clauses in required.items():
            with self.subTest(path=path):
                text = canonical_f4_contract(path)
                for clause in clauses:
                    self.assertEqual(
                        text.count(clause),
                        1,
                        f"{path} must contain exactly one canonical clause: {clause}",
                    )

        spec = normalized("docs/zynk/SPEC.md")
        for clause in (
            "final composed frame is the authority for committed local animation demand",
            "a full-frame layer replaces every animation contribution beneath it",
        ):
            self.assertEqual(spec.count(clause), 1, clause)

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


class SendCommandRosterParserTest(unittest.TestCase):
    def test_rejects_unsupported_variant_syntax(self) -> None:
        source = '''
pub enum SendCommand { AgentSend(u8), }
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentSend => "agent send",
        }
    }
}
'''
        with self.assertRaisesRegex(ValueError, "unsupported SendCommand variant"):
            parse_send_command_source(source)

    def test_rejects_wildcard_arm(self) -> None:
        source = '''
pub enum SendCommand { AgentSend, }
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentSend => "agent send",
            _ => "other",
        }
    }
}
'''
        with self.assertRaisesRegex(ValueError, "wildcard"):
            parse_send_command_source(source)

    def test_rejects_non_bijection(self) -> None:
        source = '''
pub enum SendCommand {
    AgentSend,
    AgentPrompt,
}
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self { Self::AgentSend => "agent send", }
    }
}
'''
        with self.assertRaisesRegex(ValueError, "bijection"):
            parse_send_command_source(source)


if __name__ == "__main__":
    unittest.main()
