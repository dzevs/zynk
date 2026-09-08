"""Producer-side release evidence (ADR 0012): binary inspection without execution, and the EVIDENCE.json sidecar
that binds the packaged archive/binary hashes to the CI checkout provenance. unittest style."""
import io
import json
import os
import pathlib
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from scripts import release_binary, release_evidence  # noqa: E402


def fake_elf(machine=0x3E, glibc=(b"GLIBC_2.17", b"GLIBC_2.30"), interp=b"/lib64/ld-linux-x86-64.so.2"):
    ident = b"\x7fELF" + bytes([2, 1, 1, 0]) + b"\0" * 8
    header = ident + struct.pack("<HH", 2, machine) + b"\0" * 44
    return header + b"\0".join(glibc) + b"\0" + interp + b"\0" + b"\0" * 32


def fake_macho(cputype=0x0100000C, minos=(11, 0, 0), sdk=(15, 0, 0)):
    cmd = struct.pack("<IIIII", 0x32, 24, 1, (minos[0] << 16) | (minos[1] << 8) | minos[2],
                      (sdk[0] << 16) | (sdk[1] << 8) | sdk[2]) + struct.pack("<I", 0)
    header = struct.pack("<IiiIIIII", 0xFEEDFACF, cputype, 0, 2, 1, len(cmd), 0, 0)
    return header + cmd + b"\0" * 32


def fake_pe(machine=0x8664, subsystem=3, os_ver=(6, 0), sub_ver=(6, 0)):
    dos = bytearray(0x80)
    dos[:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 0x80)
    coff = b"PE\0\0" + struct.pack("<HHIIIHH", machine, 1, 0, 0, 0, 112, 0)
    opt = bytearray(112)
    struct.pack_into("<H", opt, 0, 0x20B)
    struct.pack_into("<HHHHHH", opt, 40, os_ver[0], os_ver[1], 0, 0, sub_ver[0], sub_ver[1])
    struct.pack_into("<H", opt, 68, subsystem)
    return bytes(dos) + coff + bytes(opt) + b"\0" * 32


def write_targz(path, member, data):
    with tarfile.open(path, "w:gz") as tar:
        info = tarfile.TarInfo(member)
        info.size = len(data)
        info.mode = 0o755
        tar.addfile(info, io.BytesIO(data))


def write_zip(path, member, data):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(member, data)


PRODUCER_ENV = {
    "GITHUB_SHA": "a" * 40,
    "GITHUB_RUN_ID": "1001",
    "GITHUB_RUN_ATTEMPT": "1",
    "GITHUB_JOB": "build-linux-x86_64",
    "GITHUB_REPOSITORY": "dzevs/zynk",
    "RUNNER_OS": "Linux",
    "RUNNER_ARCH": "X64",
}


class InspectBinary(unittest.TestCase):
    def test_elf_reports_cpu_libc_and_glibc_floor(self):
        info = release_binary.inspect_binary(fake_elf())
        self.assertEqual((info["format"], info["cpu"], info["os"]), ("elf", "x86_64", "linux"))
        self.assertEqual(info["abi"]["libc"], "glibc")
        self.assertEqual(info["abi"]["glibc_floor"], "2.30")

    def test_elf_aarch64_and_musl(self):
        info = release_binary.inspect_binary(fake_elf(machine=0xB7, glibc=(), interp=b"/lib/ld-musl-aarch64.so.1"))
        self.assertEqual(info["cpu"], "aarch64")
        self.assertEqual(info["abi"]["libc"], "musl")
        self.assertIsNone(info["abi"]["glibc_floor"])

    def test_macho_reports_arch_and_min_os(self):
        info = release_binary.inspect_binary(fake_macho())
        self.assertEqual((info["format"], info["cpu"], info["os"]), ("macho", "aarch64", "macos"))
        self.assertEqual(info["abi"]["min_os"], "11.0.0")
        self.assertEqual(info["abi"]["sdk"], "15.0.0")
        self.assertEqual(release_binary.inspect_binary(fake_macho(cputype=0x01000007))["cpu"], "x86_64")

    def test_pe_reports_arch_subsystem_and_min_os(self):
        info = release_binary.inspect_binary(fake_pe())
        self.assertEqual((info["format"], info["cpu"], info["os"]), ("pe", "x86_64", "windows"))
        self.assertEqual(info["abi"]["subsystem"], 3)
        self.assertEqual(info["abi"]["min_os"], "6.0")

    def test_unknown_bytes_are_rejected(self):
        with self.assertRaises(ValueError):
            release_binary.inspect_binary(b"#!/bin/sh\necho no\n")

    def test_glibc_version_order_is_numeric(self):
        self.assertTrue(release_binary.glibc_within("2.30", "2.30"))
        self.assertTrue(release_binary.glibc_within("2.4", "2.30"))
        self.assertFalse(release_binary.glibc_within("2.34", "2.30"))


class SingleMemberArchive(unittest.TestCase):
    def test_targz_and_zip_yield_the_single_member(self):
        with tempfile.TemporaryDirectory() as tmp:
            tgz = pathlib.Path(tmp, "a.tar.gz")
            write_targz(tgz, "zynk", b"elf-bytes")
            self.assertEqual(release_binary.extract_single_member(tgz), ("zynk", b"elf-bytes"))
            zp = pathlib.Path(tmp, "a.zip")
            write_zip(zp, "zynk.exe", b"pe-bytes")
            self.assertEqual(release_binary.extract_single_member(zp), ("zynk.exe", b"pe-bytes"))

    def test_two_members_are_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            tgz = pathlib.Path(tmp, "a.tar.gz")
            with tarfile.open(tgz, "w:gz") as tar:
                for name in ("zynk", "README"):
                    info = tarfile.TarInfo(name)
                    info.size = 1
                    tar.addfile(info, io.BytesIO(b"x"))
            with self.assertRaises(ValueError):
                release_binary.extract_single_member(tgz)


class Sidecar(unittest.TestCase):
    def make_archive(self, tmp):
        archive = pathlib.Path(tmp, "zynk-v3.1.0-linux-x86_64.tar.gz")
        write_targz(archive, "zynk", fake_elf())
        return archive

    def test_sidecar_binds_hashes_version_and_provenance(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            ev = release_evidence.build_evidence(
                target="linux-x86_64", version="3.1.0", archive=archive, exec_status="ran",
                exec_output="zynk 3.1.0\n", cargo_version="3.1.0", checkout_head="a" * 40,
                env=PRODUCER_ENV, toolchain={"rustc": "rustc 1.98.1"}, native_tool_output="ELF 64-bit",
            )
            self.assertEqual(ev["schema"], 1)
            self.assertEqual(ev["target"], "linux-x86_64")
            self.assertEqual(ev["tier"], "required")
            self.assertEqual(ev["archive"]["sha256"], release_binary.sha256_file(archive))
            self.assertEqual(ev["binary"]["member"], "zynk")
            self.assertEqual(ev["binary"]["sha256"], release_binary.sha256_bytes(fake_elf()))
            self.assertEqual(ev["binary"]["cpu"], "x86_64")
            self.assertEqual(ev["binary"]["abi"]["glibc_floor"], "2.30")
            self.assertEqual(ev["exec"], {"status": "ran", "output": "zynk 3.1.0"})
            self.assertEqual(ev["provenance"]["git_sha"], "a" * 40)
            self.assertEqual(ev["provenance"]["run_attempt"], 1)
            self.assertEqual(ev["provenance"]["job"], "build-linux-x86_64")
            self.assertEqual(ev["toolchain"], {"rustc": "rustc 1.98.1"})

    def test_not_run_is_recorded_without_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            ev = release_evidence.build_evidence(
                target="linux-x86_64", version="3.1.0", archive=archive, exec_status="not_run",
                exec_output="", cargo_version="3.1.0", checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={},
            )
            self.assertEqual(ev["exec"], {"status": "not_run", "output": ""})

    def test_unknown_target_or_status_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            with self.assertRaises(ValueError):
                release_evidence.build_evidence(target="freebsd", version="3.1.0", archive=archive,
                                                exec_status="ran", exec_output="", cargo_version="3.1.0",
                                                checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={})
            with self.assertRaises(ValueError):
                release_evidence.build_evidence(target="linux-x86_64", version="3.1.0", archive=archive,
                                                exec_status="maybe", exec_output="", cargo_version="3.1.0",
                                                checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={})

    def test_cli_writes_the_sidecar_next_to_the_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            cargo = pathlib.Path(tmp, "Cargo.toml")
            cargo.write_text('[package]\nname = "zynk"\nversion = "3.1.0"\n')
            exec_out = pathlib.Path(tmp, "exec.txt")
            exec_out.write_text("zynk 3.1.0\n")
            out = pathlib.Path(tmp, "EVIDENCE.json")
            env = dict(os.environ, **PRODUCER_ENV)
            proc = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "release_evidence.py"), "--target", "linux-x86_64",
                 "--version", "3.1.0", "--archive", str(archive), "--exec-status", "ran",
                 "--exec-output-file", str(exec_out), "--cargo-toml", str(cargo), "--checkout-head", "a" * 40,
                 "--toolchain", "rustc=rustc 1.98.1", "--out", str(out)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            ev = json.loads(out.read_text())
            self.assertEqual(ev["cargo_version"], "3.1.0")
            self.assertEqual(ev["exec"]["output"], "zynk 3.1.0")
            self.assertEqual(ev["provenance"]["checkout_head"], "a" * 40)


if __name__ == "__main__":
    unittest.main()
