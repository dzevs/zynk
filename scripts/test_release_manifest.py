"""Consumer-side release manifest (ADR 0012): per-target ELIGIBLE / BUILT_UNVERIFIED / OMITTED / INCONSISTENT
decisions from producer job results + hash-bound EVIDENCE.json sidecars; SHA256SUMS over ELIGIBLE archives only;
a required-target problem fails the manifest. unittest style."""
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from scripts import release_binary, release_evidence, release_manifest  # noqa: E402
from scripts.test_release_evidence import fake_elf, fake_macho, fake_pe, write_targz, write_zip  # noqa: E402

SHA = "a" * 40
CTX = {"version": "3.1.0", "git_sha": SHA, "run_id": "1001", "run_attempt": 1, "optional_targets": "eligible"}


def runner_for(job):
    """The native runner the target table expects for the producer job (ADR 0012: native execution)."""
    for spec in release_binary.TARGETS.values():
        if spec["build_job"] == job:
            return spec["runner"]
    raise KeyError(job)


def producer_env(job, run_id="1001", attempt="1", sha=SHA, runner=None):
    runner = runner or runner_for(job)
    return {"GITHUB_SHA": sha, "GITHUB_RUN_ID": run_id, "GITHUB_RUN_ATTEMPT": attempt, "GITHUB_JOB": job,
            "GITHUB_REPOSITORY": "dzevs/zynk", "RUNNER_OS": runner["os"], "RUNNER_ARCH": runner["arch"],
            "LIBGHOSTTY_VT_OPTIMIZE": "ReleaseFast", "LIBGHOSTTY_VT_SIMD": "false"}


class Fixture:
    """A dist/ directory plus a producers (needs) map, built target by target."""

    def __init__(self, tmp):
        self.dist = pathlib.Path(tmp, "dist")
        self.dist.mkdir()
        self.producers = {}
        self.cargo = pathlib.Path(tmp, "Cargo.toml")
        self.cargo.write_text('[package]\nname = "zynk"\nversion = "3.1.0"\n')

    def job(self, name, result="success", artifact_id="777"):
        entry = {"result": result, "outputs": {}}
        if name.startswith("build-") and result == "success" and artifact_id is not None:
            entry["outputs"]["artifact-id"] = artifact_id
        self.producers[name] = entry

    def artifact(self, target, binary=None, version="3.1.0", exec_status="ran", exec_output=None, env=None,
                 sidecar=True, member=None, cargo_version="3.1.0", checkout_head=SHA, extra_file=None,
                 native_tool_output=None, tree_status="", mutate=None):
        spec = release_binary.TARGETS[target]
        if binary is None:
            if spec["format"] == "elf":
                binary = fake_elf(machine=0xB7 if spec["cpu"] == "aarch64" else 0x3E)
            elif spec["format"] == "macho":
                binary = fake_macho(cputype=0x01000007 if spec["cpu"] == "x86_64" else 0x0100000C)
            else:
                binary = fake_pe()
        member = member or spec["member"]
        tdir = self.dist / target
        tdir.mkdir(exist_ok=True)
        archive = tdir / spec["archive"].format(version=version)
        (write_zip if archive.suffix == ".zip" else write_targz)(archive, member, binary)
        if sidecar and native_tool_output is None and spec["format"] == "elf":
            info = release_binary.inspect_binary(binary)
            native_tool_output = "".join(f"GLIBC_{v}\n" for v in info["abi"]["glibc_versions"]) or None
        if sidecar:
            ev = release_evidence.build_evidence(
                target=target, version=version, archive=archive, exec_status=exec_status,
                exec_output=exec_output if exec_output is not None else f"zynk {version}\n",
                cargo_version=cargo_version, checkout_head=checkout_head,
                env=env or producer_env(spec["build_job"]), toolchain={"rustc": "rustc 1.98.1"},
                native_tool_output=native_tool_output, tree_status=tree_status,
            )
            if mutate:
                mutate(ev)
            (tdir / "EVIDENCE.json").write_text(json.dumps(ev))
        if extra_file:
            (tdir / extra_file).write_text("stray")
        return archive

    def required_ok(self):
        self.job("test-linux")
        self.job("build-linux-x86_64")
        self.artifact("linux-x86_64")

    def default_downloads(self):
        """Explicit outcomes: success for every producer that succeeded, skipped otherwise (as the workflow records)."""
        out = {}
        for target, spec in release_binary.TARGETS.items():
            job = self.producers.get(spec["build_job"]) or {}
            out[target] = "success" if job.get("result") == "success" else "skipped"
        return out

    def evaluate(self, downloads="default", **ctx_overrides):
        ctx = dict(CTX, **ctx_overrides)
        if downloads == "default":
            downloads = self.default_downloads()
        return release_manifest.evaluate(ctx, self.producers, self.dist, cargo_toml=self.cargo, downloads=downloads)

    def cli(self, out, optional_targets="eligible", downloads="default", run_attempt="1", downloads_text=None):
        producers = pathlib.Path(self.dist.parent, "producers.json")
        producers.write_text(json.dumps(self.producers))
        dl = pathlib.Path(self.dist.parent, "downloads.json")
        if downloads_text is not None:
            dl.write_text(downloads_text)
        else:
            dl.write_text(json.dumps(self.default_downloads() if downloads == "default" else downloads))
        args = [sys.executable, str(ROOT / "scripts" / "release_manifest.py"), "--version", "3.1.0", "--sha", SHA,
                "--run-id", "1001", "--run-attempt", run_attempt, "--optional-targets", optional_targets,
                "--producers", str(producers), "--dist", str(self.dist), "--cargo-toml", str(self.cargo),
                "--downloads", str(dl), "--out", str(out)]
        return subprocess.run(args, capture_output=True, text=True)


def status(manifest, target):
    return manifest["targets"][target]["status"]


class RequiredTarget(unittest.TestCase):
    def test_required_eligible_yields_ok_and_checksums(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            m = f.evaluate(optional_targets="none")
            self.assertTrue(m["ok"], m)
            self.assertEqual(status(m, "linux-x86_64"), "ELIGIBLE")
            self.assertEqual(m["targets"]["linux-x86_64"]["producer_run_attempt"], 1)
            sums = release_manifest.render_sha256sums(m)
            self.assertIn("zynk-v3.1.0-linux-x86_64.tar.gz", sums)
            self.assertEqual(len(sums.strip().splitlines()), 1)

    def test_required_build_failure_fails_the_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64", result="failure")
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "OMITTED")

    def test_required_missing_archive_is_inconsistent_and_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")

    def test_required_test_failure_is_built_unverified_and_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux", result="failure")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64")
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "BUILT_UNVERIFIED")

    def test_glibc_floor_above_contract_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", binary=fake_elf(glibc=("GLIBC_2.17", "GLIBC_2.34")))
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("2.34" in r for r in m["targets"]["linux-x86_64"]["reasons"]))


class OptionalTargets(unittest.TestCase):
    def test_not_requested_is_omitted_without_failing(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            m = f.evaluate(optional_targets="none")
            self.assertTrue(m["ok"])
            for t in ("macos-aarch64", "windows-x86_64", "linux-aarch64", "macos-x86_64"):
                self.assertEqual(status(m, t), "OMITTED", t)

    def test_eligible_optional_targets_join_the_checksums(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-macos-aarch64")
            f.job("build-macos-aarch64", artifact_id="778")
            f.artifact("macos-aarch64")
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "macos-aarch64"), "ELIGIBLE")
            self.assertEqual(status(m, "windows-x86_64"), "ELIGIBLE")
            self.assertEqual(status(m, "linux-aarch64"), "OMITTED")
            sums = release_manifest.render_sha256sums(m)
            self.assertEqual(len(sums.strip().splitlines()), 3)

    def test_optional_build_failure_is_omitted_and_does_not_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-macos-aarch64")
            f.job("build-macos-aarch64", result="failure")
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "macos-aarch64"), "OMITTED")
            self.assertNotIn("macos-aarch64", release_manifest.render_sha256sums(m))

    def test_optional_test_failure_is_built_unverified(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64", result="failure")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "BUILT_UNVERIFIED")
            self.assertNotIn("windows", release_manifest.render_sha256sums(m))

    def test_build_only_targets_are_never_eligible(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("build-linux-aarch64", artifact_id="780")
            f.artifact("linux-aarch64", binary=fake_elf(machine=0xB7), exec_status="not_run", exec_output="")
            m = f.evaluate(optional_targets="all")
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "linux-aarch64"), "BUILT_UNVERIFIED")

    def test_rosetta_executed_intel_macos_is_never_eligible(self):
        # Gate-3 INSPECTOR-675-001: a successful Rosetta run on the native ARM64 runner is execution evidence
        # only; without an Intel-native test job the target stays BUILT_UNVERIFIED and out of SHA256SUMS.
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("build-macos-x86_64", artifact_id="781")
            f.artifact("macos-x86_64", exec_status="ran", exec_output="zynk 3.1.0\n")
            m = f.evaluate(optional_targets="all")
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "macos-x86_64"), "BUILT_UNVERIFIED")
            self.assertTrue(any("no applicable hosted test job" in r for r in m["targets"]["macos-x86_64"]["reasons"]))
            self.assertEqual(m["targets"]["macos-x86_64"]["producer_runner"], "macOS/ARM64")
            sums = release_manifest.render_sha256sums(m)
            self.assertNotIn("macos-x86_64", sums)
            self.assertEqual(sums.count("\n"), 1)

    def test_unexecuted_binary_is_built_unverified(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64", exec_status="not_run", exec_output="")
            m = f.evaluate()
            self.assertEqual(status(m, "windows-x86_64"), "BUILT_UNVERIFIED")


class Inconsistencies(unittest.TestCase):
    def optional_windows(self, f, **kw):
        f.required_ok()
        f.job("test-windows-x86_64")
        f.job("build-windows-x86_64", artifact_id="779")
        return f.artifact("windows-x86_64", **kw)

    def test_success_without_artifact_id_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id=None)
            f.artifact("windows-x86_64")
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")

    def test_missing_sidecar_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, sidecar=False)
            self.assertEqual(status(f.evaluate(), "windows-x86_64"), "INCONSISTENT")

    def test_archive_modified_after_the_sidecar_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            archive = self.optional_windows(f)
            write_zip(archive, "zynk.exe", fake_pe(subsystem=2))
            m = f.evaluate()
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")
            self.assertTrue(any("sha256" in r for r in m["targets"]["windows-x86_64"]["reasons"]))

    def test_sidecar_from_another_commit_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, env=producer_env("build-windows-x86_64", sha="b" * 40), checkout_head="b" * 40)
            self.assertEqual(status(f.evaluate(), "windows-x86_64"), "INCONSISTENT")

    def test_sidecar_from_another_run_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, env=producer_env("build-windows-x86_64", run_id="999"))
            m = f.evaluate()
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")
            self.assertTrue(any("run_id" in r for r in m["targets"]["windows-x86_64"]["reasons"]))

    def test_producer_from_an_earlier_attempt_is_reusable_and_recorded(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, env=producer_env("build-windows-x86_64", attempt="1"))
            m = f.evaluate(run_attempt=2)
            self.assertEqual(status(m, "windows-x86_64"), "ELIGIBLE")
            self.assertEqual(m["targets"]["windows-x86_64"]["producer_run_attempt"], 1)
            self.assertEqual(m["manifest_run_attempt"], 2)

    def test_version_mismatch_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, exec_output="zynk 3.0.1\n")
            self.assertEqual(status(f.evaluate(), "windows-x86_64"), "INCONSISTENT")

    def test_wrong_member_name_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, member="zynk-old.exe")
            self.assertEqual(status(f.evaluate(), "windows-x86_64"), "INCONSISTENT")

    def test_wrong_cpu_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, binary=fake_pe(machine=0xAA64))
            self.assertEqual(status(f.evaluate(), "windows-x86_64"), "INCONSISTENT")

    def test_stray_file_next_to_the_archive_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.optional_windows(f, extra_file="zynk-v3.1.0-windows-x86_64-old.zip")
            self.assertEqual(status(f.evaluate(), "windows-x86_64"), "INCONSISTENT")

    def test_required_inconsistency_fails_even_when_optional_is_fine(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", env=producer_env("build-linux-x86_64", run_id="999"))
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            m = f.evaluate()
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertEqual(release_manifest.render_sha256sums(m).strip(), "", "nothing is published on failure")


class StrictStructure(unittest.TestCase):
    """Codex Gate-2 P1 at 0ece9be: absent or malformed structure must be INCONSISTENT, never silently ELIGIBLE."""

    def required_with_sidecar_text(self, f, text):
        f.job("test-linux")
        f.job("build-linux-x86_64")
        f.artifact("linux-x86_64")
        (f.dist / "linux-x86_64" / "EVIDENCE.json").write_text(text)

    def assert_required_inconsistent(self, m, needle=None):
        self.assertFalse(m["ok"])
        self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
        self.assertEqual(release_manifest.render_sha256sums(m), "")
        if needle:
            self.assertTrue(any(needle in r for r in m["targets"]["linux-x86_64"]["reasons"]),
                            m["targets"]["linux-x86_64"]["reasons"])

    def test_null_sidecar_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.required_with_sidecar_text(f, "null")
            m = f.evaluate(optional_targets="none")
            self.assert_required_inconsistent(m, "not a JSON object")
            self.assertIsNone(m["targets"]["linux-x86_64"]["producer_run_attempt"])

    def test_non_object_sidecar_is_inconsistent(self):
        for text in ("[]", '"evidence"', "42"):
            with tempfile.TemporaryDirectory() as tmp:
                f = Fixture(tmp)
                self.required_with_sidecar_text(f, text)
                self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "not a JSON object")

    def test_sidecar_missing_mandatory_fields_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            path = f.dist / "linux-x86_64" / "EVIDENCE.json"
            ev = json.loads(path.read_text())
            del ev["provenance"]
            path.write_text(json.dumps(ev))
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "provenance")

    def test_sidecar_with_wrong_job_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", env=producer_env("build-windows-x86_64"))
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "job")

    def test_sidecar_with_non_integer_attempt_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            path = f.dist / "linux-x86_64" / "EVIDENCE.json"
            ev = json.loads(path.read_text())
            ev["provenance"]["run_attempt"] = "1"
            path.write_text(json.dumps(ev))
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "run_attempt")
            ev["provenance"]["run_attempt"] = 0
            path.write_text(json.dumps(ev))
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "run_attempt")

    def test_sidecar_archive_name_mismatch_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            path = f.dist / "linux-x86_64" / "EVIDENCE.json"
            ev = json.loads(path.read_text())
            ev["archive"]["name"] = "zynk-v3.1.0-linux-x86_64-old.tar.gz"
            path.write_text(json.dumps(ev))
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "archive name")

    def test_directory_in_place_of_the_archive_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            archive = f.dist / "linux-x86_64" / "zynk-v3.1.0-linux-x86_64.tar.gz"
            archive.unlink()
            archive.mkdir()
            m = f.evaluate(optional_targets="none")
            self.assert_required_inconsistent(m, "regular file")
            self.assertIsNone(m["targets"]["linux-x86_64"]["sha256"])

    def test_symlinked_archive_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            tdir = f.dist / "linux-x86_64"
            archive = tdir / "zynk-v3.1.0-linux-x86_64.tar.gz"
            real = pathlib.Path(tmp, "elsewhere.tar.gz")
            archive.rename(real)
            archive.symlink_to(real)
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "regular file")

    def test_directory_in_place_of_the_sidecar_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            sidecar = f.dist / "linux-x86_64" / "EVIDENCE.json"
            sidecar.unlink()
            sidecar.mkdir()
            self.assert_required_inconsistent(f.evaluate(optional_targets="none"), "regular file")

    def test_eligible_entries_always_carry_a_string_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            m = f.evaluate(optional_targets="none")
            for e in m["targets"].values():
                if e["status"] == "ELIGIBLE":
                    self.assertIsInstance(e["sha256"], str)
                    self.assertEqual(len(e["sha256"]), 64)
            m["targets"]["linux-x86_64"]["sha256"] = None
            with self.assertRaises(ValueError):
                release_manifest.render_sha256sums(m)


class OptionalStructure(unittest.TestCase):
    """Codex Gate-2 #1/#2: optional-target structure or decoding problems are INCONSISTENT for that target only."""

    def test_null_sidecar_on_an_optional_target_is_inconsistent_and_linux_still_ships(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            (f.dist / "windows-x86_64" / "EVIDENCE.json").write_text("[]")
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")
            self.assertEqual(release_manifest.render_sha256sums(m).count("\n"), 1)

    def test_corrupt_optional_zip_is_contained(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            archive = f.artifact("windows-x86_64")
            archive.write_bytes(b"PK\x03\x04 definitely not a zip")
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")
            self.assertTrue(any("archive" in r for r in m["targets"]["windows-x86_64"]["reasons"]))

    def test_truncated_binary_inside_a_valid_archive_is_contained(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            archive = f.artifact("windows-x86_64")
            write_zip(archive, "zynk.exe", fake_pe()[:0x90])
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")

    def test_cli_with_a_corrupt_optional_archive_still_writes_the_manifest_and_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            archive = f.artifact("windows-x86_64")
            archive.write_bytes(b"\x00garbage")
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            text = (out / "RELEASE_MANIFEST.txt").read_text()
            self.assertIn("target=windows-x86_64 tier=optional status=INCONSISTENT", text)
            self.assertIn("result=OK", text)
            self.assertEqual((out / "SHA256SUMS").read_text().count("\n"), 1)

    def test_cli_with_a_corrupt_required_archive_writes_a_failing_manifest_and_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            archive = f.artifact("linux-x86_64")
            archive.write_bytes(b"\x1f\x8b broken gzip")
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out, optional_targets="none")
            self.assertNotEqual(proc.returncode, 0)
            text = (out / "RELEASE_MANIFEST.txt").read_text()
            self.assertIn("target=linux-x86_64 tier=required status=INCONSISTENT", text)
            self.assertIn("result=FAIL", text)
            self.assertEqual((out / "SHA256SUMS").read_text(), "")


class DownloadOutcomes(unittest.TestCase):
    """Codex Gate-2 #3: a failed download is a visible exclusion, never a crash and never a source of files."""

    def test_optional_download_failure_excludes_the_target_and_ignores_partial_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")  # a complete-looking directory left behind by a failed download
            downloads = {"linux-x86_64": "success", "windows-x86_64": "failure"}
            m = f.evaluate(downloads=downloads)
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")
            self.assertTrue(any("download" in r for r in m["targets"]["windows-x86_64"]["reasons"]))
            self.assertIsNone(m["targets"]["windows-x86_64"]["sha256"])
            self.assertEqual(release_manifest.render_sha256sums(m).count("\n"), 1)

    def test_required_download_failure_fails_the_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            m = f.evaluate(downloads={"linux-x86_64": "failure"}, optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")

    def test_missing_download_record_for_a_built_target_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            m = f.evaluate(downloads={"linux-x86_64": "success"})
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")

    def test_cli_downloads_file_is_honoured(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64")
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out, downloads={"linux-x86_64": "success", "windows-x86_64": "failure"})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("target=windows-x86_64 tier=optional status=INCONSISTENT", (out / "RELEASE_MANIFEST.txt").read_text())


class ArtifactIds(unittest.TestCase):
    """Codex Gate-2 #4: an artifact id must be a single numeric id; anything else is INCONSISTENT."""

    def test_non_numeric_artifact_id_is_inconsistent(self):
        for bad in ("abc", "12 34", "../x", "12,13", ""):
            with tempfile.TemporaryDirectory() as tmp:
                f = Fixture(tmp)
                f.job("test-linux")
                f.job("build-linux-x86_64", artifact_id=bad)
                f.artifact("linux-x86_64")
                m = f.evaluate(optional_targets="none")
                self.assertFalse(m["ok"], bad)
                self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT", bad)


class GlibcContract(unittest.TestCase):
    """Codex Gate-2 #5: the floor is an ELF version-need measurement, cross-checked with the native objdump evidence."""

    def linux_with(self, f, **kw):
        f.job("test-linux")
        f.job("build-linux-x86_64")
        f.artifact("linux-x86_64", **kw)
        return f.evaluate(optional_targets="none")

    def test_harmless_glibc_looking_string_does_not_reject(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), binary=fake_elf(body=b"seen GLIBC_99.99 in a log line\0"))
            self.assertTrue(m["ok"], m["targets"]["linux-x86_64"]["reasons"])
            self.assertEqual(status(m, "linux-x86_64"), "ELIGIBLE")

    def test_musl_or_static_binary_violates_the_glibc_dynamic_contract(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), binary=fake_elf(glibc=(), interp=b"/lib/ld-musl-x86_64.so.1"))
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), binary=fake_elf(glibc=(), interp=None))
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")

    def test_glibc_binary_without_version_needs_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), binary=fake_elf(glibc=()))
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("GLIBC" in r for r in m["targets"]["linux-x86_64"]["reasons"]))

    def test_missing_native_objdump_evidence_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), native_tool_output="ELF 64-bit LSB pie executable\n")
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("native" in r for r in m["targets"]["linux-x86_64"]["reasons"]))

    def test_native_objdump_evidence_must_agree_with_the_measured_floor(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), native_tool_output="GLIBC_2.17\nGLIBC_2.34\n")
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("native" in r for r in m["targets"]["linux-x86_64"]["reasons"]))
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux_with(Fixture(tmp), native_tool_output="ELF 64-bit\nGLIBC_2.17\nGLIBC_2.30\n")
            self.assertEqual(status(m, "linux-x86_64"), "ELIGIBLE")


class DownloadEvidence(unittest.TestCase):
    """Codex Gate-2 R2 #1: download-outcome evidence is mandatory; null/non-object evidence fails closed."""

    def complete_looking(self, f):
        f.required_ok()
        f.job("test-windows-x86_64")
        f.job("build-windows-x86_64", artifact_id="779")
        f.artifact("windows-x86_64")

    def test_missing_or_non_object_download_evidence_fails_closed_without_reading_artifacts(self):
        for bad in (None, [], "success", 1):
            with tempfile.TemporaryDirectory() as tmp:
                f = Fixture(tmp)
                self.complete_looking(f)
                m = f.evaluate(downloads=bad)
                self.assertFalse(m["ok"], repr(bad))
                self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT", repr(bad))
                self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT", repr(bad))
                self.assertIsNone(m["targets"]["linux-x86_64"]["sha256"], "artifacts must not be read")
                self.assertEqual(release_manifest.render_sha256sums(m), "")

    def test_cli_null_downloads_file_fails_with_empty_sums(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.complete_looking(f)
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out, downloads_text="null")
            self.assertNotEqual(proc.returncode, 0)
            text = (out / "RELEASE_MANIFEST.txt").read_text()
            self.assertIn("result=FAIL", text)
            self.assertIn("target=linux-x86_64 tier=required status=INCONSISTENT", text)
            self.assertEqual((out / "SHA256SUMS").read_text(), "")

    def test_cli_absent_optional_outcome_excludes_only_the_optional_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            self.complete_looking(f)
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out, downloads={"linux-x86_64": "success"})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            text = (out / "RELEASE_MANIFEST.txt").read_text()
            self.assertIn("target=linux-x86_64 tier=required status=ELIGIBLE", text)
            self.assertIn("target=windows-x86_64 tier=optional status=INCONSISTENT", text)
            sums = (out / "SHA256SUMS").read_text()
            self.assertEqual(sums.count("\n"), 1)
            self.assertIn("linux-x86_64", sums)

    def test_cli_requires_the_downloads_argument(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            producers = pathlib.Path(tmp, "producers.json")
            producers.write_text(json.dumps(f.producers))
            proc = subprocess.run([sys.executable, str(ROOT / "scripts" / "release_manifest.py"), "--version", "3.1.0",
                                   "--sha", SHA, "--run-id", "1001", "--run-attempt", "1", "--optional-targets", "none",
                                   "--producers", str(producers), "--dist", str(f.dist), "--cargo-toml", str(f.cargo),
                                   "--out", str(pathlib.Path(tmp, "out"))], capture_output=True, text=True)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("--downloads", proc.stderr)

    def test_non_string_outcome_is_not_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            m = f.evaluate(downloads={"linux-x86_64": True}, optional_targets="none")
            self.assertFalse(m["ok"])


class ArtifactIdBound(unittest.TestCase):
    """Codex R2 nonblocking: ids beyond JavaScript's safe-integer range are not exact in the pinned action."""

    def test_ids_beyond_the_safe_integer_bound_are_rejected(self):
        for bad in ("9007199254740993", "0", "00", "99999999999999999999"):
            with tempfile.TemporaryDirectory() as tmp:
                f = Fixture(tmp)
                f.job("test-linux")
                f.job("build-linux-x86_64", artifact_id=bad)
                f.artifact("linux-x86_64")
                m = f.evaluate(optional_targets="none")
                self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT", bad)
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64", artifact_id="9007199254740991")
            f.artifact("linux-x86_64")
            self.assertEqual(status(f.evaluate(optional_targets="none"), "linux-x86_64"), "ELIGIBLE")


class NativeRawEvidence(unittest.TestCase):
    """Gate-3 AUD-4F1-REL-001: the raw native objdump text is mandatory for Linux and re-derived by the consumer."""

    def linux(self, f, **kw):
        f.job("test-linux")
        f.job("build-linux-x86_64")
        f.artifact("linux-x86_64", **kw)
        return f.evaluate(optional_targets="none")

    def test_missing_raw_native_output_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux(Fixture(tmp), mutate=lambda ev: ev.pop("native_tool_output"))
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")

    def test_raw_native_output_contradicting_the_derived_floor_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            def contradict(ev):
                ev["native_tool_output"] = "GLIBC_2.17\nGLIBC_2.99\n"  # derived field left at 2.30
            m = self.linux(Fixture(tmp), mutate=contradict)
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("raw" in r for r in m["targets"]["linux-x86_64"]["reasons"]))
            self.assertEqual(release_manifest.render_sha256sums(m), "")

    def test_raw_native_output_without_glibc_tokens_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            def blank(ev):
                ev["native_tool_output"] = "ELF 64-bit LSB pie executable\n"
                ev["binary"]["abi"]["native_glibc_floor"] = "2.30"  # derived claims a floor the raw text lacks
            m = self.linux(Fixture(tmp), mutate=blank)
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")

    def test_consistent_raw_derived_and_measured_floor_is_eligible(self):
        with tempfile.TemporaryDirectory() as tmp:
            m = self.linux(Fixture(tmp))
            self.assertEqual(status(m, "linux-x86_64"), "ELIGIBLE")


class RunnerIdentity(unittest.TestCase):
    """Gate-3 INSPECTOR-4F1-001: the producer must have run on the target's native runner."""

    def test_non_native_runner_is_inconsistent_for_every_eligible_capable_target(self):
        cases = [
            ("linux-x86_64", "test-linux", "build-linux-x86_64", {"os": "Linux", "arch": "ARM64"}),
            ("macos-aarch64", "test-macos-aarch64", "build-macos-aarch64", {"os": "Linux", "arch": "X64"}),
            ("windows-x86_64", "test-windows-x86_64", "build-windows-x86_64", {"os": "Linux", "arch": "X64"}),
        ]
        for target, test_job, build_job, wrong in cases:
            with tempfile.TemporaryDirectory() as tmp:
                f = Fixture(tmp)
                if target != "linux-x86_64":
                    f.required_ok()
                f.job(test_job)
                f.job(build_job, artifact_id="778")
                f.artifact(target, env=producer_env(build_job, runner=wrong))
                m = f.evaluate(optional_targets="eligible")
                self.assertEqual(status(m, target), "INCONSISTENT", target)
                self.assertTrue(any("runner" in r for r in m["targets"][target]["reasons"]), target)

    def test_native_runner_is_eligible(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-macos-aarch64")
            f.job("build-macos-aarch64", artifact_id="778")
            f.artifact("macos-aarch64")
            m = f.evaluate()
            self.assertEqual(status(m, "macos-aarch64"), "ELIGIBLE")
            self.assertEqual(m["targets"]["macos-aarch64"]["producer_runner"], "macOS/ARM64")

    def test_empty_runner_fields_are_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", mutate=lambda ev: ev["provenance"].update(runner_os="", runner_arch=""))
            self.assertEqual(status(f.evaluate(optional_targets="none"), "linux-x86_64"), "INCONSISTENT")


class SymlinkedDirectories(unittest.TestCase):
    """Gate-3 AUD-4F1-REL-002: the target directory and the dist root must be real directories."""

    def test_symlinked_target_directory_to_a_valid_external_directory_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            outside = pathlib.Path(tmp, "outside")
            (f.dist / "linux-x86_64").rename(outside)
            (f.dist / "linux-x86_64").symlink_to(outside, target_is_directory=True)
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("symlink" in r for r in m["targets"]["linux-x86_64"]["reasons"]))
            self.assertEqual(release_manifest.render_sha256sums(m), "")

    def test_symlinked_dist_root_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            real = pathlib.Path(tmp, "real-dist")
            f.dist.rename(real)
            f.dist.symlink_to(real, target_is_directory=True)
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")


class TreeCleanliness(unittest.TestCase):
    """Gate-3 ARCH-REL-PROVENANCE-001: the producer's tracked tree must have been clean after the build."""

    def test_dirty_tracked_tree_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", tree_status=" M vendor/libghostty-vt/build.zig\n")
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("tracked tree" in r for r in m["targets"]["linux-x86_64"]["reasons"]))

    def test_missing_tree_clean_field_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", mutate=lambda ev: ev["provenance"].pop("tree_clean"))
            self.assertEqual(status(f.evaluate(optional_targets="none"), "linux-x86_64"), "INCONSISTENT")

    def test_tree_clean_true_with_a_non_empty_status_is_inconsistent(self):
        # Codex in-flight P2 at c628f63: the derived flag must not be trusted over the raw status.
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", mutate=lambda ev: ev["provenance"].update(tree_clean=True, tree_status=" M src/main.rs"))
            m = f.evaluate(optional_targets="none")
            self.assertFalse(m["ok"])
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertEqual(release_manifest.render_sha256sums(m), "")

    def test_optional_tree_contradiction_excludes_only_that_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            f.job("test-windows-x86_64")
            f.job("build-windows-x86_64", artifact_id="779")
            f.artifact("windows-x86_64", mutate=lambda ev: ev["provenance"].update(tree_clean=True, tree_status="?? junk\n M a"))
            m = f.evaluate()
            self.assertTrue(m["ok"])
            self.assertEqual(status(m, "windows-x86_64"), "INCONSISTENT")
            self.assertEqual(status(m, "linux-x86_64"), "ELIGIBLE")
            self.assertEqual(release_manifest.render_sha256sums(m).count("\n"), 1)

    def test_cli_tree_contradiction_fails_with_empty_sums(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", mutate=lambda ev: ev["provenance"].update(tree_clean=True, tree_status=" M src/main.rs"))
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out, optional_targets="none")
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("result=FAIL", (out / "RELEASE_MANIFEST.txt").read_text())
            self.assertEqual((out / "SHA256SUMS").read_text(), "")

    def test_empty_status_with_true_flag_is_the_positive_control(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", mutate=lambda ev: ev["provenance"].update(tree_clean=True, tree_status="  \n"))
            m = f.evaluate(optional_targets="none")
            self.assertEqual(status(m, "linux-x86_64"), "ELIGIBLE")

    def test_tree_clean_string_true_is_not_accepted(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", mutate=lambda ev: ev["provenance"].update(tree_clean="true"))
            self.assertEqual(status(f.evaluate(optional_targets="none"), "linux-x86_64"), "INCONSISTENT")


class SidecarSemantics(unittest.TestCase):
    """Gate-3 ARB-4F1-SIDECAR-SEMANTICS-001: sizes match the bytes; a producer attempt never exceeds the manifest's."""

    def test_wrong_sizes_are_inconsistent(self):
        for field in ("archive", "binary"):
            with tempfile.TemporaryDirectory() as tmp:
                f = Fixture(tmp)
                f.job("test-linux")
                f.job("build-linux-x86_64")
                f.artifact("linux-x86_64", mutate=lambda ev, field=field: ev[field].update(size=0))
                m = f.evaluate(optional_targets="none")
                self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT", field)
                self.assertTrue(any("size" in r for r in m["targets"]["linux-x86_64"]["reasons"]), field)

    def test_future_producer_attempt_is_inconsistent(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", env=producer_env("build-linux-x86_64", attempt="999"))
            m = f.evaluate(optional_targets="none", run_attempt=2)
            self.assertEqual(status(m, "linux-x86_64"), "INCONSISTENT")
            self.assertTrue(any("attempt" in r for r in m["targets"]["linux-x86_64"]["reasons"]))

    def test_same_attempt_is_eligible(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.job("test-linux")
            f.job("build-linux-x86_64")
            f.artifact("linux-x86_64", env=producer_env("build-linux-x86_64", attempt="2"))
            self.assertEqual(status(f.evaluate(optional_targets="none", run_attempt=2), "linux-x86_64"), "ELIGIBLE")


class Rendering(unittest.TestCase):
    def test_text_manifest_names_every_target_and_the_result(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            m = f.evaluate(optional_targets="none")
            text = release_manifest.render_text(m)
            for t in release_binary.TARGETS:
                self.assertIn(f"target={t} ", text)
            self.assertIn("result=OK", text)
            self.assertIn(f"git_sha={SHA}", text)
            self.assertIn("producer_run_attempt=1", text)

    def test_cli_writes_outputs_and_exit_code_reflects_the_required_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(tmp)
            f.required_ok()
            out = pathlib.Path(tmp, "out")
            proc = f.cli(out, optional_targets="none")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertTrue((out / "RELEASE_MANIFEST.txt").exists())
            self.assertTrue((out / "RELEASE_MANIFEST.json").exists())
            self.assertIn("linux-x86_64.tar.gz", (out / "SHA256SUMS").read_text())
            shutil.rmtree(f.dist / "linux-x86_64")
            proc = f.cli(out, optional_targets="none")
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("result=FAIL", (out / "RELEASE_MANIFEST.txt").read_text())


if __name__ == "__main__":
    unittest.main()
