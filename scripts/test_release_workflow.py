"""Structural guards for the candidate-evidence workflow and the required CI job (ADR 0012, Codex Gate-2 R2 #2):
every download step is bounded by its own timeout, continues on error, is guarded by a validated artifact id,
and the sum of the download bounds leaves the manifest job a documented reserve for checkout, id validation,
outcome recording, manifest generation and upload. Stdlib only: the workflow's own line layout is parsed."""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
CANDIDATE = ROOT / ".github" / "workflows" / "release-dryrun.yml"
CI = ROOT / ".github" / "workflows" / "ci.yml"

MANIFEST_RESERVE_MINUTES = 8  # after the worst-case download total: checkout, ids, outcome recording, manifest, upload


def job_block(text: str, job: str) -> str:
    match = re.search(rf"^  {re.escape(job)}:\n(.*?)(?=^  [a-z][a-z0-9_-]*:\n|\Z)", text, re.S | re.M)
    if not match:
        raise AssertionError(f"job {job!r} not found")
    return match.group(0)


def job_field(block: str, key: str):
    match = re.search(rf"^    {re.escape(key)}: (.*)$", block, re.M)
    return match.group(1).strip() if match else None


def steps(block: str) -> list[dict]:
    """Each step's top-level keys (8-space indent) as strings; `name` comes from the `- name:` line."""
    out = []
    for chunk in re.split(r"^      - ", block, flags=re.M)[1:]:
        step = {}
        first, _, rest = chunk.partition("\n")
        k, _, v = first.partition(":")
        step[k.strip()] = v.strip()
        for line in rest.splitlines():
            m = re.match(r"^        ([a-z-]+): ?(.*)$", line)
            if m:
                step[m.group(1)] = m.group(2).strip()
        out.append(step)
    return out


class ManifestJob(unittest.TestCase):
    def setUp(self):
        self.text = CANDIDATE.read_text()
        self.block = job_block(self.text, "manifest")
        self.steps = steps(self.block)
        self.downloads = [s for s in self.steps if s.get("name", "").startswith("Download ")]

    def test_every_download_step_is_bounded_guarded_and_non_fatal(self):
        self.assertEqual(len(self.downloads), 5, [s.get("name") for s in self.steps])
        for step in self.downloads:
            self.assertRegex(step.get("timeout-minutes", ""), r"^[1-9][0-9]*$", step["name"])
            self.assertEqual(step.get("continue-on-error"), "true", step["name"])
            self.assertRegex(step.get("if", ""), r"steps\.ids\.outputs\.id_[a-z0-9_]+ != ''", step["name"])
            self.assertRegex(step.get("id", ""), r"^dl-[a-z0-9_-]+$", step["name"])

    def test_download_bounds_leave_the_job_a_reserve(self):
        job_timeout = int(job_field(self.block, "timeout-minutes"))
        total = sum(int(s["timeout-minutes"]) for s in self.downloads)
        self.assertLessEqual(total + MANIFEST_RESERVE_MINUTES, job_timeout,
                             f"downloads {total} min + reserve {MANIFEST_RESERVE_MINUTES} min exceed job {job_timeout} min")

    def test_step_order_and_the_always_uploaded_manifest(self):
        names = [s.get("name", s.get("uses", "")) for s in self.steps]
        def index(prefix):
            return next(i for i, n in enumerate(names) if n.startswith(prefix))
        self.assertLess(index("Record producer results"), index("Validate artifact ids"))
        self.assertLess(index("Validate artifact ids"), index("Download linux-x86_64"))
        self.assertLess(max(i for i, n in enumerate(names) if n.startswith("Download ")), index("Record download outcomes"))
        self.assertLess(index("Record download outcomes"), index("Build the manifest"))
        self.assertLess(index("Build the manifest"), index("Upload manifest"))
        upload = next(s for s in self.steps if s.get("name", "").startswith("Upload manifest"))
        self.assertEqual(upload.get("if"), "${{ always() }}")
        build = next(s for s in self.steps if s.get("name", "").startswith("Build the manifest"))
        self.assertNotIn("continue-on-error", build)

    def test_manifest_only_runs_on_a_successful_required_tier(self):
        cond = job_field(self.block, "if")
        self.assertIn("needs.test-linux.result == 'success'", cond)
        self.assertIn("needs.build-linux-x86_64.result == 'success'", cond)
        self.assertIn("always()", cond)

    def test_producer_jobs_never_continue_on_error(self):
        for job in ("test-linux", "build-linux-x86_64", "test-macos-aarch64", "build-macos-aarch64",
                    "test-windows-x86_64", "build-windows-x86_64", "build-linux-aarch64", "build-macos-x86_64"):
            block = job_block(self.text, job)
            self.assertIsNone(job_field(block, "continue-on-error"), job)
            self.assertRegex(job_field(block, "timeout-minutes") or "", r"^[1-9][0-9]*$", job)
            for step in steps(block):
                self.assertNotIn("continue-on-error", step, f"{job}: {step.get('name')}")


class ProducerJobs(unittest.TestCase):
    """Gate-3 ARCH-REL-PROVENANCE-001 / INSPECTOR-4F1-001: producers assert a clean tracked tree after the build
    and before packaging/evidence, bind it into the sidecar, and the macOS-aarch64 jobs assert the ARM64 runner."""

    BUILD_JOBS = ("build-linux-x86_64", "build-macos-aarch64", "build-windows-x86_64", "build-linux-aarch64",
                  "build-macos-x86_64")

    def setUp(self):
        self.text = CANDIDATE.read_text()

    def names(self, job):
        return [s.get("name", s.get("uses", "")) for s in steps(job_block(self.text, job))]

    def test_every_build_job_asserts_a_clean_tracked_tree_between_build_and_packaging(self):
        for job in self.BUILD_JOBS:
            names = self.names(job)
            build = next(i for i, n in enumerate(names) if n.startswith("Build"))
            clean = next(i for i, n in enumerate(names) if n.startswith("Assert clean tracked tree"))
            package = next(i for i, n in enumerate(names) if n.startswith("Package"))
            evidence = next(i for i, n in enumerate(names) if n.startswith("Write evidence sidecar"))
            self.assertLess(build, clean, job)
            self.assertLess(clean, package, job)
            self.assertLess(clean, evidence, job)
            block = job_block(self.text, job)
            self.assertIn("git status --porcelain --untracked-files=no", block, job)
            self.assertIn("--tree-status-file", block, job)

    def test_linux_producers_record_native_objdump_evidence(self):
        for job in ("build-linux-x86_64", "build-linux-aarch64"):
            block = job_block(self.text, job)
            self.assertIn("objdump -T", block, job)
            self.assertIn("--native-tool-output-file", block, job)

    def test_macos_aarch64_jobs_assert_the_arm64_runner(self):
        for job in ("test-macos-aarch64", "build-macos-aarch64"):
            names = self.names(job)
            self.assertTrue(any(n.startswith("Assert Apple-silicon runner") for n in names), (job, names))
            self.assertIn('RUNNER_ARCH', job_block(self.text, job))

    def test_required_jobs_and_manifest_assert_the_candidate_sha_when_given(self):
        for job in ("test-linux", "build-linux-x86_64", "manifest"):
            names = self.names(job)
            self.assertTrue(any(n.startswith("Assert candidate SHA") for n in names), (job, names))
        self.assertIn("candidate_sha:", self.text)


class RequiredCi(unittest.TestCase):
    def test_check_required_runs_just_check_with_gitleaks_installed(self):
        block = job_block(CI.read_text(), "check-required")
        names = [s.get("name", "") for s in steps(block)]
        self.assertTrue(any(n.startswith("Install gitleaks") for n in names), names)
        run_steps = [s for s in steps(block) if s.get("run") == "just check"]
        self.assertEqual(len(run_steps), 1, names)

    def test_candidate_test_linux_runs_just_check_with_gitleaks_installed(self):
        block = job_block(CANDIDATE.read_text(), "test-linux")
        names = [s.get("name", "") for s in steps(block)]
        self.assertTrue(any(n.startswith("Install gitleaks") for n in names), names)
        self.assertTrue(any(s.get("run") == "just check" for s in steps(block)), names)


if __name__ == "__main__":
    unittest.main()
