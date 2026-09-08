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


def producer_env(job, run_id="1001", attempt="1", sha=SHA):
    return {"GITHUB_SHA": sha, "GITHUB_RUN_ID": run_id, "GITHUB_RUN_ATTEMPT": attempt, "GITHUB_JOB": job,
            "GITHUB_REPOSITORY": "dzevs/zynk", "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64"}


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
                 sidecar=True, member=None, cargo_version="3.1.0", checkout_head=SHA, extra_file=None):
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
        if sidecar:
            ev = release_evidence.build_evidence(
                target=target, version=version, archive=archive, exec_status=exec_status,
                exec_output=exec_output if exec_output is not None else f"zynk {version}\n",
                cargo_version=cargo_version, checkout_head=checkout_head,
                env=env or producer_env(spec["build_job"]), toolchain={"rustc": "rustc 1.98.1"},
            )
            (tdir / "EVIDENCE.json").write_text(json.dumps(ev))
        if extra_file:
            (tdir / extra_file).write_text("stray")
        return archive

    def required_ok(self):
        self.job("test-linux")
        self.job("build-linux-x86_64")
        self.artifact("linux-x86_64")

    def evaluate(self, **ctx_overrides):
        ctx = dict(CTX, **ctx_overrides)
        return release_manifest.evaluate(ctx, self.producers, self.dist, cargo_toml=self.cargo)


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
            f.artifact("linux-x86_64", binary=fake_elf(glibc=(b"GLIBC_2.17", b"GLIBC_2.34")))
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
            producers = pathlib.Path(tmp, "producers.json")
            producers.write_text(json.dumps(f.producers))
            out = pathlib.Path(tmp, "out")
            args = [sys.executable, str(ROOT / "scripts" / "release_manifest.py"), "--version", "3.1.0", "--sha", SHA,
                    "--run-id", "1001", "--run-attempt", "1", "--optional-targets", "none", "--producers",
                    str(producers), "--dist", str(f.dist), "--cargo-toml", str(f.cargo), "--out", str(out)]
            proc = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertTrue((out / "RELEASE_MANIFEST.txt").exists())
            self.assertTrue((out / "RELEASE_MANIFEST.json").exists())
            self.assertIn("linux-x86_64.tar.gz", (out / "SHA256SUMS").read_text())
            shutil.rmtree(f.dist / "linux-x86_64")
            proc = subprocess.run(args, capture_output=True, text=True)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("result=FAIL", (out / "RELEASE_MANIFEST.txt").read_text())


if __name__ == "__main__":
    unittest.main()
