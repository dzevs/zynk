# Modified by the zynk project: this file differs from the upstream version it was derived from.
# See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
from __future__ import annotations

import hashlib
import re
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
HOT_PATH_SOURCES = (
    PROJECT_ROOT / "src" / "ui.rs",
    *sorted((PROJECT_ROOT / "src" / "ui").rglob("*.rs")),
    PROJECT_ROOT / "src" / "server" / "render_stream.rs",
)
APP_SERVER_SOURCES = (
    *sorted((PROJECT_ROOT / "src" / "app").rglob("*.rs")),
    *sorted((PROJECT_ROOT / "src" / "server").rglob("*.rs")),
)
HEADLESS_SOURCE = PROJECT_ROOT / "src" / "server" / "headless.rs"
TEST_MODULE = re.compile(r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+\w+\s*\{")
INPUT_STATE_CALL = re.compile(r"(?:\.|::)input_state\b")
KEYBOARD_STATE_ANSI_CALL = re.compile(
    r"(?:\.|::)(?:keyboard_state_ansi|kitty_keyboard_state_ansi)\b"
)
AGGREGATE_STATE_CALLS = (
    (INPUT_STATE_CALL, "aggregate terminal input state; add a narrow accessor"),
    (KEYBOARD_STATE_ANSI_CALL, "formatted keyboard state"),
)
FORBIDDEN_CALLS = (
    *AGGREGATE_STATE_CALLS,
    (
        re.compile(r"(?:\.|::)screen_text_snapshot\b"),
        "formatted terminal screen snapshot",
    ),
    (
        re.compile(r"\bforeground_job\s*\("),
        "process-tree inspection",
    ),
)
HEADLESS_RUN_FORBIDDEN_IDENTIFIERS = (
    "workspaces",
    "tabs",
    "panes",
    "pane_ids",
    "terminals",
    "terminal_runtimes",
    "HashSet",
)
ALT_SCREEN_BOUNDARY_FORBIDDEN_IDENTIFIERS = (
    "workspaces",
    "tabs",
    "panes",
    "pane_ids",
)
HEADLESS_RUN_SELF_METHODS = frozenset(
    {
        "accept_client_connections",
        "begin_shutdown_if_requested",
        "complete_shutdown",
        "drain_api_requests_with_render_impact",
        "drain_api_requests_with_shutdown_check",
        "drain_client_config_reload_request",
        "drain_internal_events_with_forwarding",
        "drain_server_events",
        "drain_server_events_with_render_impact",
        "expire_direct_graphics",
        "handle_api_request_with_render_impact",
        "handle_api_request_with_shutdown_check",
        "handle_deferred_requests_headless",
        "handle_internal_event_with_forwarding",
        "handle_scheduled_tasks_headless",
        "handle_server_event",
        "handle_server_event_with_render_impact",
        "has_app_client",
        "has_pending_presentation_work_with_graphics",
        "pane_graphics_runtime_active",
        "process_pending_alt_screen_reads",
        "pty_sources_visible_to_any_render_target",
        "render_and_stream",
        "render_retained_graphics_update_and_stream",
        "render_retained_pty_update_and_stream",
        "settle_event_selected_at_stop_boundary",
        "stream_host_keyboard_enhancement_flags",
        "stream_host_mouse_capture_mode",
        "sync_immediate_pty_sources",
        "sync_terminal_title_sources",
        "sync_window_title",
    }
)
HEADLESS_RUN_BODY_FINGERPRINT = (
    "d416d7b021315ca14942a0a27926e158defa859b54b89e6c9e61bd4589d405cb"
)
SELF_METHOD_CALL = re.compile(r"\bself\s*\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(")
ALT_SCREEN_MAINTENANCE_BODY = (
    "let completed_alt_screen_reads = self.poll_pending_alt_screen_reads(now); "
    "self.release_deferred_alt_screen_terminals(completed_alt_screen_reads)"
)
ALT_SCREEN_MAINTENANCE_BODY_FINGERPRINTS = {
    "process_pending_alt_screen_reads": (
        "49804d194c43c34ac7019e634789a4cf5993a2afec3eb71130673b363a104a19"
    ),
    "poll_pending_alt_screen_reads": (
        "0ff728fe18aad8d42c19f67676d43cef78abb9dbfdbf881bd6c2fca271818acb"
    ),
    "release_deferred_alt_screen_terminals": (
        "7eb159a7e81c97bd2d3d218765499dea38f0fc84a4623874a7ef955228032360"
    ),
    "release_deferred_alt_screen_terminals_with": (
        "d6f938076db5fbaa43842ea5d375c0676e147c2b313d37febcca48cba3d604e8"
    ),
    "take_ready_handoff": (
        "2e4d6a72054327045cca644728650c08cbcfb664befda5f1cecc03e3e01c5ff4"
    ),
}


def blank_non_newlines(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def mask_comments_and_literals(source: str) -> str:
    chars = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end == -1 else end
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth > 0:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source[index] == "r":
            quote = index + 1
            while quote < len(source) and source[quote] == "#":
                quote += 1
            if quote < len(source) and source[quote] == '"':
                suffix = '"' + "#" * (quote - index - 1)
                end = source.find(suffix, quote + 1)
                end = len(source) if end == -1 else end + len(suffix)
                blank_non_newlines(chars, index, end)
                index = end
                continue

        if source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            blank_non_newlines(chars, index, min(end, len(source)))
            index = end
            continue

        if source[index] == "'":
            end = index + 2
            if index + 1 < len(source) and source[index + 1] == "\\":
                end += 1
            if end < len(source) and source[end] == "'":
                end += 1
                blank_non_newlines(chars, index, end)
                index = end
                continue

        index += 1

    return "".join(chars)


def production_code(source: str) -> str:
    code = mask_comments_and_literals(source)
    chars = list(code)
    search_from = 0

    while test_module := TEST_MODULE.search(code, search_from):
        depth = 0
        end = test_module.end() - 1
        while end < len(code):
            if code[end] == "{":
                depth += 1
            elif code[end] == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            end += 1
        blank_non_newlines(chars, test_module.start(), end)
        code = "".join(chars)
        search_from = end

    return code


def find_violations(paths, rules) -> list[str]:
    violations: list[str] = []
    for path in paths:
        code = production_code(path.read_text(encoding="utf-8"))
        for pattern, description in rules:
            for match in pattern.finditer(code):
                line = code.count("\n", 0, match.start()) + 1
                relative_path = path.relative_to(PROJECT_ROOT)
                violations.append(f"{relative_path}:{line}: {description}")
    return violations


def rust_function_body(source: str, name: str) -> str:
    code = production_code(source)
    signature = re.compile(
        rf"\b(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+{re.escape(name)}\s*\("
    )
    matches = list(signature.finditer(code))
    if len(matches) != 1:
        raise AssertionError(f"expected one production fn {name}, found {len(matches)}")
    start = code.find("{", matches[0].end())
    if start == -1:
        raise AssertionError(f"production fn {name} has no body")
    depth = 0
    for end in range(start, len(code)):
        if code[end] == "{":
            depth += 1
        elif code[end] == "}":
            depth -= 1
            if depth == 0:
                return code[start + 1 : end]
    raise AssertionError(f"production fn {name} has an unterminated body")


def normalized_body_fingerprint(body: str) -> str:
    normalized = re.sub(r"\s+", "", body)
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def identifier_hits(body: str, identifiers) -> list[str]:
    return [
        identifier
        for identifier in identifiers
        if re.search(rf"\b{re.escape(identifier)}\b", body)
    ]


def headless_alt_screen_maintenance_violations(source: str) -> list[str]:
    run_body = rust_function_body(source, "run")
    boundary_bodies = {
        name: rust_function_body(source, name)
        for name in ALT_SCREEN_MAINTENANCE_BODY_FINGERPRINTS
    }
    helper_body = boundary_bodies["process_pending_alt_screen_reads"]
    violations = [
        f"run contains forbidden collection identifier {identifier}"
        for identifier in identifier_hits(run_body, HEADLESS_RUN_FORBIDDEN_IDENTIFIERS)
    ]
    run_fingerprint = normalized_body_fingerprint(run_body)
    if run_fingerprint != HEADLESS_RUN_BODY_FINGERPRINT:
        violations.append(
            "run body fingerprint changed: "
            f"expected {HEADLESS_RUN_BODY_FINGERPRINT}, got {run_fingerprint}"
        )
    run_self_methods = frozenset(SELF_METHOD_CALL.findall(run_body))
    for method in sorted(run_self_methods - HEADLESS_RUN_SELF_METHODS):
        violations.append(f"run contains unreviewed self method call {method}")
    for method in sorted(HEADLESS_RUN_SELF_METHODS - run_self_methods):
        violations.append(f"run is missing reviewed self method call {method}")
    helper_call = "self.process_pending_alt_screen_reads(now)"
    if run_body.count(helper_call) != 1:
        violations.append("run must call process_pending_alt_screen_reads(now) exactly once")
    if "poll_pending_alt_screen_reads(" in run_body:
        violations.append("run must not call poll_pending_alt_screen_reads directly")
    if "release_deferred_alt_screen_terminals(" in run_body:
        violations.append("run must not call release_deferred_alt_screen_terminals directly")
    if " ".join(helper_body.split()) != ALT_SCREEN_MAINTENANCE_BODY:
        violations.append("process_pending_alt_screen_reads must be exactly poll then release")
    for name, body in boundary_bodies.items():
        for identifier in identifier_hits(body, ALT_SCREEN_BOUNDARY_FORBIDDEN_IDENTIFIERS):
            violations.append(
                f"{name} contains forbidden collection identifier {identifier}"
            )
        expected = ALT_SCREEN_MAINTENANCE_BODY_FINGERPRINTS[name]
        actual = normalized_body_fingerprint(body)
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )
    return violations


class UiHotPathArchitectureTests(unittest.TestCase):
    def test_render_hot_paths_avoid_known_expensive_runtime_queries(self) -> None:
        violations = find_violations(HOT_PATH_SOURCES, FORBIDDEN_CALLS)

        self.assertEqual(
            violations,
            [],
            "Render/layout code must not perform pane-scaled expensive reads:\n"
            + "\n".join(violations),
        )

    def test_app_and_server_avoid_aggregate_terminal_state(self) -> None:
        self.assertTrue(APP_SERVER_SOURCES, "No app/server Rust sources were discovered")
        violations = find_violations(APP_SERVER_SOURCES, AGGREGATE_STATE_CALLS)

        self.assertEqual(
            violations,
            [],
            "App/server code must use narrow terminal-state accessors:\n"
            + "\n".join(violations),
        )

    def test_headless_alt_screen_maintenance_avoids_per_loop_layout_scans(self) -> None:
        violations = headless_alt_screen_maintenance_violations(
            HEADLESS_SOURCE.read_text(encoding="utf-8")
        )

        self.assertEqual(
            violations,
            [],
            "Headless alternate-screen maintenance must stay transition-only:\n"
            + "\n".join(violations),
        )

    def test_headless_guard_rejects_uninstrumented_reviewer_scan(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """            if self.process_pending_alt_screen_reads(now) {
"""
        replacement = """            let _live = self.app.state.workspaces.iter()
                .flat_map(|workspace| workspace.tabs.iter())
                .flat_map(|tab| tab.layout.pane_ids())
                .count();
            if self.process_pending_alt_screen_reads(now) {
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        violations = headless_alt_screen_maintenance_violations(source)
        self.assertTrue(
            any(
                violation.startswith("run body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

    def test_headless_guard_rejects_line_broken_direct_pane_iteration(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """            if self.process_pending_alt_screen_reads(now) {
"""
        replacement = """            let _live = self.app.state.workspaces
                .iter()
                .flat_map(|workspace| workspace.tabs
                    .iter())
                .flat_map(|tab| tab.panes
                    .values())
                .count();
            if self.process_pending_alt_screen_reads(now) {
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        violations = headless_alt_screen_maintenance_violations(source)
        self.assertTrue(
            any(
                violation.startswith("run body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

    def test_headless_guard_rejects_direct_pane_scan_inside_poll(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """    fn poll_pending_alt_screen_reads(&mut self, now: Instant) -> Vec<crate::terminal::TerminalId> {
        let pending = std::mem::take(&mut self.pending_alt_screen_reads);
"""
        replacement = """    fn poll_pending_alt_screen_reads(&mut self, now: Instant) -> Vec<crate::terminal::TerminalId> {
        let _live = self
            .app
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .count();
        let pending = std::mem::take(&mut self.pending_alt_screen_reads);
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        self.assertTrue(headless_alt_screen_maintenance_violations(source))

    def test_headless_guard_rejects_borrowed_collection_scan_in_run(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """            if self.process_pending_alt_screen_reads(now) {
"""
        replacement = """            for workspace in &self.app.state.workspaces {
                for tab in &workspace.tabs {
                    for _pane in &tab.panes {}
                }
            }
            if self.process_pending_alt_screen_reads(now) {
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        self.assertTrue(headless_alt_screen_maintenance_violations(source))

    def test_headless_guard_rejects_new_run_helper_call(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """            if self.process_pending_alt_screen_reads(now) {
"""
        replacement = """            self.sync_foreground_client_state();
            if self.process_pending_alt_screen_reads(now) {
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        self.assertTrue(headless_alt_screen_maintenance_violations(source))

    def test_headless_guard_rejects_associated_run_helper_call(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        run_needle = """            if self.process_pending_alt_screen_reads(now) {
"""
        run_replacement = """            Self::gate3_uninstrumented_run_helper(self);
            if self.process_pending_alt_screen_reads(now) {
"""
        helper_needle = """    fn process_pending_alt_screen_reads(&mut self, now: Instant) -> bool {
"""
        helper_replacement = """    fn gate3_uninstrumented_run_helper(&mut self) {
        let count = self
            .app
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .count();
        std::hint::black_box(count);
    }

    fn process_pending_alt_screen_reads(&mut self, now: Instant) -> bool {
"""
        self.assertEqual(source.count(run_needle), 1)
        self.assertEqual(source.count(helper_needle), 1)
        source = source.replace(run_needle, run_replacement, 1)
        source = source.replace(helper_needle, helper_replacement, 1)

        violations = headless_alt_screen_maintenance_violations(source)
        self.assertTrue(
            any(
                violation.startswith("run body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

    def test_headless_guard_rejects_qualified_run_helper_call(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        run_needle = """            if self.process_pending_alt_screen_reads(now) {
"""
        run_replacement = """            HeadlessServer::gate3_uninstrumented_run_helper(self);
            if self.process_pending_alt_screen_reads(now) {
"""
        helper_needle = """    fn process_pending_alt_screen_reads(&mut self, now: Instant) -> bool {
"""
        helper_replacement = """    fn gate3_uninstrumented_run_helper(&mut self) {
        let count = self
            .app
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .count();
        std::hint::black_box(count);
    }

    fn process_pending_alt_screen_reads(&mut self, now: Instant) -> bool {
"""
        self.assertEqual(source.count(run_needle), 1)
        self.assertEqual(source.count(helper_needle), 1)
        source = source.replace(run_needle, run_replacement, 1)
        source = source.replace(helper_needle, helper_replacement, 1)

        violations = headless_alt_screen_maintenance_violations(source)
        self.assertTrue(
            any(
                violation.startswith("run body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

    def test_headless_guard_rejects_new_poll_helper_call(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """    fn poll_pending_alt_screen_reads(&mut self, now: Instant) -> Vec<crate::terminal::TerminalId> {
        let pending = std::mem::take(&mut self.pending_alt_screen_reads);
"""
        replacement = """    fn poll_pending_alt_screen_reads(&mut self, now: Instant) -> Vec<crate::terminal::TerminalId> {
        self.sync_foreground_client_state();
        let pending = std::mem::take(&mut self.pending_alt_screen_reads);
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        self.assertTrue(headless_alt_screen_maintenance_violations(source))

    def test_scanner_ignores_non_production_references(self) -> None:
        source = '''
// runtime.input_state()
const EXAMPLE: &str = "runtime.input_state()";
#[cfg(test)]
mod tests {
    fn aggregate_state_test() { runtime.input_state(); }
}
fn production_after_tests() {}
'''
        code = production_code(source)
        self.assertNotRegex(code, FORBIDDEN_CALLS[0][0])
        self.assertIn("fn production_after_tests()", code)
        self.assertEqual(code.count("\n"), source.count("\n"))

    def test_scanner_checks_production_after_test_modules(self) -> None:
        source = '''
#[cfg(test)]
mod tests {
    const BRACES: &str = "}}";
}
fn render() { TerminalRuntime::input_state; }
'''
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[0][0])

    def test_scanner_catches_each_aggregate_state_call(self) -> None:
        cases = (
            ("fn render() { runtime.input_state(); }", INPUT_STATE_CALL),
            ("fn render() { runtime.keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
            ("fn render() { runtime.kitty_keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
        )
        for source, pattern in cases:
            with self.subTest(source=source):
                self.assertRegex(production_code(source), pattern)

    def test_scanner_catches_imported_process_query(self) -> None:
        source = "fn render() { foreground_job(pid); }"
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[3][0])


if __name__ == "__main__":
    unittest.main()
