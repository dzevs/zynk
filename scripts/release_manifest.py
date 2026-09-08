#!/usr/bin/env python3
"""Consumer-side release manifest (ADR 0012). Reads the candidate run's producer results (`toJSON(needs)`), the
archives downloaded by immutable artifact id, and each EVIDENCE.json sidecar; decides per target:

  ELIGIBLE          test job passed, build passed, download outcome recorded as success, sidecar bound to the
                    exact archive, binary executed, ABI within the published contract (glibc floor measured from
                    the version needs and matching the producer's native objdump evidence)
  BUILT_UNVERIFIED  built, but no applicable test job / test job failed / binary not executed
  OMITTED           not requested, or the build job did not succeed
  INCONSISTENT      evidence contradicts itself (hash, commit, run, version, target, packaging)

`SHA256SUMS` lists ELIGIBLE archives only, and nothing at all when the required target is not ELIGIBLE.
"Present" is never publication authority; ELIGIBLE is evidence for the G2/operator inclusion decision."""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from scripts import release_binary  # noqa: E402

OPTIONAL_MODES = ("none", "eligible", "all")
SIDECAR_SCHEMA_VERSION = 1
STATUSES = ("ELIGIBLE", "BUILT_UNVERIFIED", "OMITTED", "INCONSISTENT")


def _requested(spec: dict, mode: str) -> bool:
    if spec["tier"] == "required":
        return True
    return mode == "all" or (mode == "eligible" and spec["test_job"] is not None)


_HEX64 = re.compile(r"^[0-9a-f]{64}$")
_HEX40 = re.compile(r"^[0-9a-f]{40}$")

# Mandatory EVIDENCE.json shape (schema 1). Every key must exist with the given type; nested dicts recurse.
_SIDECAR_SCHEMA = {
    "schema": int, "target": str, "tier": str, "version_input": str, "cargo_version": str,
    "archive": {"name": str, "sha256": str, "size": int},
    "binary": {"member": str, "sha256": str, "size": int, "format": str, "cpu": str, "os": str, "abi": dict},
    "exec": {"status": str, "output": str},
    "provenance": {"git_sha": str, "checkout_head": str, "repository": str, "run_id": str, "run_attempt": int,
                   "job": str, "runner_os": str, "runner_arch": str},
    "toolchain": dict,
    "build_inputs": dict,
}


def _check_shape(obj, schema, path, problems):
    for key, typ in schema.items():
        if key not in obj:
            problems.append(f"EVIDENCE.json missing {path}{key}")
            continue
        value = obj[key]
        if isinstance(typ, dict):
            if not isinstance(value, dict):
                problems.append(f"EVIDENCE.json {path}{key} is not an object")
            else:
                _check_shape(value, typ, f"{path}{key}.", problems)
        elif typ is int:
            if isinstance(value, bool) or not isinstance(value, int):
                problems.append(f"EVIDENCE.json {path}{key} is not an integer")
        elif not isinstance(value, typ):
            problems.append(f"EVIDENCE.json {path}{key} is not a {typ.__name__}")


def _validate_sidecar(sidecar, name: str, spec: dict, expected_archive: str) -> list[str]:
    """Structural + schema validation that must pass before any content check or eligibility decision."""
    if not isinstance(sidecar, dict):
        return ["EVIDENCE.json is not a JSON object"]
    problems: list[str] = []
    _check_shape(sidecar, _SIDECAR_SCHEMA, "", problems)
    if problems:
        return problems
    if sidecar["schema"] != SIDECAR_SCHEMA_VERSION:
        problems.append(f"EVIDENCE.json schema {sidecar['schema']!r} is not {SIDECAR_SCHEMA_VERSION}")
    if sidecar["target"] != name:
        problems.append(f"sidecar target {sidecar['target']!r} is not {name!r}")
    if sidecar["tier"] != spec["tier"]:
        problems.append(f"sidecar tier {sidecar['tier']!r} is not {spec['tier']!r}")
    if sidecar["archive"]["name"] != expected_archive:
        problems.append(f"sidecar archive name {sidecar['archive']['name']!r} is not {expected_archive!r}")
    if sidecar["binary"]["member"] != spec["member"]:
        problems.append(f"sidecar binary member {sidecar['binary']['member']!r} is not {spec['member']!r}")
    for path, value in (("archive.sha256", sidecar["archive"]["sha256"]), ("binary.sha256", sidecar["binary"]["sha256"])):
        if not _HEX64.match(value):
            problems.append(f"sidecar {path} is not a sha256 hex digest")
    if sidecar["exec"]["status"] not in ("ran", "not_run"):
        problems.append(f"sidecar exec status {sidecar['exec']['status']!r} is not ran/not_run")
    prov = sidecar["provenance"]
    for key in ("git_sha", "checkout_head"):
        if not _HEX40.match(prov[key]):
            problems.append(f"sidecar provenance.{key} is not a 40-hex commit")
    if prov["run_attempt"] < 1:
        problems.append(f"sidecar provenance.run_attempt {prov['run_attempt']!r} is not >= 1")
    if not prov["run_id"]:
        problems.append("sidecar provenance.run_id is empty")
    if prov["job"] != spec["build_job"]:
        problems.append(f"sidecar provenance.job {prov['job']!r} is not the producer job {spec['build_job']!r}")
    return problems


def _regular_file(path: pathlib.Path) -> bool:
    return path.is_file() and not path.is_symlink()


def _decide(name: str, spec: dict, ctx: dict, producers: dict, dist: pathlib.Path, cargo_version: str,
            downloads) -> dict:
    entry = {
        "tier": spec["tier"], "status": None, "reasons": [], "producer_job": spec["build_job"],
        "producer_run_id": None, "producer_run_attempt": None, "artifact_id": None,
        "archive": spec["archive"].format(version=ctx["version"]), "sha256": None,
    }

    def finish(status, reasons):
        entry["status"] = status
        entry["reasons"] = list(reasons)
        if status == "ELIGIBLE" and not isinstance(entry["sha256"], str):
            raise ValueError(f"{name}: ELIGIBLE without an archive hash")
        return entry

    # ---- phase 0: request + producer result -------------------------------------------------------------
    if not _requested(spec, ctx["optional_targets"]):
        return finish("OMITTED", [f"not requested (optional_targets={ctx['optional_targets']})"])
    build = producers.get(spec["build_job"]) or {}
    result = build.get("result", "missing")
    if result != "success":
        return finish("OMITTED", [f"job {spec['build_job']} result={result}"])

    # ---- phase 1: mandatory structure (any gap is INCONSISTENT before anything else is trusted) ------------
    bad: list[str] = []
    artifact_id = (build.get("outputs") or {}).get("artifact-id", "")
    entry["artifact_id"] = artifact_id or None
    if not artifact_id:
        bad.append(f"job {spec['build_job']} succeeded without an artifact id")
    elif not release_binary.valid_artifact_id(artifact_id):
        bad.append(f"artifact id {artifact_id!r} is not a single positive safe-integer id")
    if bad:
        return finish("INCONSISTENT", bad)
    # Download-outcome evidence is mandatory: without a typed outcome object nothing is read (ADR 0012 §4).
    if not isinstance(downloads, dict):
        return finish("INCONSISTENT", ["download outcome evidence missing or not a JSON object; artifact not read"])
    outcome = downloads.get(name)
    if outcome != "success":
        # Files left by a failed or partial download are never read: nothing below runs.
        shown = outcome if isinstance(outcome, str) and outcome else "not recorded"
        return finish("INCONSISTENT", [f"artifact download outcome={shown}; downloaded files ignored"])
    tdir = dist / name
    archive = tdir / entry["archive"]
    sidecar_path = tdir / "EVIDENCE.json"
    if not tdir.is_dir():
        bad.append(f"no downloaded artifact directory for {name}")
        return finish("INCONSISTENT", bad)
    names = sorted(p.name for p in tdir.iterdir())
    extra = sorted(set(names) - {entry["archive"], "EVIDENCE.json"})
    if extra:
        bad.append(f"unexpected entries in the artifact: {extra}")
    for label, path in (("archive", archive), ("EVIDENCE.json", sidecar_path)):
        if path.name not in names:
            bad.append(f"{label} {path.name} missing from the downloaded artifact")
        elif not _regular_file(path):
            bad.append(f"{label} {path.name} is not a regular file")
    if bad:
        return finish("INCONSISTENT", bad)
    try:
        sidecar = json.loads(sidecar_path.read_text())
    except (OSError, ValueError) as err:
        return finish("INCONSISTENT", [f"EVIDENCE.json unreadable: {err}"])
    problems = _validate_sidecar(sidecar, name, spec, entry["archive"])
    if problems:
        return finish("INCONSISTENT", problems)
    prov = sidecar["provenance"]
    entry["producer_run_id"] = prov["run_id"]
    entry["producer_run_attempt"] = prov["run_attempt"]

    # ---- phase 2: content binding -----------------------------------------------------------------------
    archive_sha = release_binary.sha256_file(archive)
    entry["sha256"] = archive_sha
    if sidecar["archive"]["sha256"] != archive_sha:
        bad.append("archive sha256 differs from the sidecar (archive changed after the evidence was written)")
    try:
        member, binary = release_binary.extract_single_member(archive)
    except (OSError, ValueError) as err:
        member, binary = None, None
        bad.append(f"archive not a single-member archive: {err}")
    if member is not None and member != spec["member"]:
        bad.append(f"archive member {member!r} is not the expected {spec['member']!r}")
    info = None
    if binary is not None:
        if sidecar["binary"]["sha256"] != release_binary.sha256_bytes(binary):
            bad.append("binary sha256 differs from the sidecar")
        try:
            info = release_binary.inspect_binary(binary)
        except ValueError as err:
            bad.append(f"binary header unreadable: {err}")
    if info is not None:
        for key in ("format", "cpu", "os"):
            if info[key] != spec[key]:
                bad.append(f"binary {key} {info[key]!r} does not match the target's {spec[key]!r}")
            if sidecar["binary"][key] != info[key]:
                bad.append(f"sidecar binary {key} differs from the downloaded binary")
        if spec["glibc_max"] and info["format"] == "elf":
            libc = info["abi"].get("libc")
            floor = info["abi"].get("glibc_floor")
            if libc != "glibc":
                bad.append(f"not a glibc-dynamic binary (libc={libc}); the published contract is GNU/glibc dynamic")
            elif floor is None:
                bad.append("no GLIBC version requirements (.gnu.version_r) in the binary")
            elif not release_binary.glibc_within(floor, spec["glibc_max"]):
                bad.append(f"glibc floor {floor} exceeds the published {spec['glibc_max']} contract")
            # The producer's native `objdump -T` measurement is mandatory and must agree (ADR 0012 §4).
            native = (sidecar["binary"]["abi"] or {}).get("native_glibc_floor")
            if not isinstance(native, str) or not native:
                bad.append("no native objdump glibc evidence recorded by the producer")
            elif floor is not None and native != floor:
                bad.append(f"native objdump glibc floor {native} disagrees with the measured floor {floor}")
    if prov["git_sha"] != ctx["git_sha"]:
        bad.append(f"sidecar git_sha {prov['git_sha']!r} is not the candidate {ctx['git_sha']!r}")
    if prov["checkout_head"] != ctx["git_sha"]:
        bad.append(f"sidecar checkout_head {prov['checkout_head']!r} is not the candidate {ctx['git_sha']!r}")
    if str(prov["run_id"]) != str(ctx["run_id"]):
        bad.append(f"sidecar run_id {prov['run_id']!r} is not this run {ctx['run_id']!r}")
    if sidecar["version_input"] != ctx["version"]:
        bad.append(f"sidecar version_input {sidecar['version_input']!r} is not {ctx['version']!r}")
    if sidecar["cargo_version"] != cargo_version:
        bad.append(f"sidecar cargo_version {sidecar['cargo_version']!r} is not {cargo_version!r}")
    unverified: list[str] = []
    if sidecar["exec"]["status"] == "ran":
        expected = f"zynk {ctx['version']}"
        if sidecar["exec"]["output"].strip() != expected:
            bad.append(f"executed binary printed {sidecar['exec']['output']!r}, expected {expected!r}")
    else:
        unverified.append("binary was not executed on the producing runner")

    # ---- phase 3: applicable test evidence --------------------------------------------------------------
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


def evaluate(ctx: dict, producers: dict, dist: pathlib.Path, cargo_toml: pathlib.Path, downloads) -> dict:
    """`downloads` is the per-target download-step outcome object; anything but a JSON object fails closed."""
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
    if not isinstance(downloads, dict):
        manifest["manifest_reasons"].append("download outcome evidence missing or not a JSON object")
        manifest["ok"] = False
    dist = pathlib.Path(dist)
    for name, spec in release_binary.TARGETS.items():
        try:
            entry = _decide(name, spec, ctx, producers, dist, cargo_version, downloads)
        except Exception as err:  # noqa: BLE001 — one target must never abort the whole manifest
            entry = {
                "tier": spec["tier"], "status": "INCONSISTENT",
                "reasons": [f"evaluation error: {type(err).__name__}: {err}"], "producer_job": spec["build_job"],
                "producer_run_id": None, "producer_run_attempt": None, "artifact_id": None,
                "archive": spec["archive"].format(version=ctx["version"]), "sha256": None,
            }
        manifest["targets"][name] = entry
        if spec["tier"] == "required" and entry["status"] != "ELIGIBLE":
            manifest["ok"] = False
    return manifest


def render_sha256sums(manifest: dict) -> str:
    """ELIGIBLE archives only; nothing at all when the manifest failed; an ELIGIBLE entry without a real hash is
    a programming error, never a published line."""
    if not manifest["ok"]:
        return ""
    lines = []
    for name, e in manifest["targets"].items():
        if e["status"] != "ELIGIBLE":
            continue
        if not isinstance(e["sha256"], str) or not _HEX64.match(e["sha256"]):
            raise ValueError(f"{name}: ELIGIBLE entry without a sha256 digest")
        lines.append(f"{e['sha256']}  {e['archive']}")
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
    parser.add_argument("--downloads", required=True, type=pathlib.Path,
                        help="JSON object: per-target download step outcome (success|failure|skipped|cancelled)")
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    ctx = {"version": args.version, "git_sha": args.sha, "run_id": args.run_id, "run_attempt": args.run_attempt,
           "optional_targets": args.optional_targets}
    producers = json.loads(args.producers.read_text())
    try:
        downloads = json.loads(args.downloads.read_text())
    except (OSError, ValueError):
        downloads = None  # evaluate() fails closed on anything that is not an object
    manifest = evaluate(ctx, producers, args.dist, args.cargo_toml, downloads=downloads)
    write_outputs(manifest, args.out)
    sys.stdout.write(render_text(manifest))
    return 0 if manifest["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
