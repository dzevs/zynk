#!/usr/bin/env python3
"""Consumer-side release manifest (ADR 0012). Reads the candidate run's producer results (`toJSON(needs)`), the
archives downloaded by immutable artifact id, and each EVIDENCE.json sidecar; decides per target:

  ELIGIBLE          test job passed, build passed, sidecar bound to the exact archive, binary executed, ABI within
                    the published contract
  BUILT_UNVERIFIED  built, but no applicable test job / test job failed / binary not executed
  OMITTED           not requested, or the build job did not succeed
  INCONSISTENT      evidence contradicts itself (hash, commit, run, version, target, packaging)

`SHA256SUMS` lists ELIGIBLE archives only, and nothing at all when the required target is not ELIGIBLE.
"Present" is never publication authority; ELIGIBLE is evidence for the G2/operator inclusion decision."""
from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from scripts import release_binary  # noqa: E402

OPTIONAL_MODES = ("none", "eligible", "all")
STATUSES = ("ELIGIBLE", "BUILT_UNVERIFIED", "OMITTED", "INCONSISTENT")


def _requested(spec: dict, mode: str) -> bool:
    if spec["tier"] == "required":
        return True
    return mode == "all" or (mode == "eligible" and spec["test_job"] is not None)


def _decide(name: str, spec: dict, ctx: dict, producers: dict, dist: pathlib.Path, cargo_version: str) -> dict:
    entry = {
        "tier": spec["tier"], "status": None, "reasons": [], "producer_job": spec["build_job"],
        "producer_run_id": None, "producer_run_attempt": None, "artifact_id": None,
        "archive": spec["archive"].format(version=ctx["version"]), "sha256": None,
    }
    bad: list[str] = []      # inconsistencies
    unverified: list[str] = []

    def finish(status, reasons):
        entry["status"] = status
        entry["reasons"] = list(reasons)
        return entry

    if not _requested(spec, ctx["optional_targets"]):
        return finish("OMITTED", [f"not requested (optional_targets={ctx['optional_targets']})"])
    build = producers.get(spec["build_job"]) or {}
    result = build.get("result", "missing")
    if result != "success":
        return finish("OMITTED", [f"job {spec['build_job']} result={result}"])
    artifact_id = (build.get("outputs") or {}).get("artifact-id", "")
    entry["artifact_id"] = artifact_id or None
    if not artifact_id:
        bad.append(f"job {spec['build_job']} succeeded without an artifact id")

    tdir = dist / name
    files = sorted(p.name for p in tdir.iterdir()) if tdir.is_dir() else []
    archive = tdir / entry["archive"]
    if entry["archive"] not in files:
        bad.append(f"archive {entry['archive']} missing from the downloaded artifact")
    extra = sorted(set(files) - {entry["archive"], "EVIDENCE.json"})
    if extra:
        bad.append(f"unexpected files in the artifact: {extra}")
    sidecar = None
    if "EVIDENCE.json" not in files:
        bad.append("EVIDENCE.json missing from the artifact")
    else:
        try:
            sidecar = json.loads((tdir / "EVIDENCE.json").read_text())
        except (OSError, ValueError) as err:
            bad.append(f"EVIDENCE.json unreadable: {err}")

    if archive.is_file():
        archive_sha = release_binary.sha256_file(archive)
        try:
            member, binary = release_binary.extract_single_member(archive)
        except (OSError, ValueError) as err:
            member, binary = None, None
            bad.append(f"archive not a single-member archive: {err}")
        if member is not None and member != spec["member"]:
            bad.append(f"archive member {member!r} is not the expected {spec['member']!r}")
        info = None
        if binary is not None:
            try:
                info = release_binary.inspect_binary(binary)
            except ValueError as err:
                bad.append(f"binary header unreadable: {err}")
        if info is not None:
            for key in ("format", "cpu", "os"):
                if info[key] != spec[key]:
                    bad.append(f"binary {key} {info[key]!r} does not match the target's {spec[key]!r}")
            floor = info["abi"].get("glibc_floor") if info["format"] == "elf" else None
            if spec["glibc_max"] and floor and not release_binary.glibc_within(floor, spec["glibc_max"]):
                bad.append(f"glibc floor {floor} exceeds the published {spec['glibc_max']} contract")
        if sidecar is not None:
            prov = sidecar.get("provenance") or {}
            entry["producer_run_id"] = prov.get("run_id")
            entry["producer_run_attempt"] = prov.get("run_attempt")
            if sidecar.get("target") != name:
                bad.append(f"sidecar target {sidecar.get('target')!r} is not {name!r}")
            if (sidecar.get("archive") or {}).get("sha256") != archive_sha:
                bad.append("archive sha256 differs from the sidecar (archive changed after the evidence was written)")
            if binary is not None and (sidecar.get("binary") or {}).get("sha256") != release_binary.sha256_bytes(binary):
                bad.append("binary sha256 differs from the sidecar")
            if info is not None:
                for key in ("format", "cpu"):
                    if (sidecar.get("binary") or {}).get(key) != info[key]:
                        bad.append(f"sidecar binary {key} differs from the downloaded binary")
            if prov.get("git_sha") != ctx["git_sha"]:
                bad.append(f"sidecar git_sha {prov.get('git_sha')!r} is not the candidate {ctx['git_sha']!r}")
            if prov.get("checkout_head") != ctx["git_sha"]:
                bad.append(f"sidecar checkout_head {prov.get('checkout_head')!r} is not the candidate {ctx['git_sha']!r}")
            if str(prov.get("run_id")) != str(ctx["run_id"]):
                bad.append(f"sidecar run_id {prov.get('run_id')!r} is not this run {ctx['run_id']!r}")
            if sidecar.get("version_input") != ctx["version"]:
                bad.append(f"sidecar version_input {sidecar.get('version_input')!r} is not {ctx['version']!r}")
            if sidecar.get("cargo_version") != cargo_version:
                bad.append(f"sidecar cargo_version {sidecar.get('cargo_version')!r} is not {cargo_version!r}")
            exec_info = sidecar.get("exec") or {}
            if exec_info.get("status") == "ran":
                expected = f"zynk {ctx['version']}"
                if exec_info.get("output", "").strip() != expected:
                    bad.append(f"executed binary printed {exec_info.get('output')!r}, expected {expected!r}")
            else:
                unverified.append("binary was not executed on the producing runner")
        entry["sha256"] = archive_sha

    if spec["test_job"] is None:
        unverified.append("no applicable hosted test job for this target")
    else:
        test_result = (producers.get(spec["test_job"]) or {}).get("result", "missing")
        if test_result != "success":
            unverified.append(f"job {spec['test_job']} result={test_result}")

    if bad:
        return finish("INCONSISTENT", bad + unverified)
    if unverified:
        return finish("BUILT_UNVERIFIED", unverified)
    return finish("ELIGIBLE", [])


def evaluate(ctx: dict, producers: dict, dist: pathlib.Path, cargo_toml: pathlib.Path) -> dict:
    if ctx["optional_targets"] not in OPTIONAL_MODES:
        raise ValueError(f"optional_targets must be one of {OPTIONAL_MODES}")
    with open(cargo_toml, "rb") as handle:
        cargo_version = tomllib.load(handle)["package"]["version"]
    manifest = {
        "version": ctx["version"], "git_sha": ctx["git_sha"], "manifest_run_id": str(ctx["run_id"]),
        "manifest_run_attempt": int(ctx["run_attempt"]), "optional_targets": ctx["optional_targets"],
        "cargo_version": cargo_version, "manifest_reasons": [], "targets": {}, "ok": True,
    }
    if cargo_version != ctx["version"]:
        manifest["manifest_reasons"].append(
            f"requested version {ctx['version']!r} is not the checkout's Cargo.toml version {cargo_version!r}")
        manifest["ok"] = False
    dist = pathlib.Path(dist)
    for name, spec in release_binary.TARGETS.items():
        entry = _decide(name, spec, ctx, producers, dist, cargo_version)
        manifest["targets"][name] = entry
        if spec["tier"] == "required" and entry["status"] != "ELIGIBLE":
            manifest["ok"] = False
    return manifest


def render_sha256sums(manifest: dict) -> str:
    if not manifest["ok"]:
        return ""
    lines = [f"{e['sha256']}  {e['archive']}" for e in manifest["targets"].values() if e["status"] == "ELIGIBLE"]
    return "".join(line + "\n" for line in sorted(lines, key=lambda l: l.split("  ", 1)[1]))


def render_text(manifest: dict) -> str:
    lines = [
        f"version={manifest['version']}",
        f"git_sha={manifest['git_sha']}",
        f"manifest_run_id={manifest['manifest_run_id']}",
        f"manifest_run_attempt={manifest['manifest_run_attempt']}",
        f"optional_targets={manifest['optional_targets']}",
        f"cargo_version={manifest['cargo_version']}",
    ]
    for name, e in manifest["targets"].items():
        reasons = "; ".join(e["reasons"]) or "-"
        lines.append(
            f"target={name} tier={e['tier']} status={e['status']} producer_job={e['producer_job']} "
            f"producer_run_id={e['producer_run_id'] or '-'} producer_run_attempt={e['producer_run_attempt'] if e['producer_run_attempt'] is not None else '-'} "
            f"artifact_id={e['artifact_id'] or '-'} sha256={e['sha256'] or '-'} reasons={reasons}"
        )
    for reason in manifest["manifest_reasons"]:
        lines.append(f"manifest_reason={reason}")
    lines.append("result=OK" if manifest["ok"] else "result=FAIL")
    return "\n".join(lines) + "\n"


def write_outputs(manifest: dict, out: pathlib.Path) -> None:
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "RELEASE_MANIFEST.txt").write_text(render_text(manifest))
    (out / "RELEASE_MANIFEST.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    (out / "SHA256SUMS").write_text(render_sha256sums(manifest))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--sha", required=True, help="GITHUB_SHA of the candidate run")
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--optional-targets", required=True, choices=OPTIONAL_MODES)
    parser.add_argument("--producers", required=True, type=pathlib.Path, help="JSON: the run's `needs` context")
    parser.add_argument("--dist", required=True, type=pathlib.Path, help="dir with one subdir per target")
    parser.add_argument("--cargo-toml", required=True, type=pathlib.Path)
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    ctx = {"version": args.version, "git_sha": args.sha, "run_id": args.run_id, "run_attempt": args.run_attempt,
           "optional_targets": args.optional_targets}
    producers = json.loads(args.producers.read_text())
    manifest = evaluate(ctx, producers, args.dist, args.cargo_toml)
    write_outputs(manifest, args.out)
    sys.stdout.write(render_text(manifest))
    return 0 if manifest["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
