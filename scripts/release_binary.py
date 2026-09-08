"""Release-artifact facts shared by the producer (`release_evidence.py`) and the consumer (`release_manifest.py`):
the target table of ADR 0012, single-member archive extraction, and binary-header inspection that never executes
the binary (ELF machine + glibc floor, Mach-O cputype + LC_BUILD_VERSION, PE machine + subsystem/min-OS)."""
from __future__ import annotations

import hashlib
import pathlib
import re
import struct
import tarfile
import zipfile

LINUX_GLIBC_MAX = "2.30"  # the published compatibility floor (README: "glibc >= 2.30")

# target -> tier, applicable test job (None = no hosted test evidence), build job, archive/member names, header facts.
TARGETS = {
    "linux-x86_64": {
        "tier": "required", "test_job": "test-linux", "build_job": "build-linux-x86_64",
        "archive": "zynk-v{version}-linux-x86_64.tar.gz", "member": "zynk",
        "format": "elf", "cpu": "x86_64", "os": "linux", "glibc_max": LINUX_GLIBC_MAX,
    },
    "macos-aarch64": {
        "tier": "optional", "test_job": "test-macos-aarch64", "build_job": "build-macos-aarch64",
        "archive": "zynk-v{version}-macos-aarch64.tar.gz", "member": "zynk",
        "format": "macho", "cpu": "aarch64", "os": "macos", "glibc_max": None,
    },
    "windows-x86_64": {
        "tier": "optional", "test_job": "test-windows-x86_64", "build_job": "build-windows-x86_64",
        "archive": "zynk-v{version}-windows-x86_64.zip", "member": "zynk.exe",
        "format": "pe", "cpu": "x86_64", "os": "windows", "glibc_max": None,
    },
    "macos-x86_64": {
        "tier": "optional", "test_job": None, "build_job": "build-macos-x86_64",
        "archive": "zynk-v{version}-macos-x86_64.tar.gz", "member": "zynk",
        "format": "macho", "cpu": "x86_64", "os": "macos", "glibc_max": None,
    },
    "linux-aarch64": {
        "tier": "optional", "test_job": None, "build_job": "build-linux-aarch64",
        "archive": "zynk-v{version}-linux-aarch64.tar.gz", "member": "zynk",
        "format": "elf", "cpu": "aarch64", "os": "linux", "glibc_max": LINUX_GLIBC_MAX,
    },
}

_ELF_MACHINES = {0x3E: "x86_64", 0xB7: "aarch64"}
_MACHO_CPUS = {0x0100000C: "aarch64", 0x01000007: "x86_64"}
_PE_MACHINES = {0x8664: "x86_64", 0xAA64: "aarch64"}


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    return sha256_bytes(pathlib.Path(path).read_bytes())


def _glibc_key(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("."))


def glibc_within(floor: str, maximum: str) -> bool:
    """True when a binary needing `floor` runs on a system that provides `maximum` (numeric, not lexical)."""
    return _glibc_key(floor) <= _glibc_key(maximum)


def _version_triple(value: int) -> str:
    return f"{value >> 16}.{(value >> 8) & 0xFF}.{value & 0xFF}"


def _inspect_elf(data: bytes) -> dict:
    if len(data) < 64:
        raise ValueError("ELF header truncated")
    elf_class = {1: "ELF32", 2: "ELF64"}.get(data[4])
    endian = {1: "<", 2: ">"}.get(data[5])
    if elf_class is None or endian is None:
        raise ValueError("unknown ELF class or byte order")
    (machine,) = struct.unpack_from(endian + "H", data, 18)
    floors = {m.decode() for m in re.findall(rb"GLIBC_(\d+\.\d+)", data)}
    floor = max(floors, key=_glibc_key) if floors else None
    if b"/ld-musl" in data:
        libc = "musl"
    elif b"/ld-linux" in data:
        libc = "glibc"
    else:
        libc = "static"
    return {
        "format": "elf", "cpu": _ELF_MACHINES.get(machine, f"0x{machine:x}"), "os": "linux",
        "abi": {"class": elf_class, "libc": libc, "glibc_floor": floor},
    }


def _inspect_macho(data: bytes) -> dict:
    if len(data) < 32:
        raise ValueError("Mach-O header truncated")
    cputype, _, _, ncmds, sizeofcmds = struct.unpack_from("<iiIII", data, 4)
    offset, end = 32, min(len(data), 32 + sizeofcmds)
    min_os = sdk = platform = None
    for _ in range(ncmds):
        if offset + 8 > end:
            break
        cmd, size = struct.unpack_from("<II", data, offset)
        if size < 8:
            raise ValueError("Mach-O load command with zero size")
        if cmd == 0x32 and offset + 20 <= len(data):  # LC_BUILD_VERSION
            platform, minos_raw, sdk_raw = struct.unpack_from("<III", data, offset + 8)
            min_os, sdk = _version_triple(minos_raw), _version_triple(sdk_raw)
        elif cmd == 0x24 and offset + 16 <= len(data):  # LC_VERSION_MIN_MACOSX
            minos_raw, sdk_raw = struct.unpack_from("<II", data, offset + 8)
            min_os, sdk, platform = _version_triple(minos_raw), _version_triple(sdk_raw), 1
        offset += size
    return {
        "format": "macho", "cpu": _MACHO_CPUS.get(cputype & 0xFFFFFFFF, f"0x{cputype & 0xFFFFFFFF:x}"),
        "os": "macos",
        "abi": {"platform": {1: "macos"}.get(platform, str(platform)), "min_os": min_os, "sdk": sdk},
    }


def _inspect_pe(data: bytes) -> dict:
    if len(data) < 0x40:
        raise ValueError("PE header truncated")
    (pe,) = struct.unpack_from("<I", data, 0x3C)
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("missing PE signature")
    (machine,) = struct.unpack_from("<H", data, pe + 4)
    opt = pe + 24
    (magic,) = struct.unpack_from("<H", data, opt)
    fmt = {0x20B: "PE32+", 0x10B: "PE32"}.get(magic)
    if fmt is None:
        raise ValueError("unknown PE optional-header magic")
    os_major, os_minor, _, _, sub_major, sub_minor = struct.unpack_from("<HHHHHH", data, opt + 40)
    (subsystem,) = struct.unpack_from("<H", data, opt + 68)
    return {
        "format": "pe", "cpu": _PE_MACHINES.get(machine, f"0x{machine:x}"), "os": "windows",
        "abi": {"image": fmt, "subsystem": subsystem, "min_os": f"{sub_major}.{sub_minor}",
                "os_version": f"{os_major}.{os_minor}"},
    }


def inspect_binary(data: bytes) -> dict:
    """Format, CPU, OS and ABI facts read from the executable's headers. Never executes anything."""
    if data[:4] == b"\x7fELF":
        return _inspect_elf(data)
    if data[:4] == b"\xcf\xfa\xed\xfe":
        return _inspect_macho(data)
    if data[:2] == b"MZ":
        return _inspect_pe(data)
    raise ValueError("not an ELF, Mach-O 64-bit or PE executable")


def extract_single_member(path: pathlib.Path) -> tuple[str, bytes]:
    """The archive's one regular file (name, bytes); anything else is a packaging error."""
    path = pathlib.Path(path)
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as zf:
            names = [n for n in zf.namelist() if not n.endswith("/")]
            if len(names) != 1:
                raise ValueError(f"expected exactly one member in {path.name}, found {names}")
            return names[0], zf.read(names[0])
    with tarfile.open(path, "r:gz") as tar:
        members = [m for m in tar.getmembers() if m.isfile()]
        if len(members) != 1 or len(tar.getmembers()) != 1:
            raise ValueError(f"expected exactly one member in {path.name}, found {[m.name for m in tar.getmembers()]}")
        handle = tar.extractfile(members[0])
        if handle is None:
            raise ValueError(f"unreadable member in {path.name}")
        return members[0].name, handle.read()
