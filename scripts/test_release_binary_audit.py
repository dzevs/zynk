"""Guard for `scripts/release_binary_audit.py` — the release-binary self-update audit.

The audit itself needs a built release binary, which `just check` must never produce (a release build
is neither hermetic nor fast). So this drives the audit against tiny shell-script "binaries": one
that behaves like a correctly gated release build, and several that reproduce the exact regressions
the audit exists to catch — a leaked debug-only env var name, a leaked update URL constant, an
updater that succeeds, and one that reaches a downloader only when `ZYNK_FAKE_UPDATE_VERSION` is set
(Gate-3 B1 `WARDEN-R14-SOURCE-ONLY-BYPASS-001`). The real binary is audited by `just release-audit`
during release verification.

unittest style (run via `python3 -m unittest`)."""

import os
import pathlib
import stat
import tempfile
import unittest

from scripts import release_binary_audit as audit

FAIL_CLOSED_MESSAGE = "zynk update is not available yet: build from source"

# A correctly gated release build: fails closed, never runs a downloader, no debug seam strings.
GOOD_BINARY = f"""#!/bin/sh
printf '%s\\n' "{FAIL_CLOSED_MESSAGE}" >&2
exit 1
"""

# The gate is closed, but the retired runtime override reopens it — the regression under audit.
OVERRIDE_REOPENS_BINARY = f"""#!/bin/sh
if [ -n "${{ZYNK_FAKE_UPDATE_VERSION:-}}" ]; then
  curl -sfL https://example.invalid/latest.json
  printf 'update failed\\n' >&2
  exit 1
fi
printf '%s\\n' "{FAIL_CLOSED_MESSAGE}" >&2
exit 1
"""

# Fails closed but still runs the downloader first: no fetch may be attempted at all.
FETCHES_ANYWAY_BINARY = f"""#!/bin/sh
curl -sfL https://example.invalid/latest.json
printf '%s\\n' "{FAIL_CLOSED_MESSAGE}" >&2
exit 1
"""

SUCCEEDS_BINARY = """#!/bin/sh
printf 'updated to 9.9.9\\n'
exit 0
"""

# Records the environment of every run it is given, so the sanitization can be asserted per case.
ENV_DUMP_SEPARATOR = "=== run ==="
ENV_DUMP_BINARY = f"""#!/bin/sh
{{ printf '%s\\n' "{ENV_DUMP_SEPARATOR}"; env; }} >> "$AUDIT_ENV_DUMP"
printf '%s\\n' "{FAIL_CLOSED_MESSAGE}" >&2
exit 1
"""

FAKE_SOURCE = """
const STABLE_UPDATE_MANIFEST_URL: &str = "https://zynk.example/latest.json";
const PREVIEW_UPDATE_MANIFEST_URL: &str = "https://zynk.example/preview.json";
const NOT_A_URL: &str = "zynk update";
"""


def _write_binary(directory, name, body):
    path = pathlib.Path(directory) / name
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _write_source_root(directory, source=FAKE_SOURCE):
    root = pathlib.Path(directory) / "src-root"
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "src" / "update.rs").write_text(source, encoding="utf-8")
    return root


class UrlConstantParsingTests(unittest.TestCase):
    def test_only_http_url_constants_are_collected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = _write_source_root(tmp)
            self.assertEqual(
                audit.url_constants(root),
                {
                    "STABLE_UPDATE_MANIFEST_URL": "https://zynk.example/latest.json",
                    "PREVIEW_UPDATE_MANIFEST_URL": "https://zynk.example/preview.json",
                },
            )

    def test_the_real_checkout_still_declares_url_constants(self):
        # If the updater's URL constants are ever renamed away, the strings check would silently
        # become vacuous. The audit reports that as a failure; this pins the parser to reality.
        self.assertTrue(audit.url_constants(audit.ROOT), "no URL constants found in the checkout")


class ExtractStringsTests(unittest.TestCase):
    def test_printable_runs_are_recovered_from_binary_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "blob"
            path.write_bytes(b"\x00\x01ZYNK_FAKE_UPDATE_VERSION\x00ab\x00tail-string\xff")
            found = audit.extract_strings(path)
            self.assertIn("ZYNK_FAKE_UPDATE_VERSION", found)
            self.assertIn("tail-string", found)
            self.assertNotIn("ab", found, "runs shorter than the minimum must be dropped")


class AuditTests(unittest.TestCase):
    def _audit(self, body, name="zynk", source=FAKE_SOURCE):
        with tempfile.TemporaryDirectory() as tmp:
            binary = _write_binary(tmp, name, body)
            root = _write_source_root(tmp, source)
            return audit.audit(binary, root)

    def test_a_correctly_gated_binary_passes(self):
        failures, _ = self._audit(GOOD_BINARY)
        self.assertEqual(failures, [], failures)

    def test_a_leaked_debug_env_name_fails(self):
        leaky = GOOD_BINARY + "# leftover: ZYNK_FAKE_UPDATE_VERSION\n"
        failures, _ = self._audit(leaky)
        self.assertTrue(
            any("ZYNK_FAKE_UPDATE_VERSION" in f for f in failures),
            f"the leaked env var name must fail the audit: {failures}",
        )

    def test_a_leaked_peer_trust_seam_name_fails(self):
        # ADR 0014 fix #18 could not prove this from the source alone; the audit closes it.
        leaky = GOOD_BINARY + "# leftover: ZYNK_TEST_TRUST_PEER_PID\n"
        failures, _ = self._audit(leaky)
        self.assertTrue(
            any("ZYNK_TEST_TRUST_PEER_PID" in f for f in failures),
            f"the leaked peer-trust seam name must fail the audit: {failures}",
        )

    def test_a_leaked_update_url_constant_fails(self):
        leaky = GOOD_BINARY + "# leftover: https://zynk.example/latest.json\n"
        failures, _ = self._audit(leaky)
        self.assertTrue(
            any("STABLE_UPDATE_MANIFEST_URL" in f for f in failures),
            f"the leaked manifest URL must fail the audit: {failures}",
        )

    def test_a_source_root_without_url_constants_is_reported_as_vacuous(self):
        failures, _ = self._audit(GOOD_BINARY, source="// nothing here\n")
        self.assertTrue(
            any("vacuous" in f for f in failures),
            f"an empty URL-constant set must not pass silently: {failures}",
        )

    def test_an_updater_that_reaches_the_downloader_fails(self):
        failures, _ = self._audit(FETCHES_ANYWAY_BINARY)
        self.assertTrue(
            any("invoked the PATH-local downloader" in f for f in failures),
            f"a fetch attempt must fail the audit: {failures}",
        )

    def test_a_runtime_override_that_reopens_the_updater_fails(self):
        failures, _ = self._audit(OVERRIDE_REOPENS_BINARY)
        self.assertTrue(
            any("ZYNK_FAKE_UPDATE_VERSION=9.9.9" in f for f in failures),
            f"the override case must be the one that fails: {failures}",
        )
        self.assertFalse(
            any("sanitized environment" in f for f in failures),
            f"the sanitized case must still pass: {failures}",
        )

    def test_an_updater_that_succeeds_fails_the_audit(self):
        failures, _ = self._audit(SUCCEEDS_BINARY)
        self.assertTrue(
            any("must fail closed" in f for f in failures),
            f"a successful self-update must fail the audit: {failures}",
        )

    def test_missing_fail_closed_message_fails(self):
        silent = "#!/bin/sh\nexit 1\n"
        failures, _ = self._audit(silent)
        self.assertTrue(
            any("did not print the fail-closed message" in f for f in failures),
            f"a silent failure must not count as failing closed: {failures}",
        )


class EnvironmentSanitizationTests(unittest.TestCase):
    def test_inherited_zynk_variables_do_not_reach_the_audited_binary(self):
        planted = {
            "ZYNK_FAKE_UPDATE_VERSION": "8.8.8",
            "ZYNK_SOCKET_PATH": "/run/live/zynk.sock",
            "ZYNK_HOME": "/home/live/.zynk",
        }
        previous = {k: os.environ.get(k) for k in planted}
        os.environ.update(planted)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                dump = pathlib.Path(tmp) / "env-dump"
                os.environ["AUDIT_ENV_DUMP"] = str(dump)
                binary = _write_binary(tmp, "zynk", ENV_DUMP_BINARY)
                report = []
                failures = audit.check_update_fails_closed(binary, report)
                self.assertEqual(failures, [], failures)
                runs = [
                    dict(
                        line.split("=", 1)
                        for line in block.splitlines()
                        if "=" in line and not line.startswith(ENV_DUMP_SEPARATOR)
                    )
                    for block in dump.read_text(encoding="utf-8").split(ENV_DUMP_SEPARATOR)
                    if block.strip()
                ]
        finally:
            for key, value in previous.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
            os.environ.pop("AUDIT_ENV_DUMP", None)

        self.assertEqual(len(runs), 2, f"expected one environment per audited run: {runs}")
        sanitized, with_override = runs
        self.assertNotIn(
            "ZYNK_FAKE_UPDATE_VERSION",
            sanitized,
            "the inherited override must be stripped before the sanitized run",
        )
        self.assertEqual(
            with_override.get("ZYNK_FAKE_UPDATE_VERSION"),
            "9.9.9",
            "the second run must carry the audit's own override, not the inherited 8.8.8",
        )
        for run in runs:
            self.assertNotEqual(
                run.get("ZYNK_SOCKET_PATH"),
                "/run/live/zynk.sock",
                "the audit must not let the live socket path through",
            )
            self.assertNotEqual(
                run.get("ZYNK_HOME"),
                "/home/live/.zynk",
                "the audit must not let the live ZYNK_HOME through",
            )


if __name__ == "__main__":
    unittest.main()
