from __future__ import annotations

import io
import subprocess
import tarfile
import tempfile
import unittest
from types import SimpleNamespace
from pathlib import Path
from unittest import mock

from scripts.vendor_libghostty_vt import (
    ensure_dist_archive,
    extract_archive,
    parse_archive_root,
    require_clean_checkout,
    vendor_libghostty_vt,
)

ARCHIVE_ROOT = "libghostty-vt-1.0.0"


def _write_archive(archive: Path, members: list[tarfile.TarInfo]) -> None:
    """Build a .tar.gz whose members are taken verbatim, escapes and all."""
    with tarfile.open(archive, "w:gz") as tar:
        for info in members:
            if info.isreg():
                data = b"payload"
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            else:
                tar.addfile(info)


def _regular(name: str) -> tarfile.TarInfo:
    return tarfile.TarInfo(name)


def _symlink(name: str, target: str) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.type = tarfile.SYMTYPE
    info.linkname = target
    return info


def _hardlink(name: str, target: str) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.type = tarfile.LNKTYPE
    info.linkname = target
    return info


def _device(name: str) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.type = tarfile.CHRTYPE
    info.devmajor = 1
    info.devminor = 3
    return info


class UnsafeArchiveMemberTests(unittest.TestCase):
    """INSPECTOR-D03-003 / INSPECTOR-B1-003: only the first path component was ever validated, so a
    member like `libghostty-vt-1.0.0/../../escaped.txt` shared the expected root and still wrote
    outside the extraction directory. Every member is checked now, on every interpreter."""

    def _extract(self, members: list[tarfile.TarInfo]):
        """Extract into <tmp>/deep/root so an escaping member still lands inside <tmp>."""
        temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(temp_dir.cleanup)
        base = Path(temp_dir.name)
        archive = base / "dist.tar.gz"
        _write_archive(archive, members)
        root = base / "deep" / "root"
        root.mkdir(parents=True)
        return archive, root, base

    def _assert_refused(self, members: list[tarfile.TarInfo], escaped_name: str) -> None:
        archive, root, base = self._extract(members)
        with self.assertRaises(ValueError) as raised:
            extract_archive(archive, root)
        message = str(raised.exception)
        self.assertIn("refusing to extract", message)
        escaped = base / "deep" / escaped_name
        self.assertFalse(escaped.exists(), f"{escaped} was written outside the extraction root")

    def test_a_traversal_member_is_refused(self) -> None:
        self._assert_refused(
            [_regular(f"{ARCHIVE_ROOT}/README.md"), _regular(f"{ARCHIVE_ROOT}/../../escaped.txt")],
            "escaped.txt",
        )

    def test_an_absolute_member_is_refused(self) -> None:
        archive, root, _ = self._extract(
            [_regular(f"{ARCHIVE_ROOT}/README.md"), _regular("/etc/zynk-escaped.txt")]
        )
        with self.assertRaisesRegex(ValueError, "refusing to extract"):
            extract_archive(archive, root)
        self.assertFalse(Path("/etc/zynk-escaped.txt").exists())

    def test_a_symlink_escaping_the_root_is_refused(self) -> None:
        self._assert_refused(
            [
                _regular(f"{ARCHIVE_ROOT}/README.md"),
                _symlink(f"{ARCHIVE_ROOT}/link", "../../escaped.txt"),
            ],
            "escaped.txt",
        )

    def test_a_hardlink_escaping_the_root_is_refused(self) -> None:
        self._assert_refused(
            [
                _regular(f"{ARCHIVE_ROOT}/README.md"),
                _hardlink(f"{ARCHIVE_ROOT}/link", "../../escaped.txt"),
            ],
            "escaped.txt",
        )

    def test_a_device_member_is_refused(self) -> None:
        archive, root, _ = self._extract(
            [_regular(f"{ARCHIVE_ROOT}/README.md"), _device(f"{ARCHIVE_ROOT}/null")]
        )
        with self.assertRaisesRegex(ValueError, "refusing to extract"):
            extract_archive(archive, root)
        self.assertFalse((root / ARCHIVE_ROOT / "null").exists())

    def test_a_clean_archive_still_extracts(self) -> None:
        archive, root, _ = self._extract(
            [
                _regular(f"{ARCHIVE_ROOT}/README.md"),
                _regular(f"{ARCHIVE_ROOT}/src/lib_vt.zig"),
                _symlink(f"{ARCHIVE_ROOT}/src/alias.zig", "lib_vt.zig"),
                _hardlink(f"{ARCHIVE_ROOT}/src/hard.zig", f"{ARCHIVE_ROOT}/src/lib_vt.zig"),
            ]
        )
        extract_archive(archive, root)
        self.assertEqual((root / ARCHIVE_ROOT / "README.md").read_bytes(), b"payload")
        self.assertTrue((root / ARCHIVE_ROOT / "src" / "lib_vt.zig").exists())
        self.assertTrue((root / ARCHIVE_ROOT / "src" / "alias.zig").is_symlink())
        self.assertEqual((root / ARCHIVE_ROOT / "src" / "hard.zig").read_bytes(), b"payload")

    def test_a_clean_archive_extracts_without_tarfile_filters(self) -> None:
        with (
            mock.patch("scripts.vendor_libghostty_vt.tarfile", SimpleNamespace(open=tarfile.open)),
            mock.patch.object(tarfile.TarFile, "extraction_filter",
                              staticmethod(lambda member, path: member), create=True),
        ):
            self.test_a_clean_archive_still_extracts()

    def test_parent_components_are_refused_even_without_lexical_escape(self) -> None:
        archive, root, _ = self._extract([_regular(f"{ARCHIVE_ROOT}/src/../unexpected")])
        with self.assertRaisesRegex(ValueError, "refusing to extract"):
            extract_archive(archive, root)

    def test_link_descendants_are_refused_without_tarfile_filters(self) -> None:
        archive, root, base = self._extract([
            _symlink(f"{ARCHIVE_ROOT}/link", ".."),
            _regular(f"{ARCHIVE_ROOT}/link/../escaped.txt"),
        ])
        # Emulate the legacy unfiltered path, including on Python whose default
        # filter is now "data". The script must refuse before tarfile writes.
        with (
            mock.patch("scripts.vendor_libghostty_vt.tarfile", SimpleNamespace(open=tarfile.open)),
            mock.patch.object(tarfile.TarFile, "extraction_filter",
                              staticmethod(lambda member, path: member), create=True),
        ):
            with self.assertRaisesRegex(ValueError, "refusing to extract"):
                extract_archive(archive, root)
        self.assertFalse((base / "deep" / "escaped.txt").exists())

    def test_no_member_may_traverse_an_archive_link_in_either_order(self) -> None:
        directory = tarfile.TarInfo(f"{ARCHIVE_ROOT}/src")
        directory.type = tarfile.DIRTYPE
        members = [
            directory,
            _symlink(f"{ARCHIVE_ROOT}/link", "src"),
            _regular(f"{ARCHIVE_ROOT}/link/unexpected"),
        ]
        for order in (members, list(reversed(members))):
            archive, root, _ = self._extract(order)
            with self.subTest(first=order[0].name), self.assertRaisesRegex(ValueError, "refusing to extract"):
                extract_archive(archive, root)


class VendorReplacementTests(unittest.TestCase):
    def _fixture(self, members):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        base = Path(directory.name)
        archive = base / "dist.tar.gz"
        _write_archive(archive, members)
        destination = base / "vendor"
        destination.mkdir()
        (destination / "previous").write_bytes(b"keep this tree")
        return archive, destination

    def _vendor(self, archive, destination):
        with (
            mock.patch("scripts.vendor_libghostty_vt.ensure_dist_archive", return_value=archive),
            mock.patch("scripts.vendor_libghostty_vt.git_head", return_value="a" * 40),
        ):
            return vendor_libghostty_vt(archive.parent, destination)

    def test_a_parent_link_cannot_destroy_the_existing_vendor(self) -> None:
        archive, destination = self._fixture([
            _regular(f"{ARCHIVE_ROOT}/README.md"),
            _symlink(f"{ARCHIVE_ROOT}/link", ".."),
        ])
        with self.assertRaises(Exception) as rejected:
            self._vendor(archive, destination)
        self.assertTrue((destination / "previous").exists(),
                        "the rejected archive destroyed the previous vendor tree")
        self.assertIsInstance(rejected.exception, ValueError)
        self.assertEqual((destination / "previous").read_bytes(), b"keep this tree")

    def test_a_staging_copy_failure_preserves_the_existing_vendor(self) -> None:
        archive, destination = self._fixture([_regular(f"{ARCHIVE_ROOT}/README.md")])
        with mock.patch("scripts.vendor_libghostty_vt.shutil.copytree", side_effect=OSError("copy failed")):
            with self.assertRaisesRegex(OSError, "copy failed"):
                self._vendor(archive, destination)
        self.assertTrue((destination / "previous").exists(),
                        "copy failure removed the old vendor before the new one was ready")

    def test_a_valid_archive_replaces_the_existing_vendor(self) -> None:
        archive, destination = self._fixture([_regular(f"{ARCHIVE_ROOT}/README.md")])
        metadata = self._vendor(archive, destination)
        self.assertEqual((destination / "README.md").read_bytes(), b"payload")
        self.assertFalse((destination / "previous").exists())
        self.assertEqual(metadata.source_commit, "a" * 40)

    def test_failed_install_renames_the_previous_vendor_back(self) -> None:
        archive, destination = self._fixture([_regular(f"{ARCHIVE_ROOT}/README.md")])
        rename = Path.rename

        def fail_install(path, target):
            if path.name == "new":
                raise OSError("install failed")
            return rename(path, target)

        with mock.patch.object(Path, "rename", fail_install):
            with self.assertRaisesRegex(OSError, "install failed"):
                self._vendor(archive, destination)
        self.assertEqual((destination / "previous").read_bytes(), b"keep this tree")

    def test_failed_rollback_retains_the_backup_outside_cleanup(self) -> None:
        archive, destination = self._fixture([_regular(f"{ARCHIVE_ROOT}/README.md")])
        rename = Path.rename

        def fail_install_and_rollback(path, target):
            if path.name in ("new", "previous"):
                raise OSError("rename failed")
            return rename(path, target)

        with mock.patch.object(Path, "rename", fail_install_and_rollback):
            with self.assertRaisesRegex(OSError, "backup retained"):
                self._vendor(archive, destination)
        backups = list(destination.parent.glob(".vendor-stage-*/previous/previous"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), b"keep this tree")


class VendorLibghosttyVtTests(unittest.TestCase):
    def test_parse_archive_root_returns_single_top_level_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            archive = Path(temp_dir) / "libghostty-vt.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                data = b"hello"
                info = tarfile.TarInfo("libghostty-vt-1.0.0/README.md")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))

            self.assertEqual(parse_archive_root(archive), "libghostty-vt-1.0.0")

    def test_ensure_dist_archive_refuses_stale_archives_without_head_match(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            dist = repo / "zig-out" / "dist"
            dist.mkdir(parents=True)
            (dist / "libghostty-vt-1.3.2-main-+deadbeef0.tar.gz").write_bytes(b"stale")

            def git_output(command: list[str], **_kwargs: object) -> str:
                if command[1] == "status":
                    return ""
                return "0123456789abcdef\n"

            with (
                mock.patch("scripts.vendor_libghostty_vt.subprocess.run"),
                mock.patch(
                    "scripts.vendor_libghostty_vt.subprocess.check_output",
                    side_effect=git_output,
                ),
            ):
                with self.assertRaisesRegex(FileNotFoundError, "HEAD 012345678"):
                    ensure_dist_archive(repo)

    def test_require_clean_checkout_rejects_tracked_and_untracked_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            with mock.patch(
                "scripts.vendor_libghostty_vt.subprocess.check_output",
                return_value=" M src/terminal.zig\n?? local.patch\n",
            ):
                with self.assertRaisesRegex(ValueError, "refusing to vendor from dirty checkout"):
                    require_clean_checkout(repo)

    def test_ensure_dist_archive_rejects_checkout_dirtied_by_build(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            with (
                mock.patch("scripts.vendor_libghostty_vt.subprocess.run"),
                mock.patch(
                    "scripts.vendor_libghostty_vt.subprocess.check_output",
                    side_effect=["", "0123456789abcdef\n", " M generated.txt\n"],
                ),
            ):
                with self.assertRaisesRegex(ValueError, "refusing to vendor from dirty checkout"):
                    ensure_dist_archive(repo)

    def test_vendored_tree_contains_required_upstream_files(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "libghostty-vt"
        required = [
            root / "build.zig",
            root / "build.zig.zon",
            root / "CMakeLists.txt",
            root / "dist" / "cmake" / "ghostty-vt-config.cmake.in",
            root / "include" / "ghostty" / "vt.h",
            root / "include" / "ghostty" / "vt" / "render.h",
            root / "src" / "lib_vt.zig",
        ]

        missing = [str(path.relative_to(root)) for path in required if not path.exists()]
        self.assertEqual(missing, [])

    def test_vendor_metadata_exists_and_points_at_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        metadata = project_root / "vendor" / "libghostty-vt.vendor.json"
        self.assertTrue(metadata.exists())
        text = metadata.read_text()
        self.assertIn('"source_commit"', text)
        self.assertIn('"dist_archive"', text)
        self.assertIn('"extracted_dir"', text)

    def test_local_vendor_patches_are_listed_in_patch_index(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        index = project_root / "vendor" / "libghostty-vt.patches.md"
        patch_dir = project_root / "vendor" / "patches" / "libghostty-vt"
        patches = sorted(patch_dir.glob("*.patch"))

        if not patches:
            return

        self.assertTrue(index.exists())
        text = index.read_text()
        missing = [
            str(path.relative_to(project_root))
            for path in patches
            if str(path.relative_to(project_root)) not in text
        ]
        self.assertEqual(missing, [])

    def test_local_vendor_patches_are_applied_to_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        patch_dir = project_root / "vendor" / "patches" / "libghostty-vt"

        for patch in sorted(patch_dir.glob("*.patch")):
            result = subprocess.run(
                ["git", "apply", "--check", "--reverse", str(patch.relative_to(project_root))],
                cwd=project_root,
                text=True,
                capture_output=True,
            )
            self.assertEqual(
                result.returncode,
                0,
                f"{patch.relative_to(project_root)} is not applied cleanly:\n"
                f"stdout:\n{result.stdout}\n"
                f"stderr:\n{result.stderr}",
            )

    def test_embedded_libghostty_logging_is_silenced(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "libghostty-vt"
        lib_vt = root / "src" / "lib_vt.zig"
        sys_zig = root / "src" / "terminal" / "c" / "sys.zig"
        lib_text = lib_vt.read_text()
        sys_text = sys_zig.read_text()
        self.assertIn('.logFn = @import("terminal/c/sys.zig").logFn', lib_text)
        self.assertIn("if (global.log == null) return;", sys_text)


if __name__ == "__main__":
    unittest.main()
