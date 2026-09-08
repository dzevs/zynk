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


def fake_elf(machine=0x3E, glibc=("GLIBC_2.17", "GLIBC_2.30"), interp=b"/lib64/ld-linux-x86-64.so.2", body=b""):
    """A minimal but structurally real ELF64: a PT_INTERP program header and, when `glibc` names versions,
    a .gnu.version_r (verneed) section for libc.so.6 with those entries — the metadata objdump -T reports."""
    ehdr_size, phdr_size, shdr_size = 64, 56, 64
    versions = list(glibc)
    # .dynstr: "\0libc.so.6\0<versions...>\0"
    dynstr = b"\0libc.so.6\0" + b"".join(v.encode() + b"\0" for v in versions)
    off_libname = 1
    off_versions = []
    cursor = len(b"\0libc.so.6\0")
    for v in versions:
        off_versions.append(cursor)
        cursor += len(v) + 1
    verneed = b""
    if versions:
        aux = b""
        for i, name_off in enumerate(off_versions):
            vna_next = 16 if i + 1 < len(off_versions) else 0
            aux += struct.pack("<IHHII", 0, 0, 2 + i, name_off, vna_next)
        verneed = struct.pack("<HHIII", 1, len(versions), off_libname, 16, 0) + aux
    shstrtab = b"\0.interp\0.dynstr\0.gnu.version_r\0.shstrtab\0"
    phoff = ehdr_size
    interp_off = phoff + phdr_size
    interp_blob = (interp or b"") + b"\0"
    dynstr_off = interp_off + len(interp_blob)
    verneed_off = dynstr_off + len(dynstr)
    body_off = verneed_off + len(verneed)
    shstr_off = body_off + len(body)
    shoff = shstr_off + len(shstrtab)
    shoff += (-shoff) % 8
    sections = [
        (0, 0, 0, 0, 0, 0, 0, 0),  # null
        (shstrtab.index(b".interp"), 1, 2, 0, interp_off, len(interp_blob), 0, 0),
        (shstrtab.index(b".dynstr"), 3, 2, 0, dynstr_off, len(dynstr), 0, 0),
        (shstrtab.index(b".gnu.version_r"), 0x6FFFFFFE, 2, 0, verneed_off, len(verneed), 2, 1 if versions else 0),
        (shstrtab.index(b".shstrtab"), 3, 0, 0, shstr_off, len(shstrtab), 0, 0),
    ]
    if not versions:
        sections.pop(3)
    shstrndx = len(sections) - 1
    phnum = 1 if interp else 0
    ehdr = struct.pack("<16sHHIQQQIHHHHHH", b"\x7fELF" + bytes([2, 1, 1, 0]) + b"\0" * 8, 3, machine, 1, 0,
                       phoff if phnum else 0, shoff, 0, ehdr_size, phdr_size, phnum, shdr_size, len(sections), shstrndx)
    phdr = struct.pack("<IIQQQQQQ", 3, 4, interp_off, 0, 0, len(interp_blob), len(interp_blob), 1) if phnum else b"\0" * phdr_size
    blob = ehdr + phdr + interp_blob + dynstr + verneed + body + shstrtab
    blob += b"\0" * (shoff - len(blob))
    for (name, typ, flags, addr, offset, size, link, info) in sections:
        blob += struct.pack("<IIQQQQIIQQ", name, typ, flags, addr, offset, size, link, info, 1, 0)
    return blob


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
    "LIBGHOSTTY_VT_OPTIMIZE": "ReleaseFast",
    "LIBGHOSTTY_VT_SIMD": "false",
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

    def test_glibc_floor_comes_from_version_needs_not_from_strings(self):
        # A harmless "GLIBC_99.99" string in the program body must not become the floor (Codex Gate-2 #5).
        info = release_binary.inspect_binary(fake_elf(glibc=("GLIBC_2.2.5", "GLIBC_2.17"), body=b"log: GLIBC_99.99 seen\0"))
        self.assertEqual(info["abi"]["glibc_floor"], "2.17")
        self.assertEqual(info["abi"]["glibc_versions"], ["2.2.5", "2.17"])

    def test_patch_versions_are_kept_and_ordered_numerically(self):
        info = release_binary.inspect_binary(fake_elf(glibc=("GLIBC_2.2.5",)))
        self.assertEqual(info["abi"]["glibc_floor"], "2.2.5")
        self.assertTrue(release_binary.glibc_within("2.2.5", "2.30"))
        self.assertFalse(release_binary.glibc_within("2.30.1", "2.30"))

    def test_glibc_interpreter_without_version_needs_reports_no_floor(self):
        info = release_binary.inspect_binary(fake_elf(glibc=()))
        self.assertEqual(info["abi"]["libc"], "glibc")
        self.assertIsNone(info["abi"]["glibc_floor"])

    def test_static_elf_has_no_interpreter(self):
        info = release_binary.inspect_binary(fake_elf(glibc=(), interp=None))
        self.assertEqual(info["abi"]["libc"], "static")
        self.assertIsNone(info["abi"]["interpreter"])

    def test_truncated_headers_raise_value_error_not_struct_error(self):
        for blob in (fake_elf()[:100], fake_macho()[:40], fake_pe()[:0x90]):
            with self.assertRaises(ValueError):
                release_binary.inspect_binary(blob)

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

    def test_corrupt_archives_raise_value_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad_zip = pathlib.Path(tmp, "bad.zip")
            bad_zip.write_bytes(b"PK\x03\x04 not really a zip")
            bad_tgz = pathlib.Path(tmp, "bad.tar.gz")
            bad_tgz.write_bytes(b"\x1f\x8b garbage")
            for path in (bad_zip, bad_tgz):
                with self.assertRaises(ValueError):
                    release_binary.extract_single_member(path)

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
                env=PRODUCER_ENV, toolchain={"rustc": "rustc 1.98.1"},
                native_tool_output="ELF 64-bit LSB pie executable\nGLIBC_2.17\nGLIBC_2.30\n", tree_status="",
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
            self.assertEqual(ev["build_inputs"], {"libghostty_optimize": "ReleaseFast", "libghostty_simd": "false"})
            self.assertEqual(ev["binary"]["abi"]["native_glibc_floor"], "2.30")
            self.assertEqual(ev["native_tool_output"], "ELF 64-bit LSB pie executable\nGLIBC_2.17\nGLIBC_2.30")
            self.assertEqual(ev["provenance"]["runner_os"], "Linux")
            self.assertEqual(ev["provenance"]["runner_arch"], "X64")
            self.assertTrue(ev["provenance"]["tree_clean"])
            self.assertEqual(ev["provenance"]["tree_status"], "")

    def test_dirty_tree_status_is_recorded_as_not_clean(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            ev = release_evidence.build_evidence(
                target="linux-x86_64", version="3.1.0", archive=archive, exec_status="ran", exec_output="zynk 3.1.0",
                cargo_version="3.1.0", checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={},
                native_tool_output="GLIBC_2.30\n", tree_status=" M src/main.rs\n",
            )
            self.assertFalse(ev["provenance"]["tree_clean"])
            self.assertEqual(ev["provenance"]["tree_status"], " M src/main.rs")
            self.assertEqual(ev["native_tool_output"], "GLIBC_2.30")

    def test_native_tool_output_is_always_present(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            ev = release_evidence.build_evidence(
                target="linux-x86_64", version="3.1.0", archive=archive, exec_status="ran", exec_output="zynk 3.1.0",
                cargo_version="3.1.0", checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={}, tree_status="",
            )
            self.assertEqual(ev["native_tool_output"], "")
            self.assertIsNone(ev["binary"]["abi"]["native_glibc_floor"])

    def test_native_tool_output_disagreeing_with_the_headers_is_recorded_as_is(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            ev = release_evidence.build_evidence(
                target="linux-x86_64", version="3.1.0", archive=archive, exec_status="ran",
                exec_output="zynk 3.1.0", cargo_version="3.1.0", checkout_head="a" * 40, env=PRODUCER_ENV,
                toolchain={}, native_tool_output="GLIBC_2.17\nGLIBC_2.34\n", tree_status="",
            )
            self.assertEqual(ev["binary"]["abi"]["glibc_floor"], "2.30")
            self.assertEqual(ev["binary"]["abi"]["native_glibc_floor"], "2.34")

    def test_not_run_is_recorded_without_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            ev = release_evidence.build_evidence(
                target="linux-x86_64", version="3.1.0", archive=archive, exec_status="not_run",
                exec_output="", cargo_version="3.1.0", checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={},
                tree_status="",
            )
            self.assertEqual(ev["exec"], {"status": "not_run", "output": ""})

    def test_unknown_target_or_status_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            with self.assertRaises(ValueError):
                release_evidence.build_evidence(target="freebsd", version="3.1.0", archive=archive,
                                                exec_status="ran", exec_output="", cargo_version="3.1.0",
                                                checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={},
                                                tree_status="")
            with self.assertRaises(ValueError):
                release_evidence.build_evidence(target="linux-x86_64", version="3.1.0", archive=archive,
                                                exec_status="maybe", exec_output="", cargo_version="3.1.0",
                                                checkout_head="a" * 40, env=PRODUCER_ENV, toolchain={},
                                                tree_status="")

    def test_cli_writes_the_sidecar_next_to_the_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = self.make_archive(tmp)
            cargo = pathlib.Path(tmp, "Cargo.toml")
            cargo.write_text('[package]\nname = "zynk"\nversion = "3.1.0"\n')
            exec_out = pathlib.Path(tmp, "exec.txt")
            exec_out.write_text("zynk 3.1.0\n")
            tree = pathlib.Path(tmp, "tree-status.txt")
            tree.write_text("")
            out = pathlib.Path(tmp, "EVIDENCE.json")
            env = dict(os.environ, **PRODUCER_ENV)
            proc = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "release_evidence.py"), "--target", "linux-x86_64",
                 "--version", "3.1.0", "--archive", str(archive), "--exec-status", "ran",
                 "--exec-output-file", str(exec_out), "--cargo-toml", str(cargo), "--checkout-head", "a" * 40,
                 "--tree-status-file", str(tree), "--toolchain", "rustc=rustc 1.98.1", "--out", str(out)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            ev = json.loads(out.read_text())
            self.assertEqual(ev["cargo_version"], "3.1.0")
            self.assertEqual(ev["exec"]["output"], "zynk 3.1.0")
            self.assertEqual(ev["provenance"]["checkout_head"], "a" * 40)
            self.assertTrue(ev["provenance"]["tree_clean"])
            proc = subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "release_evidence.py"), "--target", "linux-x86_64",
                 "--version", "3.1.0", "--archive", str(archive), "--exec-status", "ran",
                 "--exec-output-file", str(exec_out), "--cargo-toml", str(cargo), "--checkout-head", "a" * 40,
                 "--out", str(out)], env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(proc.returncode, 0, "--tree-status-file is mandatory")


if __name__ == "__main__":
    unittest.main()
