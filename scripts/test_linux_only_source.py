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


class LinuxOnlySourceTests(unittest.TestCase):
    def test_non_linux_cfg_selectors_are_absent(self):
        offenders = []
        for path in _rust_files():
            for line_number, line in enumerate(path.read_text().splitlines(), 1):
                compact = re.sub(r"\s+", "", line)
                if "#[cfg" not in compact:
                    continue
                target_os = re.search(r'target_os="([^"]+)"', compact)
                if (
                    re.search(r"\bwindows\b", line)
                    or "not(unix)" in compact
                    or (target_os is not None and target_os.group(1) != "linux")
                ):
                    offenders.append(f"{_relative(path)}:{line_number}:{line.strip()}")
        self.assertEqual(offenders, [], f"non-Linux cfg selectors re-entered src/tests: {offenders}")

    def test_unix_cfg_gates_stay_at_the_pre_b2_test_only_floor(self):
        actual = set()
        for path in _rust_files():
            lines = path.read_text().splitlines()
            for index, line in enumerate(lines):
                compact = re.sub(r"\s+", "", line)
                if "#[cfg" in compact and re.search(r"\bunix\b", line):
                    actual.add((_relative(path), _next_function(lines, index)))
        self.assertEqual(
            actual,
            ALLOWED_UNIX_CFG_ITEMS,
            "Linux-only source gained or lost an undeclared cfg(unix) gate",
        )


if __name__ == "__main__":
    unittest.main()
