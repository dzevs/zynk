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
DELIVERY_STATUS_MARKER = "The source-derived delivery-status roster is:"
DELIVERY_STATUS_CONTRACT_DOCUMENTS = (
    "CHANGELOG.md",
    "docs/zynk/SPEC.md",
    "docs/zynk/decisions/0002-message-protocol-delivery-receipt.md",
    "docs/zynk/fork-patch-ledger.md",
)
AWARENESS_HEADER_MARKER = "The current awareness-header roster is:"
AWARENESS_HEADER_CONTRACT_DOCUMENTS = (
    "docs/zynk/decisions/0005-draft-wire-footer-deferral.md",
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


def parse_send_command_source(
    text: str,
) -> tuple[list[str], list[str], dict[str, str]]:
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

    status_function = declaration_body(
        text, r"pub\s+fn\s+delivery_status_for\s*\([^)]*\)[^{]*\{"
    )
    status_body = without_rust_comments(
        declaration_body(status_function, r"match\s+cmd\s*\{")
    )
    remaining = re.sub(r"\s+", " ", status_body).strip()
    statuses: dict[str, str] = {}
    while remaining:
        if re.match(r"_\s*=>", remaining):
            raise ValueError("delivery_status_for wildcard arm is unsupported")
        match = re.match(
            r"(.+?)\s*=>\s*DeliveryStatus::([A-Za-z_][A-Za-z0-9_]*)\s*,",
            remaining,
        )
        if match is None:
            raise ValueError(f"unsupported delivery_status_for arm: {remaining}")
        variant_list, status = match.groups()
        if status not in {"Submitted", "Drafted"}:
            raise ValueError(f"unknown DeliveryStatus in delivery_status_for: {status}")
        arm_variants = [part.strip() for part in variant_list.split("|")]
        if not arm_variants:
            raise ValueError("delivery_status_for arm must name a SendCommand")
        for part in arm_variants:
            variant_match = re.fullmatch(
                r"SendCommand::([A-Za-z_][A-Za-z0-9_]*)", part
            )
            if variant_match is None:
                raise ValueError(f"unsupported delivery_status_for variant: {part}")
            variant = variant_match.group(1)
            if variant in statuses:
                raise ValueError(f"duplicate delivery_status_for variant: {variant}")
            statuses[variant] = status.lower()
        remaining = remaining[match.end() :].strip()

    if set(variants) != set(statuses):
        raise ValueError(
            "SendCommand variants and delivery_status_for arms must form a bijection"
        )
    public_statuses = {
        public[index]: statuses[variant] for index, variant in enumerate(variants)
    }
    return envelopes, public, public_statuses


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


def canonical_marked_contract(path: str, marker: str) -> str:
    paragraphs = re.split(r"\n\s*\n", (ROOT / path).read_text(encoding="utf-8"))
    normalized_paragraphs = [
        re.sub(r"\s+", " ", paragraph).strip() for paragraph in paragraphs
    ]
    matches = [paragraph for paragraph in normalized_paragraphs if marker in paragraph]
    if len(matches) != 1:
        raise AssertionError(f"{path} must contain exactly one contract marked {marker}")
    return matches[0]


def documented_status_roster(path: str, marker: str) -> dict[str, list[str]]:
    contract = canonical_marked_contract(path, marker)
    match = re.search(
        rf"{re.escape(marker)}\s*submitted:\s*(.*?);\s*drafted:\s*(.*?)\.",
        contract,
    )
    if match is None:
        raise AssertionError(f"{path} has a malformed status roster marked {marker}")
    return {
        "submitted": re.findall(r"`(zynk [^`]+)`", match.group(1)),
        "drafted": re.findall(r"`(zynk [^`]+)`", match.group(2)),
    }


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
        envelopes, public, _statuses = parse_send_command_source(source)
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

    def test_delivery_status_roster_is_source_derived_and_complete(self) -> None:
        source = (ROOT / SEND_COMMAND_SOURCE).read_text(encoding="utf-8")
        envelopes, _public, statuses = parse_send_command_source(source)
        expected = {
            status: [command for command, actual in statuses.items() if actual == status]
            for status in ("submitted", "drafted")
        }
        self.assertEqual(
            expected,
            {
                "submitted": [
                    "zynk agent send",
                    "zynk agent prompt",
                    "zynk pane run",
                    "zynk send",
                    "zynk reply",
                ],
                "drafted": ["zynk pane send-text"],
            },
            "delivery_status_for changed; review the public contract",
        )
        for path in DELIVERY_STATUS_CONTRACT_DOCUMENTS:
            with self.subTest(path=path):
                self.assertEqual(
                    documented_status_roster(path, DELIVERY_STATUS_MARKER), expected
                )

        envelope_statuses = {
            envelope: statuses[
                envelope if envelope.startswith("zynk ") else f"zynk {envelope}"
            ]
            for envelope in envelopes
        }
        submitted = [
            envelope
            for envelope, status in envelope_statuses.items()
            if status == "submitted"
        ]
        drafted = [
            envelope
            for envelope, status in envelope_statuses.items()
            if status == "drafted"
        ]
        claude_clause = (
            f"{' / '.join(submitted)} dispatch (atomic submit) -> submitted; "
            f"{' / '.join(drafted)} (persist only, no dispatch) -> drafted"
        )
        self.assertEqual(normalized("CLAUDE.md").count(claude_clause), 1)

        stale_current_rosters = {
            "docs/zynk/SPEC.md": (
                "only `agent send`/`pane run`/a future submit produce it",
                "native `pane.send_input` ok (`agent send`/`pane run`;",
            ),
            "CLAUDE.md": (
                "`agent send`/`pane run` dispatch via native `pane.send_input`",
            ),
        }
        for path, clauses in stale_current_rosters.items():
            with self.subTest(path=path):
                text = normalized(path)
                for clause in clauses:
                    self.assertNotIn(clause, text)

    def test_current_awareness_header_roster_matches_submitted_commands(self) -> None:
        source = (ROOT / SEND_COMMAND_SOURCE).read_text(encoding="utf-8")
        _envelopes, _public, statuses = parse_send_command_source(source)
        expected = {
            status: [command for command, actual in statuses.items() if actual == status]
            for status in ("submitted", "drafted")
        }
        for path in AWARENESS_HEADER_CONTRACT_DOCUMENTS:
            with self.subTest(path=path):
                self.assertEqual(
                    documented_status_roster(path, AWARENESS_HEADER_MARKER), expected
                )

        header_functions = {
            "src/cli/agent.rs": ("agent_prompt", "agent_send"),
            "src/cli/pane.rs": ("pane_run",),
            "src/cli/native.rs": ("native_send",),
        }
        for path, functions in header_functions.items():
            text = (ROOT / path).read_text(encoding="utf-8")
            for function in functions:
                with self.subTest(path=path, function=function):
                    body = declaration_body(text, rf"fn\s+{function}\s*\([^)]*\)[^{{]*\{{")
                    self.assertIn("crate::zynk::header::render_header", body)

        pane_source = (ROOT / "src/cli/pane.rs").read_text(encoding="utf-8")
        send_text_body = declaration_body(
            pane_source, r"fn\s+pane_send_text\s*\([^)]*\)[^{]*\{"
        )
        self.assertNotIn("render_header", send_text_body)
        native_source = (ROOT / "src/cli/native.rs").read_text(encoding="utf-8")
        self.assertIn("SendCommand::ZynkSend", native_source)
        self.assertIn("SendCommand::ZynkReply", native_source)

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

    def test_rejects_delivery_status_wildcard_arm(self) -> None:
        source = '''
pub enum SendCommand { AgentSend, }
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self { Self::AgentSend => "agent send", }
    }
}
pub fn delivery_status_for(cmd: SendCommand) -> DeliveryStatus {
    match cmd { _ => DeliveryStatus::Submitted, }
}
'''
        with self.assertRaisesRegex(ValueError, "wildcard"):
            parse_send_command_source(source)

    def test_rejects_unsupported_delivery_status_arm_syntax(self) -> None:
        source = '''
pub enum SendCommand { AgentSend, }
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self { Self::AgentSend => "agent send", }
    }
}
pub fn delivery_status_for(cmd: SendCommand) -> DeliveryStatus {
    match cmd {
        SendCommand::AgentSend if enabled() => DeliveryStatus::Submitted,
    }
}
'''
        with self.assertRaisesRegex(ValueError, "unsupported delivery_status_for variant"):
            parse_send_command_source(source)

    def test_rejects_duplicate_delivery_status_variant(self) -> None:
        source = '''
pub enum SendCommand { AgentSend, }
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self { Self::AgentSend => "agent send", }
    }
}
pub fn delivery_status_for(cmd: SendCommand) -> DeliveryStatus {
    match cmd {
        SendCommand::AgentSend => DeliveryStatus::Submitted,
        SendCommand::AgentSend => DeliveryStatus::Drafted,
    }
}
'''
        with self.assertRaisesRegex(ValueError, "duplicate delivery_status_for variant"):
            parse_send_command_source(source)

    def test_rejects_unknown_delivery_status(self) -> None:
        source = '''
pub enum SendCommand { AgentSend, }
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self { Self::AgentSend => "agent send", }
    }
}
pub fn delivery_status_for(cmd: SendCommand) -> DeliveryStatus {
    match cmd { SendCommand::AgentSend => DeliveryStatus::Received, }
}
'''
        with self.assertRaisesRegex(ValueError, "unknown DeliveryStatus"):
            parse_send_command_source(source)

    def test_rejects_missing_delivery_status_variant(self) -> None:
        source = '''
pub enum SendCommand {
    AgentSend,
    AgentPrompt,
}
impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentSend => "agent send",
            Self::AgentPrompt => "agent prompt",
        }
    }
}
pub fn delivery_status_for(cmd: SendCommand) -> DeliveryStatus {
    match cmd { SendCommand::AgentSend => DeliveryStatus::Submitted, }
}
'''
        with self.assertRaisesRegex(ValueError, "delivery_status_for arms must form a bijection"):
            parse_send_command_source(source)


if __name__ == "__main__":
    unittest.main()
