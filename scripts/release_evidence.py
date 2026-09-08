#!/usr/bin/env python3
"""Producer-side release evidence (ADR 0012): writes the EVIDENCE.json sidecar that binds a packaged release
archive to the CI checkout provenance of the job that built it.

Runs on the producing job's native runner AFTER the job executed the actual packaged binary (`zynk --version`);
the execution result is passed in, everything else is read from the archive bytes and the GitHub environment.
`zynk --version` prints only the version — no commit is embedded in the binary — so `git_sha`/`checkout_head`
here are checkout provenance, not a binary self-report."""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from scripts import release_binary  # noqa: E402

SCHEMA = 1
EXEC_STATUSES = ("ran", "not_run")


def build_evidence(*, target, version, archive, exec_status, exec_output, cargo_version, checkout_head, env,
                   toolchain, native_tool_output=None) -> dict:
    spec = release_binary.TARGETS.get(target)
    if spec is None:
        raise ValueError(f"unknown target {target!r}; known: {sorted(release_binary.TARGETS)}")
    if exec_status not in EXEC_STATUSES:
        raise ValueError(f"exec status must be one of {EXEC_STATUSES}, got {exec_status!r}")
    archive = pathlib.Path(archive)
    archive_bytes = archive.read_bytes()
    member, binary = release_binary.extract_single_member(archive)
    info = release_binary.inspect_binary(binary)
    abi = dict(info["abi"])
    if info["format"] == "elf":
        abi["native_glibc_floor"] = release_binary.native_glibc_floor(native_tool_output)
    evidence = {
        "schema": SCHEMA,
        "target": target,
        "tier": spec["tier"],
        "version_input": version,
        "cargo_version": cargo_version,
        "archive": {"name": archive.name, "sha256": release_binary.sha256_bytes(archive_bytes),
                    "size": len(archive_bytes)},
        "binary": {"member": member, "sha256": release_binary.sha256_bytes(binary), "size": len(binary),
                   "format": info["format"], "cpu": info["cpu"], "os": info["os"], "abi": abi},
        "build_inputs": {"libghostty_optimize": env.get("LIBGHOSTTY_VT_OPTIMIZE", ""),
                         "libghostty_simd": env.get("LIBGHOSTTY_VT_SIMD", "")},
        "exec": {"status": exec_status, "output": (exec_output or "").strip()},
        "provenance": {
            "git_sha": env.get("GITHUB_SHA", ""),
            "checkout_head": checkout_head,
            "repository": env.get("GITHUB_REPOSITORY", ""),
            "run_id": env.get("GITHUB_RUN_ID", ""),
            "run_attempt": int(env.get("GITHUB_RUN_ATTEMPT", "0") or 0),
            "job": env.get("GITHUB_JOB", ""),
            "runner_os": env.get("RUNNER_OS", ""),
            "runner_arch": env.get("RUNNER_ARCH", ""),
        },
        "toolchain": dict(toolchain),
    }
    if native_tool_output is not None:
        evidence["native_tool_output"] = native_tool_output.strip()
    return evidence


def cargo_version_from(cargo_toml: pathlib.Path) -> str:
    with open(cargo_toml, "rb") as handle:
        return tomllib.load(handle)["package"]["version"]


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target", required=True, choices=sorted(release_binary.TARGETS))
    parser.add_argument("--version", required=True, help="the requested release version (no leading v)")
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--exec-status", required=True, choices=EXEC_STATUSES)
    parser.add_argument("--exec-output-file", type=pathlib.Path, help="stdout captured from `zynk --version`")
    parser.add_argument("--cargo-toml", required=True, type=pathlib.Path)
    parser.add_argument("--checkout-head", help="`git rev-parse HEAD` of the checkout (default: run git)")
    parser.add_argument("--toolchain", action="append", default=[], metavar="KEY=VALUE")
    parser.add_argument("--native-tool-output-file", type=pathlib.Path)
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)

    checkout_head = args.checkout_head or subprocess.run(
        ["git", "rev-parse", "HEAD"], check=True, capture_output=True, text=True
    ).stdout.strip()
    toolchain = {}
    for item in args.toolchain:
        key, sep, value = item.partition("=")
        if not sep:
            parser.error(f"--toolchain expects KEY=VALUE, got {item!r}")
        toolchain[key] = value.strip()
    exec_output = args.exec_output_file.read_text() if args.exec_output_file else ""
    native = args.native_tool_output_file.read_text() if args.native_tool_output_file else None
    evidence = build_evidence(
        target=args.target, version=args.version, archive=args.archive, exec_status=args.exec_status,
        exec_output=exec_output, cargo_version=cargo_version_from(args.cargo_toml), checkout_head=checkout_head,
        env=os.environ, toolchain=toolchain, native_tool_output=native,
    )
    args.out.write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n")
    print(f"evidence written: {args.out} ({evidence['target']} {evidence['archive']['sha256']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
