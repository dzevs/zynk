"""Keep the Linux-only source tree free of reintroduced platform branches."""

import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE_ROOTS = (ROOT / "src", ROOT / "tests")

# These seven test-only gates predate the B2 end-state port. Production code and
# new tests compile as Linux directly; a later batch may remove this finite set.
ALLOWED_UNIX_CFG_ITEMS = {
    ("src/api/server/pane_graphics_stream.rs", "pane_graphics_stream_dispatches_binary_frames"),
    ("src/api/server/pane_graphics_stream.rs", "pane_graphics_stream_reports_open_errors_before_ack"),
    ("src/api/server/pane_graphics_stream.rs", "pane_graphics_stream_closes_claim_after_open_timeout"),
    (
        "src/api/server/pane_graphics_stream.rs",
        "pane_graphics_stream_closes_claim_when_client_disconnects_before_ack",
    ),
    ("src/app/api/plugins/mod.rs", "m844_startup_manifest_runs_once_isolates_failures_and_redacts_argv"),
    ("src/app/api/plugins/runtime.rs", "<no-function>"),
    (
        "src/server/headless.rs",
        "headless_scheduled_tasks_start_pending_agent_resume_without_foreground_client",
    ),
}

ALLOWED_NON_LINUX_CFG_ITEMS = {
    ("src/main.rs", 12, "cfg", 'not(target_os="linux")'),
}


def _rust_files():
    for root in SOURCE_ROOTS:
        yield from sorted(root.rglob("*.rs"))


def _relative(path):
    return path.relative_to(ROOT).as_posix()


def _next_function(lines, start):
    for line in lines[start + 1 :]:
        match = re.search(r"\bfn\s+([A-Za-z0-9_]+)\s*\(", line)
        if match:
            return match.group(1)
        if line.strip() and not line.lstrip().startswith("#"):
            break
    return "<no-function>"


def _skip_non_code(source, start):
    if source.startswith("//", start):
        newline = source.find("\n", start + 2)
        return len(source) if newline < 0 else newline
    if source.startswith("/*", start):
        depth = 1
        cursor = start + 2
        while cursor < len(source) and depth:
            if source.startswith("/*", cursor):
                depth += 1
                cursor += 2
            elif source.startswith("*/", cursor):
                depth -= 1
                cursor += 2
            else:
                cursor += 1
        return cursor

    char_prefix = re.match(r"b?'", source[start:])
    if char_prefix:
        cursor = start + char_prefix.end()
        if cursor >= len(source):
            return None
        if source[cursor] == "\\":
            cursor += 1
            if cursor >= len(source):
                return None
            if source[cursor] == "x":
                cursor += 3
            elif source[cursor] == "u" and cursor + 1 < len(source) and source[cursor + 1] == "{":
                end = source.find("}", cursor + 2)
                if end < 0:
                    return None
                cursor = end + 1
            else:
                cursor += 1
        else:
            cursor += 1
        if cursor < len(source) and source[cursor] == "'":
            return cursor + 1
        return None

    raw = re.match(r"(?:b|c)?r(#{0,255})\"", source[start:])
    if raw:
        terminator = '"' + raw.group(1)
        end = source.find(terminator, start + raw.end())
        return len(source) if end < 0 else end + len(terminator)

    prefix = re.match(r"(?:b|c)?\"", source[start:])
    if prefix:
        cursor = start + prefix.end()
        escaped = False
        while cursor < len(source):
            char = source[cursor]
            cursor += 1
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                break
        return cursor
    return None


def _matching_paren(source, opening):
    depth = 0
    cursor = opening
    while cursor < len(source):
        skipped = _skip_non_code(source, cursor)
        if skipped is not None:
            cursor = skipped
            continue
        char = source[cursor]
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return cursor
        cursor += 1
    raise AssertionError(f"unterminated cfg expression at byte {opening}")


_SCAN_TOKEN = re.compile(
    r'//|/\*|b?\'|(?:b|c)?r#{0,255}"|(?:b|c)?"|#!?\s*\[\s*(?:cfg_attr|cfg)\s*\(|\bcfg\s*!\s*\('
)


def _cfg_constructs(source):
    constructs = []
    cursor = 0
    line = 1
    while cursor < len(source):
        token = _SCAN_TOKEN.search(source, cursor)
        if token is None:
            break
        line += source.count("\n", cursor, token.start())
        cursor = token.start()

        attribute = re.match(r"(#!?)\s*\[\s*(cfg_attr|cfg)\s*\(", source[cursor:])
        macro = re.match(r"cfg\s*!\s*\(", source[cursor:])
        if attribute:
            opening = cursor + attribute.end() - 1
            end = _matching_paren(source, opening)
            constructs.append(
                {
                    "line": line,
                    "inner": attribute.group(1) == "#!",
                    "form": attribute.group(2),
                    "expression": source[opening + 1 : end],
                }
            )
            line += source.count("\n", cursor, end + 1)
            cursor = end + 1
            continue
        if macro and (cursor == 0 or not (source[cursor - 1].isalnum() or source[cursor - 1] == "_")):
            opening = cursor + macro.end() - 1
            end = _matching_paren(source, opening)
            constructs.append(
                {
                    "line": line,
                    "inner": False,
                    "form": "cfg!",
                    "expression": source[opening + 1 : end],
                }
            )
            line += source.count("\n", cursor, end + 1)
            cursor = end + 1
            continue
        skipped = _skip_non_code(source, cursor)
        if skipped is None:
            skipped = token.end()
        line += source.count("\n", cursor, skipped)
        cursor = skipped
    return constructs


def _forbidden_cfgs(path, source):
    offenders = []
    for construct in _cfg_constructs(source):
        expression = construct["expression"]
        compact = re.sub(r"\s+", "", expression)
        targets = re.findall(r'\btarget_os\s*=\s*"([^"]+)"', expression)
        forbidden = (
            re.search(r"\bwindows\b", expression)
            or re.search(r"\bnot\s*\(\s*unix\s*\)", expression)
            or any(target != "linux" for target in targets)
            or re.search(r'\btarget_env\s*=\s*"msvc"', expression)
            or re.search(r'\btarget_vendor\s*=\s*"apple"', expression)
            or re.search(
                r'\bnot\s*\(\s*target_os\s*=\s*"linux"\s*\)', expression
            )
        )
        allowed = (path, construct["line"], construct["form"], compact)
        if forbidden and allowed not in ALLOWED_NON_LINUX_CFG_ITEMS:
            offenders.append(
                f'{path}:{construct["line"]}:{construct["form"]}({compact})'
            )
    return offenders


class LinuxOnlySourceTests(unittest.TestCase):
    def test_non_linux_cfg_selectors_are_absent(self):
        offenders = []
        for path in _rust_files():
            offenders.extend(_forbidden_cfgs(_relative(path), path.read_text()))
        self.assertEqual(offenders, [], f"non-Linux cfg selectors re-entered src/tests: {offenders}")

    def test_unix_cfg_gates_stay_at_the_pre_b2_test_only_floor(self):
        actual = set()
        for path in _rust_files():
            source = path.read_text()
            lines = source.splitlines()
            for construct in _cfg_constructs(source):
                if re.search(r"\bunix\b", construct["expression"]):
                    item = "<file>" if construct["inner"] else _next_function(lines, construct["line"] - 1)
                    actual.add((_relative(path), item))
        self.assertEqual(
            actual,
            ALLOWED_UNIX_CFG_ITEMS,
            "Linux-only source gained or lost an undeclared cfg(unix) gate",
        )

    def test_cfg_scanner_catches_inner_macro_cfg_attr_and_non_linux_target(self):
        snippets = {
            "inner": "#![cfg(windows)]\nfn main() {}\n",
            "macro": "fn main() { let _ = cfg!(windows); }\n",
            "cfg_attr": "#[cfg_attr(windows, allow(dead_code))]\nfn main() {}\n",
            "target": 'fn main() { let _ = cfg!(target_os = "macos"); }\n',
            "char_quote": "const QUOTE: char = '\"';\n#[cfg(windows)]\nfn main() {}\n",
            "escaped_char_quote": "const QUOTE: char = '\\\"';\n#[cfg(windows)]\nfn main() {}\n",
            "byte_char_quote": "const QUOTE: u8 = b'\"';\n#[cfg(windows)]\nfn main() {}\n",
            "lifetime": "fn borrow<'a>(value: &'a str) -> &'a str { value }\n#[cfg(windows)]\nfn main() {}\n",
            "not_linux": '#[cfg(not(target_os = "linux"))]\nfn main() {}\n',
            "msvc": '#[cfg(target_env = "msvc")]\nfn main() {}\n',
            "apple": '#[cfg(target_vendor = "apple")]\nfn main() {}\n',
        }
        for name, source in snippets.items():
            with self.subTest(name=name):
                self.assertTrue(_forbidden_cfgs(f"{name}.rs", source))

    def test_cfg_scanner_ignores_comments_and_string_literals(self):
        source = '''
// #![cfg(windows)]
const TEXT: &str = "cfg!(target_os = \\"macos\\")";
/* #[cfg_attr(windows, allow(dead_code))] */
'''
        self.assertEqual(_forbidden_cfgs("ignored.rs", source), [])

    def test_cfg_attribute_parser_matches_every_line_start_attribute(self):
        pattern = re.compile(
            r"(?m)^[ \t]*#!?[ \t]*\[[ \t]*cfg(?:_attr)?[ \t]*\("
        )
        mismatches = []
        for path in _rust_files():
            source = path.read_text()
            plain = len(pattern.findall(source))
            parsed = sum(
                construct["form"] in {"cfg", "cfg_attr"}
                for construct in _cfg_constructs(source)
            )
            if plain != parsed:
                mismatches.append(f"{_relative(path)}: plain={plain}, parsed={parsed}")
        self.assertEqual(mismatches, [], f"cfg scanner lost attribute parity: {mismatches}")


if __name__ == "__main__":
    unittest.main()
