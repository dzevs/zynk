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
RENDER_STREAM_SOURCE = PROJECT_ROOT / "src" / "server" / "render_stream.rs"
APP_RUNTIME_SOURCE = PROJECT_ROOT / "src" / "app" / "runtime.rs"
UI_SOURCE = PROJECT_ROOT / "src" / "ui.rs"
STATUS_SOURCE = PROJECT_ROOT / "src" / "ui" / "status.rs"
SIDEBAR_SOURCE = PROJECT_ROOT / "src" / "ui" / "sidebar.rs"
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
        "handle_scheduled_tasks_headless_with_impact",
        "handle_server_event",
        "handle_server_event_with_render_impact",
        "has_app_client",
        "has_pending_presentation_work_with_graphics",
        "pane_graphics_runtime_active",
        "process_pending_alt_screen_reads",
        "pty_sources_visible_to_any_render_target",
        "render_and_stream",
        "render_retained_animation_update_and_stream",
        "render_retained_graphics_update_and_stream",
        "render_retained_pty_animation_update_and_stream",
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
    "e9584bc8edb0f7ff9c2c881b5a8c845a92bd3d47fcaf40cb69e64f06ae9d3a19"
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
WORKING_ANIMATION_BODY_FINGERPRINTS = {
    "handle_scheduled_tasks_headless_with_impact": (
        "997e33d12d945f4cb83daec6fff1751cb4397291c09eb6a2beb8ffab57ab0668"
    ),
    "render_retained_animation_update_and_stream": (
        "b82a678c41498c5ab3030286522883d6a9b094b27e8ec74f8049b2986966dbc4"
    ),
    "retained_animation_preflight": (
        "a17e07740ff6a2afae3727d0f5848c23a63b953faff161ee545e1ba74cc37357"
    ),
    "render_retained_pty_update_and_stream_with_animation": (
        "a4d61bb2e6c3fdb40836bd22db09f44bb331507918c04867a2b1626804bd15ea"
    ),
}
WORKING_ANIMATION_RENDER_STREAM_FINGERPRINTS = {
    "collect_working_animation_cells": (
        "f0aa6bcc6fb0b455e00e827c1a1b81afe65a9b9e2aabd26079ba7b8266a0a01c"
    ),
    "collect_working_shimmer_cells": (
        "732d67067976509087db6a255b624de74c86e54a4330b835519af1b8eee8d2dd"
    ),
    "can_apply": (
        "ec5240ffed4c97a5b232bd7cd9542061d02d497443bc8e81a8b856afe979d42e"
    ),
    "apply_working_animation_cells": (
        "5da61dd0a0679ca2db7e62fa7e5d9b19bf1b90bf2d9c5e597bd44cf6f526e486"
    ),
}
WORKING_ANIMATION_APP_RUNTIME_FINGERPRINTS = {
    "tick_working_animation": (
        "d534d6ad22e146eb582b37beac6cc2cfa5f00e2813d66bf519de23e3efd70628"
    ),
}
WORKING_ANIMATION_UI_FINGERPRINTS = {
    "computed_working_animation_demand": (
        "c69e7fbb09fe76b3d64835875bd01ecd9a3cb6a99c855fcc9dea545ed0805100"
    ),
}
WORKING_ANIMATION_STATUS_FINGERPRINTS = {
    "working_label_shimmer_palette": (
        "86d3c92105b56e30232fe52f344cc4f79889b57d7e5e99b83715c4311bcdebdc"
    ),
    "working_label_shimmer_color": (
        "51f0e483b3855a768d2642f62d04e5820135cf5e27f89ac5ff6ef584def33ce4"
    ),
    "working_label_shimmer_weight": (
        "8ebb87ba9847249d91daecf7b0d185f904b22c5a42b161d2ef2bfb178e82a543"
    ),
    "working_label_spans": (
        "d0b7fcee77a1d671b74cf74504c5a1bf7d505fcd3562042308dd1084d5410efc"
    ),
    "blend_quarters": (
        "fb06b28d9003480653577686b27ceda1317096f4cc9590ae28bbf5d2375e0b84"
    ),
    "resolve_shimmer_color": (
        "b361abd279bb7e22794239ba682ee0c2d88f03673eae177d9c8fc52eae8021c4"
    ),
}
WORKING_ANIMATION_SIDEBAR_FINGERPRINTS = {
    "agent_entry_has_working_shimmer": (
        "5e027cd77df2083c61308dfb5b1ab2a61fc0d3a25552d2c32315cd463e2b388e"
    ),
}
WORKING_ANIMATION_RETAINED_FORBIDDEN = (
    (re.compile(r"\b(?:workspaces|tabs|panes|pane_ids|terminals|terminal_runtimes)\b"), "layout or terminal collection scan"),
    (re.compile(r"\brender_and_stream\s*\("), "generic full render"),
    (re.compile(r"\b(?:std::)?fs\s*::"), "filesystem access"),
    (re.compile(r"\b(?:std::process::)?Command\s*::"), "process launch or inspection"),
    (re.compile(r"\bforeground_job\s*\("), "process-tree inspection"),
    (re.compile(r"\b(?:Vec|HashMap|HashSet|BTreeMap|BTreeSet)\s*::\s*new\s*\("), "fresh aggregate collection"),
    (re.compile(r"\.collect\s*::\s*<\s*(?:Vec|HashMap|HashSet|BTreeMap|BTreeSet)"), "fresh aggregate collection"),
    (re.compile(r"\bvec\s*!"), "vec! allocation"),
    (re.compile(r"\bformat\s*!"), "format! allocation"),
    (re.compile(r"\.collect\s*\(\s*\)"), "untyped collect allocation"),
    (re.compile(r"\.to_vec\s*\(\s*\)"), "to_vec allocation"),
    (re.compile(r"\bwith_capacity\s*\("), "with_capacity allocation"),
    (re.compile(r"\.cloned\s*\(\s*\)"), "cloned allocation"),
)
FRAME_OWNERSHIP_CLONE = re.compile(
    r"client\s*\.\s*render_state\s*\.\s*last_frame\s*\(\s*\)\s*\.\s*cloned\s*\(\s*\)"
)


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


def headless_working_animation_violations(
    headless_source: str,
    render_stream_source: str,
    app_runtime_source: str | None = None,
    ui_source: str | None = None,
    status_source: str | None = None,
    sidebar_source: str | None = None,
) -> list[str]:
    violations: list[str] = []
    headless_bodies = {
        name: rust_function_body(headless_source, name)
        for name in WORKING_ANIMATION_BODY_FINGERPRINTS
    }
    render_stream_bodies = {
        name: rust_function_body(render_stream_source, name)
        for name in WORKING_ANIMATION_RENDER_STREAM_FINGERPRINTS
    }
    app_runtime_source = (
        APP_RUNTIME_SOURCE.read_text(encoding="utf-8")
        if app_runtime_source is None
        else app_runtime_source
    )
    ui_source = UI_SOURCE.read_text(encoding="utf-8") if ui_source is None else ui_source
    status_source = (
        STATUS_SOURCE.read_text(encoding="utf-8")
        if status_source is None
        else status_source
    )
    sidebar_source = (
        SIDEBAR_SOURCE.read_text(encoding="utf-8")
        if sidebar_source is None
        else sidebar_source
    )
    app_runtime_bodies = {
        name: rust_function_body(app_runtime_source, name)
        for name in WORKING_ANIMATION_APP_RUNTIME_FINGERPRINTS
    }
    ui_bodies = {
        name: rust_function_body(ui_source, name)
        for name in WORKING_ANIMATION_UI_FINGERPRINTS
    }
    status_bodies = {
        name: rust_function_body(status_source, name)
        for name in WORKING_ANIMATION_STATUS_FINGERPRINTS
    }
    sidebar_bodies = {
        name: rust_function_body(sidebar_source, name)
        for name in WORKING_ANIMATION_SIDEBAR_FINGERPRINTS
    }

    for name, expected in WORKING_ANIMATION_BODY_FINGERPRINTS.items():
        actual = normalized_body_fingerprint(headless_bodies[name])
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )
    for name, expected in WORKING_ANIMATION_RENDER_STREAM_FINGERPRINTS.items():
        actual = normalized_body_fingerprint(render_stream_bodies[name])
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )
    for name, expected in WORKING_ANIMATION_APP_RUNTIME_FINGERPRINTS.items():
        actual = normalized_body_fingerprint(app_runtime_bodies[name])
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )
    for name, expected in WORKING_ANIMATION_UI_FINGERPRINTS.items():
        actual = normalized_body_fingerprint(ui_bodies[name])
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )
    for name, expected in WORKING_ANIMATION_STATUS_FINGERPRINTS.items():
        actual = normalized_body_fingerprint(status_bodies[name])
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )
    for name, expected in WORKING_ANIMATION_SIDEBAR_FINGERPRINTS.items():
        actual = normalized_body_fingerprint(sidebar_bodies[name])
        if actual != expected:
            violations.append(
                f"{name} body fingerprint changed: expected {expected}, got {actual}"
            )

    retained_bodies = {
        "render_retained_animation_update_and_stream": headless_bodies[
            "render_retained_animation_update_and_stream"
        ],
        "retained_animation_preflight": headless_bodies[
            "retained_animation_preflight"
        ],
        "apply_working_animation_cells": render_stream_bodies[
            "apply_working_animation_cells"
        ],
        "working_animation_cell_can_apply": render_stream_bodies["can_apply"],
        "working_label_shimmer_color": status_bodies["working_label_shimmer_color"],
        "working_label_shimmer_weight": status_bodies["working_label_shimmer_weight"],
        "working_label_blend_quarters": status_bodies["blend_quarters"],
    }
    frame_clone_body = retained_bodies["render_retained_animation_update_and_stream"]
    frame_clone_matches = list(FRAME_OWNERSHIP_CLONE.finditer(frame_clone_body))
    if len(frame_clone_matches) != 1:
        violations.append(
            "render_retained_animation_update_and_stream must contain exactly one "
            "reviewed protocol-frame ownership clone"
        )
    elif len(re.findall(r"\.cloned\s*\(\s*\)", frame_clone_body)) != 1:
        violations.append(
            "render_retained_animation_update_and_stream contains an unreviewed cloned allocation"
        )
    retained_bodies["render_retained_animation_update_and_stream"] = (
        FRAME_OWNERSHIP_CLONE.sub("reviewed_frame_ownership_clone", frame_clone_body, count=1)
    )
    for name, body in retained_bodies.items():
        for pattern, description in WORKING_ANIMATION_RETAINED_FORBIDDEN:
            if pattern.search(body):
                violations.append(f"{name} contains {description}")
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

    def test_headless_working_animation_boundary_is_reviewed_and_narrow(self) -> None:
        violations = headless_working_animation_violations(
            HEADLESS_SOURCE.read_text(encoding="utf-8"),
            RENDER_STREAM_SOURCE.read_text(encoding="utf-8"),
        )

        self.assertEqual(
            violations,
            [],
            "Headless working animation must stay allocation-free and chrome-only:\n"
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

    def test_animation_guard_rejects_full_render_and_layout_scan(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """    fn render_retained_animation_update_and_stream(&mut self) -> bool {
        crate::render_prof::event("retained_animation.attempt");
"""
        replacement = """    fn render_retained_animation_update_and_stream(&mut self) -> bool {
        self.render_and_stream();
        let _ = self.app.state.workspaces.iter().flat_map(|workspace| &workspace.tabs).count();
        crate::render_prof::event("retained_animation.attempt");
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        violations = headless_working_animation_violations(
            source, RENDER_STREAM_SOURCE.read_text(encoding="utf-8")
        )
        self.assertTrue(
            any("generic full render" in violation for violation in violations), violations
        )
        self.assertTrue(
            any("layout or terminal collection scan" in violation for violation in violations),
            violations,
        )

    def test_animation_guard_rejects_io_and_fresh_collections(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """    fn render_retained_animation_update_and_stream(&mut self) -> bool {
        crate::render_prof::event("retained_animation.attempt");
"""
        replacement = """    fn render_retained_animation_update_and_stream(&mut self) -> bool {
        let _scratch = Vec::new();
        let _ = std::fs::read("/proc/self/stat");
        crate::render_prof::event("retained_animation.attempt");
"""
        self.assertEqual(source.count(needle), 1)
        source = source.replace(needle, replacement, 1)

        violations = headless_working_animation_violations(
            source, RENDER_STREAM_SOURCE.read_text(encoding="utf-8")
        )
        self.assertTrue(
            any("fresh aggregate collection" in violation for violation in violations),
            violations,
        )
        self.assertTrue(
            any("filesystem access" in violation for violation in violations), violations
        )

    def test_animation_guard_rejects_every_unreviewed_allocation_form(self) -> None:
        source = HEADLESS_SOURCE.read_text(encoding="utf-8")
        needle = """    fn render_retained_animation_update_and_stream(&mut self) -> bool {
        crate::render_prof::event("retained_animation.attempt");
"""
        cases = (
            ("let _ = vec![1_u8];", "vec! allocation"),
            ("let _ = format!(\"{}\", 1);", "format! allocation"),
            ("let _: Vec<_> = [1_u8].into_iter().collect();", "untyped collect allocation"),
            ("let _ = [1_u8].to_vec();", "to_vec allocation"),
            ("let _ = Vec::<u8>::with_capacity(1);", "with_capacity allocation"),
            ("let _ = Some(String::new()).as_ref().cloned();", "cloned allocation"),
        )
        self.assertEqual(source.count(needle), 1)
        for statement, expected in cases:
            with self.subTest(statement=statement):
                changed = source.replace(
                    needle,
                    needle + f"        {statement}\n",
                    1,
                )
                violations = headless_working_animation_violations(
                    changed, RENDER_STREAM_SOURCE.read_text(encoding="utf-8")
                )
                self.assertTrue(
                    any(expected in violation for violation in violations), violations
                )

    def test_animation_guard_fingerprints_scheduler_and_patch_helpers(self) -> None:
        headless = HEADLESS_SOURCE.read_text(encoding="utf-8")
        scheduler_needle = "let animation_changed = self.app.tick_working_animation(now);"
        self.assertEqual(headless.count(scheduler_needle), 1)
        changed_headless = headless.replace(
            scheduler_needle,
            "let animation_changed = false;",
            1,
        )
        violations = headless_working_animation_violations(
            changed_headless, RENDER_STREAM_SOURCE.read_text(encoding="utf-8")
        )
        self.assertTrue(
            any(
                violation.startswith(
                    "handle_scheduled_tasks_headless_with_impact body fingerprint changed:"
                )
                for violation in violations
            ),
            violations,
        )

        runtime = APP_RUNTIME_SOURCE.read_text(encoding="utf-8")
        tick_needle = ".wrapping_add(super::WORKING_ANIMATION_TICK_STEP);"
        self.assertEqual(runtime.count(tick_needle), 1)
        changed_runtime = runtime.replace(tick_needle, ".wrapping_add(0);", 1)
        violations = headless_working_animation_violations(
            headless,
            RENDER_STREAM_SOURCE.read_text(encoding="utf-8"),
            app_runtime_source=changed_runtime,
        )
        self.assertTrue(
            any(
                violation.startswith("tick_working_animation body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

        ui_source = UI_SOURCE.read_text(encoding="utf-8")
        demand_needle = "if !app.working_animation"
        self.assertEqual(ui_source.count(demand_needle), 1)
        changed_ui = ui_source.replace(demand_needle, "if false && !app.working_animation", 1)
        violations = headless_working_animation_violations(
            headless,
            RENDER_STREAM_SOURCE.read_text(encoding="utf-8"),
            ui_source=changed_ui,
        )
        self.assertTrue(
            any(
                violation.startswith(
                    "computed_working_animation_demand body fingerprint changed:"
                )
                for violation in violations
            ),
            violations,
        )

        render_stream = RENDER_STREAM_SOURCE.read_text(encoding="utf-8")
        patch_needle = "cell.symbol.push_str(symbol);"
        self.assertEqual(render_stream.count(patch_needle), 1)
        changed_render_stream = render_stream.replace(
            patch_needle,
            "cell.symbol.push_str(\"?\");",
            1,
        )
        violations = headless_working_animation_violations(
            headless, changed_render_stream
        )
        self.assertTrue(
            any(
                violation.startswith(
                    "apply_working_animation_cells body fingerprint changed:"
                )
                for violation in violations
            ),
            violations,
        )

        shimmer_collect_needle = "let width = usize::from(rendered.area.width);"
        self.assertEqual(render_stream.count(shimmer_collect_needle), 1)
        changed_render_stream = render_stream.replace(
            shimmer_collect_needle,
            "let width = 0usize;",
            1,
        )
        violations = headless_working_animation_violations(
            headless, changed_render_stream
        )
        self.assertTrue(
            any(
                violation.startswith(
                    "collect_working_shimmer_cells body fingerprint changed:"
                )
                for violation in violations
            ),
            violations,
        )

        status_source = STATUS_SOURCE.read_text(encoding="utf-8")
        shimmer_period_needle = "% 10) as usize"
        self.assertEqual(status_source.count(shimmer_period_needle), 1)
        changed_status = status_source.replace(
            shimmer_period_needle,
            "% 9) as usize",
            1,
        )
        violations = headless_working_animation_violations(
            headless,
            render_stream,
            status_source=changed_status,
        )
        self.assertTrue(
            any(
                violation.startswith(
                    "working_label_shimmer_weight body fingerprint changed:"
                )
                for violation in violations
            ),
            violations,
        )

        blend_needle = "+ u16::from(target) * weight + 2;"
        self.assertEqual(status_source.count(blend_needle), 1)
        changed_status = status_source.replace(
            blend_needle,
            "+ u16::from(target) * weight;",
            1,
        )
        violations = headless_working_animation_violations(
            headless,
            render_stream,
            status_source=changed_status,
        )
        self.assertTrue(
            any(
                violation.startswith("blend_quarters body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

        resolve_needle = "Color::Reset => return host.foreground,"
        self.assertEqual(status_source.count(resolve_needle), 1)
        changed_status = status_source.replace(
            resolve_needle,
            "Color::Reset => return None,",
            1,
        )
        violations = headless_working_animation_violations(
            headless,
            render_stream,
            status_source=changed_status,
        )
        self.assertTrue(
            any(
                violation.startswith("resolve_shimmer_color body fingerprint changed:")
                for violation in violations
            ),
            violations,
        )

        sidebar_source = SIDEBAR_SOURCE.read_text(encoding="utf-8")
        sidebar_needle = ".rows_for_agent(entry.agent)"
        self.assertEqual(sidebar_source.count(sidebar_needle), 1)
        changed_sidebar = sidebar_source.replace(
            sidebar_needle,
            ".rows_for_agent(None)",
            1,
        )
        violations = headless_working_animation_violations(
            headless,
            render_stream,
            sidebar_source=changed_sidebar,
        )
        self.assertTrue(
            any(
                violation.startswith(
                    "agent_entry_has_working_shimmer body fingerprint changed:"
                )
                for violation in violations
            ),
            violations,
        )

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
