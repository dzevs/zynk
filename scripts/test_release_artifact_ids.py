"""Artifact-id validation for the candidate workflow (ADR 0012, Codex Gate-2 #4): only a single numeric id from a
successful producer is exported for a download step; anything else exports nothing so no download-all can happen."""
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from scripts import release_artifact_ids  # noqa: E402


def producers(**jobs):
    out = {}
    for job, (result, artifact_id) in jobs.items():
        entry = {"result": result, "outputs": {}}
        if artifact_id is not None:
            entry["outputs"]["artifact-id"] = artifact_id
        out[job] = entry
    return out


class ExportIds(unittest.TestCase):
    def test_numeric_ids_of_successful_producers_are_exported(self):
        ids = release_artifact_ids.export_ids(producers(**{
            "build-linux-x86_64": ("success", "1234"),
            "build-windows-x86_64": ("success", "5678"),
        }))
        self.assertEqual(ids["linux-x86_64"], {"id": "1234", "valid": True, "reason": ""})
        self.assertEqual(ids["windows-x86_64"], {"id": "5678", "valid": True, "reason": ""})

    def test_missing_empty_or_non_numeric_ids_export_nothing(self):
        ids = release_artifact_ids.export_ids(producers(**{
            "build-linux-x86_64": ("success", ""),
            "build-macos-aarch64": ("success", None),
            "build-windows-x86_64": ("success", "12,34"),
            "build-linux-aarch64": ("failure", "999"),
            "build-macos-x86_64": ("skipped", None),
        }))
        for target in ids:
            self.assertFalse(ids[target]["valid"], target)
            self.assertEqual(ids[target]["id"], "", target)
        self.assertIn("empty", ids["linux-x86_64"]["reason"])
        self.assertIn("safe-integer", ids["windows-x86_64"]["reason"])
        self.assertIn("result=failure", ids["linux-aarch64"]["reason"])

    def test_ids_are_bounded_to_the_safe_integer_range(self):
        ids = release_artifact_ids.export_ids(producers(**{
            "build-linux-x86_64": ("success", "9007199254740991"),
            "build-windows-x86_64": ("success", "9007199254740993"),
            "build-macos-aarch64": ("success", "0"),
        }))
        self.assertTrue(ids["linux-x86_64"]["valid"])
        self.assertFalse(ids["windows-x86_64"]["valid"])
        self.assertFalse(ids["macos-aarch64"]["valid"])
        self.assertIn("safe", ids["windows-x86_64"]["reason"])

    def test_cli_writes_github_outputs_and_a_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            prod = pathlib.Path(tmp, "producers.json")
            prod.write_text(json.dumps(producers(**{"build-linux-x86_64": ("success", "42"),
                                                    "build-windows-x86_64": ("success", "x")})))
            gh_out = pathlib.Path(tmp, "gh_output")
            summary = pathlib.Path(tmp, "artifact-ids.json")
            proc = subprocess.run([sys.executable, str(ROOT / "scripts" / "release_artifact_ids.py"),
                                   "--producers", str(prod), "--github-output", str(gh_out), "--summary", str(summary)],
                                  capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            lines = gh_out.read_text().splitlines()
            self.assertIn("id_linux_x86_64=42", lines)
            self.assertIn("id_windows_x86_64=", lines)
            self.assertIn("id_macos_aarch64=", lines)
            self.assertFalse(json.loads(summary.read_text())["windows-x86_64"]["valid"])


if __name__ == "__main__":
    unittest.main()
