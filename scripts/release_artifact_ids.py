#!/usr/bin/env python3
"""Artifact-id export for the candidate-evidence workflow (ADR 0012).

The manifest job may download ONLY by the immutable artifact id a successful producer exported. The pinned
download action treats an empty `artifact-ids` as "download everything", which would read unreferenced or
retained artifacts, so every id is validated here first: exactly one numeric id from a job whose result is
`success`, else nothing is exported for that target and the manifest records the gap as INCONSISTENT."""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from scripts import release_binary  # noqa: E402

_NUMERIC_ID = re.compile(r"^[0-9]{1,20}$")


def output_key(target: str) -> str:
    return "id_" + target.replace("-", "_")


def export_ids(producers: dict) -> dict:
    ids = {}
    for target, spec in release_binary.TARGETS.items():
        job = producers.get(spec["build_job"]) or {}
        result = job.get("result", "missing")
        raw = (job.get("outputs") or {}).get("artifact-id")
        if result != "success":
            ids[target] = {"id": "", "valid": False, "reason": f"job {spec['build_job']} result={result}"}
        elif raw is None or raw == "":
            ids[target] = {"id": "", "valid": False, "reason": f"job {spec['build_job']} exported an empty artifact id"}
        elif not isinstance(raw, str) or not _NUMERIC_ID.match(raw):
            ids[target] = {"id": "", "valid": False,
                           "reason": f"job {spec['build_job']} artifact id {raw!r} is not a single numeric id"}
        else:
            ids[target] = {"id": raw, "valid": True, "reason": ""}
    return ids


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--producers", required=True, type=pathlib.Path, help="JSON: the run's `needs` context")
    parser.add_argument("--github-output", required=True, type=pathlib.Path, help="$GITHUB_OUTPUT file to append to")
    parser.add_argument("--summary", type=pathlib.Path, help="optional JSON summary of the validation")
    args = parser.parse_args(argv)
    ids = export_ids(json.loads(args.producers.read_text()))
    with open(args.github_output, "a", encoding="utf-8") as handle:
        for target, entry in ids.items():
            handle.write(f"{output_key(target)}={entry['id']}\n")
    if args.summary:
        args.summary.write_text(json.dumps(ids, indent=2, sort_keys=True) + "\n")
    for target, entry in ids.items():
        print(f"{target}: {'id ' + entry['id'] if entry['valid'] else 'no download (' + entry['reason'] + ')'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
