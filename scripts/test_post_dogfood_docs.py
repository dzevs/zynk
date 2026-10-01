#!/usr/bin/env python3
"""Parity checks for public post-dogfood interaction contracts."""

from pathlib import Path
import re
from collections.abc import Callable
from typing import NoReturn
import unittest


ROOT = Path(__file__).resolve().parents[1]
SEND_COMMAND_SOURCE = "src/zynk/message.rs"
F4_COMMAND_CASE_SOURCE = "tests/cli_wrapper.rs"
CLI_AGENT_SOURCE = "src/cli/agent.rs"
AGENT_API_SOURCE = "src/app/api/agents.rs"
PTY_ACTOR_SOURCE = "src/pty/actor/unix.rs"
DETECT_SOURCE = "src/detect/mod.rs"
GIT_STATUS_SOURCE = "src/workspace/git/status.rs"
GIT_REFRESH_SOURCE = "src/app/git_refresh.rs"
APP_API_SOURCE = "src/app/api.rs"
PLATFORM_LINUX_SOURCE = "src/platform/linux.rs"
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
PROMPT_TRANSPORT_MARKER = "The source-derived agent-prompt transport contract is:"
PROMPT_TRANSPORT_CONTRACT_DOCUMENTS = (
    "CHANGELOG.md",
    "docs/zynk/SPEC.md",
    "docs/zynk/fork-patch-ledger.md",
)
MANUAL_RESUME_CONTRACT_MARKERS = {
    "CHANGELOG.md": "Safe flags from a manually launched official Claude or Codex process",
    "README.md": "For manually launched official agents",
    "docs/zynk/SPEC.md": "Snapshot preservation for manually launched official agents",
    "docs/zynk/fork-patch-ledger.md": (
        "Gate-3 v14 selected-process and pipe-deadline corrections bind"
    ),
}
WORKING_SHIMMER_CONTRACT_MARKERS = {
    "CHANGELOG.md": "Working-label shimmer now keeps",
    "README.md": "With `working_animation = true`",
    "docs/zynk/SPEC.md": "The literal `working` label on expanded sidebar rows",
    "docs/zynk/fork-patch-ledger.md": "Working labels preserve each surface's resting foreground",
}
DIRTY_STATUS_CONTRACT_MARKERS = {
    "CHANGELOG.md": "The existing sidebar `git_status` token now prefixes",
    "README.md": "The `git_status` token renders a green `+N`",
    "docs/zynk/SPEC.md": "The plain configured `git_status` token also demands",
    "docs/zynk/fork-patch-ledger.md": (
        "Post-read dirty-status validation now runs while"
    ),
}
PINNED_PROMPT_COMMAND = "zynk agent prompt"
PINNED_PROMPT_METHOD = "agent.prompt"
PINNED_PROMPT_DELAY_MS = 300
PINNED_DIRECT_COMMANDS = (
    "zynk agent send",
    "zynk pane run",
    "zynk send",
    "zynk reply",
)
PINNED_DIRECT_METHOD = "pane.send_input"
PINNED_DRAFT_COMMAND = "zynk pane send-text"
PINNED_DRAFT_METHOD = "pane.send_text"


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


def parse_f4_command_cases(
    text: str,
) -> tuple[list[str], list[list[str]], list[str]]:
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
    submit_methods: list[str] = []
    for entry in entries:
        value = entry.group(1)
        envelope = re.findall(r'envelope:\s*"([^"\\]+)"', value)
        argv = re.findall(r"cli_argv:\s*&\[(.*?)\]", value, flags=re.DOTALL)
        submit_method = re.findall(r'submit_method:\s*"([^"\\]+)"', value)
        if len(envelope) != 1 or len(argv) != 1 or len(submit_method) != 1:
            raise ValueError(
                "F4 command case needs one envelope, cli_argv, and submit_method"
            )
        strings = re.findall(r'"([^"\\]+)"', argv[0])
        if not strings:
            raise ValueError("F4 command case cli_argv must not be empty")
        envelopes.append(envelope[0])
        argvs.append(strings)
        submit_methods.append(submit_method[0])
    return envelopes, argvs, submit_methods


def rust_method_to_wire(variant: str) -> str:
    words = re.findall(r"[A-Z][a-z0-9]*", variant)
    if not words or "".join(words) != variant:
        raise ValueError(f"unsupported Method variant syntax: {variant}")
    return ".".join(word.lower() for word in words)


def parse_cli_prompt_method(text: str) -> str:
    body = without_rust_comments(
        declaration_body(text, r"fn\s+agent_prompt\s*\([^)]*\)[^{]*\{")
    )
    methods = re.findall(r"method:\s*Method::([A-Za-z_][A-Za-z0-9_]*)\s*\(", body)
    if len(methods) != 1:
        raise ValueError("agent_prompt must dispatch exactly one Method variant")
    if methods[0] != "AgentPrompt":
        raise ValueError("agent_prompt must dispatch Method::AgentPrompt")
    return rust_method_to_wire(methods[0])


def parse_prompt_api_transport(
    text: str, fail: Callable[[str], NoReturn] | None = None
) -> tuple[int, str, str]:
    body = without_rust_comments(
        declaration_body(text, r"fn\s+handle_agent_prompt\s*\([^)]*\)[^{]*\{")
    )
    if re.search(r"\.try_send_bytes\s*\(", body):
        message = "handle_agent_prompt must not use the one-vector send path"
        if fail is not None:
            fail(message)
        raise ValueError(message)

    encoded_parts = re.findall(
        r"let\s*\(\s*mut\s+([A-Za-z_][A-Za-z0-9_]*)\s*,\s*"
        r"([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*=\s*"
        r"crate::app::api_helpers::encode_api_submission_parts\s*\(",
        body,
    )
    delayed_calls = re.findall(
        r"\.try_send_bytes_with_delayed_suffix\s*\(\s*"
        r"Bytes::from\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*,\s*"
        r"Bytes::from\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*,\s*"
        r"([A-Z][A-Z0-9_]*)\s*,?\s*\)",
        body,
    )
    if len(encoded_parts) != 1 or len(delayed_calls) != 1:
        raise ValueError(
            "handle_agent_prompt needs one encoded-parts call and one delayed-suffix call"
        )
    immediate, delayed, delay_constant = delayed_calls[0]
    if encoded_parts[0] != (immediate, delayed):
        raise ValueError("delayed-suffix arguments do not match encoded submission parts")

    delay_definitions = re.findall(
        rf"const\s+{re.escape(delay_constant)}\s*:\s*Duration\s*=\s*"
        r"Duration::from_millis\(\s*([0-9]+)\s*\)\s*;",
        without_rust_comments(text),
    )
    if len(delay_definitions) != 1:
        raise ValueError("prompt submit delay must have one literal millisecond definition")
    delay_ms = int(delay_definitions[0])
    if delay_ms != PINNED_PROMPT_DELAY_MS:
        raise ValueError("prompt submit delay must equal 300 ms")
    return delay_ms, immediate, delayed


def validate_pinned_prompt_transport(transport: dict[str, object]) -> None:
    actual = (
        transport["prompt_command"],
        transport["prompt_method"],
        transport["delay_ms"],
        tuple(transport["direct_commands"]),
        transport["direct_method"],
        transport["draft_command"],
        transport["draft_method"],
    )
    expected = (
        PINNED_PROMPT_COMMAND,
        PINNED_PROMPT_METHOD,
        PINNED_PROMPT_DELAY_MS,
        PINNED_DIRECT_COMMANDS,
        PINNED_DIRECT_METHOD,
        PINNED_DRAFT_COMMAND,
        PINNED_DRAFT_METHOD,
    )
    if actual != expected:
        raise ValueError("F4 transport contract does not match the pinned method map")


def validate_actor_delayed_suffix_order(text: str) -> None:
    body = without_rust_comments(
        declaration_body(text, r"fn\s+handle_data_command\s*\([^)]*\)[^{]*\{")
    )
    arm_matches = list(
        re.finditer(
            r"PtyIoDataCommand::WriteUserInputWithDelayedSuffix\s*\{\s*"
            r"immediate\s*,\s*delayed\s*,\s*delay\s*,\s*\.\.\s*\}\s*=>\s*\{",
            body,
        )
    )
    if len(arm_matches) != 1:
        raise ValueError("PTY actor needs one delayed-suffix command arm")
    opening = arm_matches[0].end() - 1
    arm = body[opening + 1 : matching_delimiter(body, opening, "{", "}")]
    immediate = "self.enqueue_write(immediate);"
    delayed = "self.enqueue_delayed_write(delayed, delay);"
    if arm.count(immediate) != 1 or arm.count(delayed) != 1:
        raise ValueError("PTY actor delayed-suffix arm needs one immediate and delayed write")
    if arm.index(immediate) > arm.index(delayed):
        raise ValueError("PTY actor must enqueue immediate bytes before delayed bytes")


def derive_prompt_transport_contract(
    send_source: str,
    f4_source: str,
    cli_source: str,
    api_source: str,
    actor_source: str,
    fail: Callable[[str], NoReturn] | None = None,
) -> dict[str, object]:
    envelopes, public, statuses = parse_send_command_source(send_source)
    case_envelopes, _argvs, submit_methods = parse_f4_command_cases(f4_source)
    if case_envelopes != envelopes or len(submit_methods) != len(public):
        raise ValueError("F4 command cases must match SendCommand order and cardinality")

    prompt_method = parse_cli_prompt_method(cli_source)
    delay_ms, immediate, delayed = parse_prompt_api_transport(api_source, fail)
    validate_actor_delayed_suffix_order(actor_source)
    if (immediate, delayed) != ("text", "enter"):
        raise ValueError("agent prompt must encode immediate text and delayed Enter")

    submit_by_command = dict(zip(public, submit_methods, strict=True))
    prompt_commands = [
        command for command, method in submit_by_command.items() if method == prompt_method
    ]
    if len(prompt_commands) != 1 or statuses[prompt_commands[0]] != "submitted":
        raise ValueError("the CLI prompt Method must map to one submitted F4 command")
    prompt_command = prompt_commands[0]

    drafted_commands = [
        command for command, status in statuses.items() if status == "drafted"
    ]
    if len(drafted_commands) != 1:
        raise ValueError("the F4 transport contract needs exactly one drafted command")
    draft_command = drafted_commands[0]
    draft_method = submit_by_command[draft_command]

    direct_commands = [
        command
        for command, status in statuses.items()
        if status == "submitted" and command != prompt_command
    ]
    direct_methods = {submit_by_command[command] for command in direct_commands}
    if len(direct_methods) != 1:
        raise ValueError("non-prompt submitted commands must share one transport method")
    direct_method = direct_methods.pop()
    if len({prompt_method, direct_method, draft_method}) != 3:
        raise ValueError("prompt, direct-submit, and draft methods must be distinct")

    transport = {
        "prompt_command": prompt_command,
        "prompt_method": prompt_method,
        "delay_ms": delay_ms,
        "direct_commands": direct_commands,
        "direct_method": direct_method,
        "draft_command": draft_command,
        "draft_method": draft_method,
    }
    validate_pinned_prompt_transport(transport)
    return transport


def markdown_list(values: list[str]) -> str:
    quoted = [f"`{value}`" for value in values]
    if len(quoted) < 2:
        return "".join(quoted)
    return f"{', '.join(quoted[:-1])}, and {quoted[-1]}"


def prompt_transport_contract(transport: dict[str, object]) -> str:
    return (
        f"{PROMPT_TRANSPORT_MARKER} `{transport['prompt_command']}` dispatches "
        f"`{transport['prompt_method']}`, which queues prompt text immediately and Enter "
        f"{transport['delay_ms']} ms later through one ordered PTY actor command; "
        f"{markdown_list(transport['direct_commands'])} dispatch "
        f"`{transport['direct_method']}` as one validated encoded byte vector; "
        f"`{transport['draft_command']}` dispatches `{transport['draft_method']}` without Enter."
    )


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
        test_envelopes, test_argvs, _submit_methods = parse_f4_command_cases(
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

    def test_agent_prompt_transport_contract_is_source_derived(self) -> None:
        transport = derive_prompt_transport_contract(
            (ROOT / SEND_COMMAND_SOURCE).read_text(encoding="utf-8"),
            (ROOT / F4_COMMAND_CASE_SOURCE).read_text(encoding="utf-8"),
            (ROOT / CLI_AGENT_SOURCE).read_text(encoding="utf-8"),
            (ROOT / AGENT_API_SOURCE).read_text(encoding="utf-8"),
            (ROOT / PTY_ACTOR_SOURCE).read_text(encoding="utf-8"),
            self.fail,
        )
        expected = prompt_transport_contract(transport)
        for path in PROMPT_TRANSPORT_CONTRACT_DOCUMENTS:
            with self.subTest(path=path):
                contract = canonical_marked_contract(path, PROMPT_TRANSPORT_MARKER)
                self.assertEqual(contract.count(expected), 1)

        prompt = str(transport["prompt_command"])
        prompt_short = prompt.removeprefix("zynk ")
        prompt_method = str(transport["prompt_method"])
        direct_method = str(transport["direct_method"])
        readme = normalized("README.md")
        self.assertIn(
            f"`{prompt_short}` submits only when the named agent is on the same terminal",
            readme,
        )
        self.assertIn(
            "Prompt text and the delayed Enter remain one ordered PTY actor command; "
            "later input cannot overtake Enter.",
            readme,
        )
        adr = normalized("docs/zynk/decisions/0015-agent-prompt-proof-source.md")
        self.assertIn(f"`proof_source = {prompt_method}`", adr)
        self.assertIn(f"**Reuse `{direct_method}`.** Rejected", adr)

        stale_claims = {
            "CHANGELOG.md": (
                "`pane.send_input`, `agent send`, `agent prompt`, and `pane run` now "
                "enqueue one encoded byte vector",
            ),
            "docs/zynk/SPEC.md": (
                "`pane.send_input`, and therefore `agent send`, `agent prompt`, and "
                "`pane run`, validates all keys before mutation and enqueues one byte vector",
            ),
        }
        for path, clauses in stale_claims.items():
            with self.subTest(path=path):
                text = normalized(path)
                for clause in clauses:
                    self.assertNotIn(clause, text)

    def test_manual_resume_contract_is_documented_exactly(self) -> None:
        required = {
            "CHANGELOG.md": (
                "exact foreground process selected by leader-first or priority detection",
                "same-name sibling cannot add or remove them",
                "most recent live launch wins over an older recorded command",
                "Tier-B captures resume by the official command name",
                "Pi retains safe Tier-A flags",
            ),
            "README.md": (
                "most recent live launch decides which flags survive",
                "later canonical or rejected live command suppresses older privileged flags",
                "resume through the official command name",
                "Pi's rewritten process title is not a faithful flag view",
                "exactly one `--no-daemon`",
            ),
            "docs/zynk/SPEC.md": (
                "present live foreground as authoritative",
                "canonical-only or rejected launch records a tombstone",
                "normalizes argv0 to the official agent command name",
                "Pi's rewritten process title is not a faithful flag view",
                "exactly one `--no-daemon`",
            ),
            "docs/zynk/fork-patch-ledger.md": (
                "exact process index chosen by leader-first or priority detection",
                "no longer performs a second normalized-name search",
                "neither add an allowlisted flag",
                "nor drop a flag",
            ),
        }
        stale = (
            "no normalization of Tier-B argv0",
            "vendor-internal executable path is equivalent",
            "older privileged flags remain authoritative",
        )
        for path, marker in MANUAL_RESUME_CONTRACT_MARKERS.items():
            with self.subTest(path=path):
                contract = canonical_marked_contract(path, marker)
                for clause in required[path]:
                    self.assertIn(clause, contract)
                for clause in stale:
                    self.assertNotIn(clause, contract)

        source = (ROOT / DETECT_SOURCE).read_text(encoding="utf-8")
        capture = declaration_body(
            source,
            r"fn\s+foreground_agent_argv_from_job\s*\([^)]*\)[^{]*\{",
        )
        compact_capture = re.sub(r"\s+", "", capture)
        self.assertIn("job.processes.get(selection.process_index)", compact_capture)
        self.assertNotIn(".find(", capture)
        selection = declaration_body(
            source,
            r"fn\s+select_agent_in_job\s*\([^)]*\)[^{]*\{",
        )
        self.assertIn("process_index", selection)

    def test_working_shimmer_contract_is_documented_exactly(self) -> None:
        required = {
            "CHANGELOG.md": (
                "each surface's existing resting foreground",
                "two leading letters at full red",
                "one trailing letter at a half blend",
                "128 ms cadence",
            ),
            "README.md": (
                "each surface's resting foreground color",
                "two leading letters use full `palette.red`",
                "trailing letter is a half blend",
                "ordinary working text remains yellow",
                "muted mobile/context labels remain muted",
            ),
            "docs/zynk/SPEC.md": (
                "per-surface resting foreground never change",
                "target is `palette.red`",
                "two leading cells at full red",
                "trailing cell at a half blend",
                "muted mobile/context labels remain `palette.overlay0`",
            ),
            "docs/zynk/fork-patch-ledger.md": (
                "each surface's resting foreground",
                "two leading letters are full red",
                "trailing letter is a half blend",
                "muted mobile/context labels stay overlay0",
            ),
        }
        stale = (
            "target is `palette.text`",
            "sweep toward white",
            "every surface's base is yellow",
            "universal yellow base",
        )
        for path, marker in WORKING_SHIMMER_CONTRACT_MARKERS.items():
            with self.subTest(path=path):
                contract = canonical_marked_contract(path, marker)
                for clause in required[path]:
                    self.assertIn(clause, contract)
                for clause in stale:
                    self.assertNotIn(clause, contract)

    def test_dirty_status_contract_is_documented_exactly(self) -> None:
        required = {
            "CHANGELOG.md": (
                "green `+N`",
                "individual untracked files count",
                "ignored paths do not",
                "configured-token background refresh only",
                "at most one bounded query per 5 seconds",
                "both output pipes to reach EOF inside 250 ms",
                "signals the original process group before reaping the direct child",
                "descendant retaining a pipe can no longer stall later Git refreshes",
                "back off for 30 seconds",
            ),
            "README.md": (
                "green `+N`",
                "every individual untracked file count",
                "plain configured `git_status` token demands them",
                "outside rendering",
                "at most once per checkout per 5 seconds",
                "last successful value (or hides `+N` before the first success)",
                "suppresses another dirty query for 30 seconds",
                "both output pipes to reach EOF inside the 250 ms query deadline",
                "signals the original process group before reaping the direct child",
                "unreaped child reserves its PID/PGID",
                "separate 250 ms cleanup grace",
                "cannot stall the global worker or later refreshes",
                "custom global `core.excludesFile` is intentionally not honored",
            ),
            "docs/zynk/SPEC.md": (
                "green `+N`",
                "every individual untracked file count",
                "plain `git_status`",
                "at most one bounded query per 5,000 ms",
                "optional locks and fsmonitor disabled",
                "both output pipes to reach EOF inside one 250 ms wall-clock deadline",
                "signals the original process group before reaping the direct child",
                "unreaped child reserves its PID/PGID",
                "separate 250 ms cleanup grace",
                "cannot stall the global refresh worker or later refreshes",
                "custom global `core.excludesFile` is not honored",
                "retains the last successful count (or hides it before first success)",
                "delays the next attempt for 30,000 ms",
                "No Git or filesystem work occurs on the render path",
            ),
            "docs/zynk/fork-patch-ledger.md": (
                "direct Git child to exit and both output pipes to reach EOF",
                "250 ms wall-clock deadline",
                "waitid",
                "WNOHANG | WNOWAIT",
                "signals the original process group before reaping the direct child",
                "unreaped zombie reserves its PID/PGID",
                "separate 250 ms cleanup grace",
                "escaped-descendant fixture records the direct PID",
                "Post-read dirty-status validation",
                "validated status-zero result commits success and reaps exactly once",
                "any other validator failure",
                "status-zero fixture emits malformed porcelain",
                "observes `State: Z` at the parser boundary",
            ),
        }
        for path, marker in DIRTY_STATUS_CONTRACT_MARKERS.items():
            with self.subTest(path=path):
                text = normalized(path)
                self.assertEqual(text.count(marker), 1)
                for clause in required[path]:
                    self.assertIn(clause, text)

        for path in DIRTY_STATUS_CONTRACT_MARKERS:
            with self.subTest(path=path, stale=True):
                text = normalized(path)
                self.assertNotIn("untracked directories count once", text)
                self.assertNotIn("query on every 1.5-second refresh", text)
                self.assertNotIn("failure clears the dirty count", text)

        status_source = (ROOT / GIT_STATUS_SOURCE).read_text(encoding="utf-8")
        for constant in ("GIT_DIRTY_STATUS_TIMEOUT", "GIT_DIRTY_STATUS_CLEANUP_GRACE"):
            definitions = re.findall(
                rf"const\s+{constant}\s*:\s*Duration\s*=\s*"
                r"Duration::from_millis\(\s*([0-9]+)\s*\)\s*;",
                without_rust_comments(status_source),
            )
            self.assertEqual(definitions, ["250"], constant)
        dirty_body = declaration_body(
            status_source,
            r"fn\s+run_dirty_status_command\s*\([^)]*\)[^{]*\{",
        )
        self.assertIn("wait_for_bounded_child_output", dirty_body)
        self.assertIn("validate_dirty_status_output", dirty_body)
        self.assertIn("BoundedChildOutputError::Validation", status_source)
        self.assertIn(
            "dirty_status_malformed_output_signals_group_before_reaping",
            status_source,
        )
        self.assertIn("GIT_CONFIG_GLOBAL", status_source)
        self.assertIn("GIT_CONFIG_SYSTEM", status_source)

        platform_source = (ROOT / PLATFORM_LINUX_SOURCE).read_text(encoding="utf-8")
        platform_body = declaration_body(
            platform_source,
            r"fn\s+wait_for_bounded_child_output(?:<[^>]+>)?\s*\([^{}]*\)[^{]*\{",
        )
        self.assertIn("libc::poll", platform_body)
        self.assertIn("observe_child_exit_without_reaping", platform_body)
        validation = platform_body.index("validate(BoundedChildOutput")
        success_reap = platform_body.index("child.try_wait()", validation)
        self.assertLess(validation, success_reap)
        self.assertIn("libc::WNOWAIT", platform_source)
        self.assertNotIn("thread::spawn", platform_body)
        self.assertNotIn(".join(", platform_body)

        refresh_source = (ROOT / GIT_REFRESH_SOURCE).read_text(encoding="utf-8")
        worker = declaration_body(
            refresh_source,
            r"fn\s+start_git_status_refresh_if_due\s*\([^)]*\)[^{]*\{",
        )
        self.assertIn("AppEvent::GitStatusRefreshed", worker)
        app_api_source = (ROOT / APP_API_SOURCE).read_text(encoding="utf-8")
        self.assertIn("self.git_refresh_in_flight = false;", app_api_source)

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


class PromptTransportParserTest(unittest.TestCase):
    def test_rejects_wrong_literal_cli_prompt_method(self) -> None:
        source = """
fn agent_prompt(args: &[String]) {
    send(Request { method: Method::PaneSendInput(one()) });
}
"""
        with self.assertRaisesRegex(
            ValueError, "agent_prompt must dispatch Method::AgentPrompt"
        ):
            parse_cli_prompt_method(source)

    def test_rejects_wrong_literal_prompt_delay(self) -> None:
        source = """
const DELAY: Duration = Duration::from_millis(17);
fn handle_agent_prompt() {
    let (mut text, enter) = crate::app::api_helpers::encode_api_submission_parts();
    runtime.try_send_bytes_with_delayed_suffix(
        Bytes::from(text),
        Bytes::from(enter),
        DELAY,
    );
}
"""
        with self.assertRaisesRegex(ValueError, "prompt submit delay must equal 300 ms"):
            parse_prompt_api_transport(source)

    def test_rejects_coordinated_transport_method_swap(self) -> None:
        transport = {
            "prompt_command": PINNED_PROMPT_COMMAND,
            "prompt_method": PINNED_PROMPT_METHOD,
            "delay_ms": PINNED_PROMPT_DELAY_MS,
            "direct_commands": list(PINNED_DIRECT_COMMANDS),
            "direct_method": PINNED_DRAFT_METHOD,
            "draft_command": PINNED_DRAFT_COMMAND,
            "draft_method": PINNED_DIRECT_METHOD,
        }
        with self.assertRaisesRegex(
            ValueError, "F4 transport contract does not match the pinned method map"
        ):
            validate_pinned_prompt_transport(transport)

    def test_rejects_multiple_cli_prompt_methods(self) -> None:
        source = """
fn agent_prompt(args: &[String]) {
    send(Request { method: Method::AgentPrompt(one()) });
    send(Request { method: Method::PaneSendInput(two()) });
}
"""
        with self.assertRaisesRegex(ValueError, "exactly one Method variant"):
            parse_cli_prompt_method(source)

    def test_rejects_ambiguous_prompt_delay(self) -> None:
        source = """
const DELAY: Duration = Duration::from_millis(17);
const DELAY: Duration = Duration::from_millis(18);
fn handle_agent_prompt() {
    let (mut text, enter) = crate::app::api_helpers::encode_api_submission_parts();
    runtime.try_send_bytes_with_delayed_suffix(
        Bytes::from(text),
        Bytes::from(enter),
        DELAY,
    );
}
"""
        with self.assertRaisesRegex(ValueError, "one literal millisecond definition"):
            parse_prompt_api_transport(source)

    def test_rejects_one_vector_prompt_send(self) -> None:
        source = """
const DELAY: Duration = Duration::from_millis(17);
fn handle_agent_prompt() {
    let (mut text, enter) = crate::app::api_helpers::encode_api_submission_parts();
    runtime.try_send_bytes(Bytes::from(text));
    runtime.try_send_bytes_with_delayed_suffix(
        Bytes::from(text),
        Bytes::from(enter),
        DELAY,
    );
}
"""
        with self.assertRaisesRegex(ValueError, "one-vector send path"):
            parse_prompt_api_transport(source)

    def test_rejects_reversed_actor_queue_order(self) -> None:
        source = """
fn handle_data_command(&mut self, command: PtyIoDataCommand) -> bool {
    match command {
        PtyIoDataCommand::WriteUserInputWithDelayedSuffix {
            immediate,
            delayed,
            delay,
            ..
        } => {
            self.enqueue_delayed_write(delayed, delay);
            self.enqueue_write(immediate);
        }
    }
}
"""
        with self.assertRaisesRegex(ValueError, "immediate bytes before delayed bytes"):
            validate_actor_delayed_suffix_order(source)


if __name__ == "__main__":
    unittest.main()
