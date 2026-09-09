"""Release-binary audit: prove a BUILT zynk binary has no self-update surface left in it.

ADR 0013 makes zynk source-only on Linux x86_64, and the updater fails closed
(`ZYNK_RELEASE_INFRA_AVAILABLE = false`). Source-level `#[cfg(debug_assertions)]` gating and unit
tests say what the compiler *should* do; this script checks the artifact that actually ships
(Gate-3 B1 `WARDEN-R14-SOURCE-ONLY-BYPASS-001`). It also closes the one condition the ADR 0014
peer-trust seam (`ZYNK_TEST_TRUST_PEER_PID`) could not verify from the source alone: that the
debug-only seam is absent from the built release binary.

Two independent checks:

1. Strings. Neither the debug-only env var names nor any update manifest/asset URL constant may
   appear in the binary. The URL constants are parsed out of the Rust sources at audit time, so
   adding a new one cannot slip past a hard-coded list.
2. Behaviour. The binary is run as `zynk update` twice — once with an environment SANITIZED of every
   `ZYNK_*` variable except the isolated ones this script sets, and once with
   `ZYNK_FAKE_UPDATE_VERSION=9.9.9` on top — each time with a PATH-local fake `curl` that writes a
   marker file. Both runs must fail closed, and the marker must never be written: no network fetch
   was even attempted.

The real-binary run belongs to the release verification, not to `just check`. Run it with
`just release-audit`, which builds `--release --locked` into the isolated target and audits the
result. `just check` runs only the hermetic fixture-based unittest
(`scripts/test_release_binary_audit.py`), which never builds anything.

Usage:
    python3 scripts/release_binary_audit.py <path-to-release-binary> [--source-root DIR]
"""

import argparse
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]

# Debug-only seams. Each is compiled behind `#[cfg(debug_assertions)]`; a release binary must not
# even contain the name, so no environment can reach the behaviour behind it.
FORBIDDEN_ENV_NAMES = (
    "ZYNK_FAKE_UPDATE_VERSION",
    "ZYNK_FAKE_UPDATE_NOTES_VERSION",
    # ADR 0014 peer-trust test seam (src/api/mod.rs).
    "ZYNK_TEST_TRUST_PEER_PID",
)

# Sources whose `const …_URL: &str = "https://…";` declarations must not survive into the binary.
URL_CONST_SOURCES = ("src/update.rs", "src/remote/unix.rs")
URL_CONST_RE = re.compile(r'const\s+([A-Z0-9_]*URL)\s*:\s*&str\s*=\s*"(https?://[^"]+)"\s*;')

# Minimum run of printable bytes counted as a string, matching `strings -n 4`.
MIN_STRING_LEN = 4
# The fail-closed message the updater prints; stable across the ADR 0013 UX rewordings.
FAIL_CLOSED_NEEDLE = "not available"
RUN_TIMEOUT_SECONDS = 120

FAKE_CURL = """#!/bin/sh
# Fake `curl` for the release-binary audit: records that it was reached, then fails.
printf '%s\\n' "$@" >> "$ZYNK_AUDIT_CURL_MARKER"
exit 1
"""


def url_constants(source_root):
    """Every http(s) URL constant declared in the updater/remote sources, as {name: url}."""
    found = {}
    for rel in URL_CONST_SOURCES:
        path = pathlib.Path(source_root) / rel
        if not path.is_file():
            continue
        for name, url in URL_CONST_RE.findall(path.read_text(encoding="utf-8")):
            found[name] = url
    return found


def extract_strings(path):
    """Printable-ASCII runs in a file — the `strings -a -n 4` behaviour, without needing binutils."""
    data = pathlib.Path(path).read_bytes()
    out, run = [], bytearray()
    for byte in data:
        if 0x20 <= byte < 0x7F or byte == 0x09:
            run.append(byte)
            continue
        if len(run) >= MIN_STRING_LEN:
            out.append(run.decode("ascii"))
        run.clear()
    if len(run) >= MIN_STRING_LEN:
        out.append(run.decode("ascii"))
    return out


def binary_strings(path):
    """`strings` output for the binary, from binutils when available, else the built-in extractor."""
    strings_bin = shutil.which("strings")
    if strings_bin:
        result = subprocess.run(
            [strings_bin, "-a", "-n", str(MIN_STRING_LEN), str(path)],
            capture_output=True,
            text=True,
            errors="replace",
        )
        if result.returncode == 0:
            return result.stdout.splitlines(), "strings(1)"
    return extract_strings(path), "built-in extractor"


def check_strings(binary, source_root, report):
    """No debug-only env name and no update URL constant may appear in the built binary."""
    lines, source = binary_strings(binary)
    haystack = "\n".join(lines)
    report.append(f"strings: read {len(lines)} strings via {source}")

    failures = []
    for name in FORBIDDEN_ENV_NAMES:
        if name in haystack:
            failures.append(f"debug-only env var name {name!r} is present in the release binary")
        else:
            report.append(f"  ok: {name} absent")

    urls = url_constants(source_root)
    if not urls:
        failures.append(
            f"no URL constants found in {list(URL_CONST_SOURCES)} — the audit would be vacuous; "
            "check --source-root"
        )
    for name, url in sorted(urls.items()):
        if url in haystack:
            failures.append(f"update URL constant {name} ({url}) is present in the release binary")
        else:
            report.append(f"  ok: {name} ({url}) absent")
    return failures


def _isolated_env(workdir, marker, extra):
    """A sanitized environment: every inherited ZYNK_* dropped, only isolated ones set back."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("ZYNK_")}
    home = workdir / "home"
    for name in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        directory = workdir / name.lower()
        directory.mkdir(parents=True, exist_ok=True)
        env[name] = str(directory)
    home.mkdir(parents=True, exist_ok=True)
    env["HOME"] = str(home)
    env["TMPDIR"] = str(workdir / "tmp")
    (workdir / "tmp").mkdir(parents=True, exist_ok=True)
    env["ZYNK_HOME"] = str(home)
    env["ZYNK_SQLITE_HOME"] = str(home)
    env["ZYNK_SOCKET_PATH"] = str(workdir / "zynk.sock")
    env["ZYNK_AUDIT_CURL_MARKER"] = str(marker)
    env["PATH"] = os.pathsep.join([str(workdir / "bin"), env.get("PATH", "/usr/bin:/bin")])
    env.update(extra)
    return env


def check_update_fails_closed(binary, report):
    """`zynk update` must fail closed and reach no downloader, with or without the retired seam."""
    failures = []
    with tempfile.TemporaryDirectory(prefix="zynk-release-audit-") as tmp:
        workdir = pathlib.Path(tmp)
        bindir = workdir / "bin"
        bindir.mkdir(parents=True, exist_ok=True)
        fake_curl = bindir / "curl"
        fake_curl.write_text(FAKE_CURL, encoding="utf-8")
        fake_curl.chmod(fake_curl.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        marker = workdir / "curl-was-invoked"

        cases = (
            ("sanitized environment", {}),
            ("with ZYNK_FAKE_UPDATE_VERSION=9.9.9", {"ZYNK_FAKE_UPDATE_VERSION": "9.9.9"}),
        )
        for label, extra in cases:
            env = _isolated_env(workdir, marker, extra)
            try:
                result = subprocess.run(
                    [str(binary), "update"],
                    env=env,
                    cwd=str(workdir),
                    capture_output=True,
                    text=True,
                    errors="replace",
                    timeout=RUN_TIMEOUT_SECONDS,
                )
            except subprocess.TimeoutExpired:
                failures.append(f"`zynk update` ({label}) did not finish in {RUN_TIMEOUT_SECONDS}s")
                continue

            output = (result.stdout or "") + (result.stderr or "")
            case_failures = []
            if result.returncode == 0:
                case_failures.append(f"`zynk update` ({label}) succeeded; it must fail closed")
            if FAIL_CLOSED_NEEDLE not in output:
                case_failures.append(
                    f"`zynk update` ({label}) did not print the fail-closed message: "
                    f"{output.strip()[:300]!r}"
                )
            if marker.exists():
                case_failures.append(
                    f"`zynk update` ({label}) invoked the PATH-local downloader: "
                    f"{marker.read_text(encoding='utf-8', errors='replace').strip()[:300]!r}"
                )
                marker.unlink()
            if case_failures:
                failures += case_failures
                report.append(f"  FAILED: `zynk update` ({label})")
            else:
                report.append(f"  ok: `zynk update` ({label}) failed closed, downloader untouched")
    return failures


def audit(binary, source_root):
    """Run both checks; return (failures, report_lines)."""
    report = [f"auditing release binary: {binary}"]
    failures = check_strings(binary, source_root, report)
    report.append("behaviour: running the binary with a PATH-local fake downloader")
    failures += check_update_fails_closed(binary, report)
    return failures, report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary", help="path to the built release binary")
    parser.add_argument(
        "--source-root",
        default=str(ROOT),
        help="repository root the URL constants are read from (default: this checkout)",
    )
    args = parser.parse_args(argv)

    binary = pathlib.Path(args.binary)
    if not binary.is_file():
        print(f"release-binary audit: FAILED — no such binary: {binary}", file=sys.stderr)
        return 1

    failures, report = audit(binary, args.source_root)
    for line in report:
        print(line)
    if failures:
        print("release-binary audit: FAILED", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    print("release-binary audit: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
