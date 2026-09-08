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

# GitHub artifact ids are integers; the pinned download action parses them with JavaScript, so anything beyond
# Number.MAX_SAFE_INTEGER (2^53 - 1) is not represented exactly. Exporter and consumer share this bound.
MAX_ARTIFACT_ID = 9007199254740991
_ARTIFACT_ID = re.compile(r"^[1-9][0-9]{0,15}$")


def valid_artifact_id(value) -> bool:
    """Exactly one positive artifact id that the pinned download action represents exactly."""
    return isinstance(value, str) and bool(_ARTIFACT_ID.match(value)) and int(value) <= MAX_ARTIFACT_ID

# target -> tier, applicable test job (None = no hosted test evidence), build job, archive/member names, header
# facts, and the producer runner (RUNNER_OS/RUNNER_ARCH) the evidence must have been produced on.
TARGETS = {
    "linux-x86_64": {
        "tier": "required", "test_job": "test-linux", "build_job": "build-linux-x86_64",
        "archive": "zynk-v{version}-linux-x86_64.tar.gz", "member": "zynk",
        "format": "elf", "cpu": "x86_64", "os": "linux", "glibc_max": LINUX_GLIBC_MAX,
        "runner": {"os": "Linux", "arch": "X64"},
    },
    "macos-aarch64": {
        "tier": "optional", "test_job": "test-macos-aarch64", "build_job": "build-macos-aarch64",
        "archive": "zynk-v{version}-macos-aarch64.tar.gz", "member": "zynk",
        "format": "macho", "cpu": "aarch64", "os": "macos", "glibc_max": None,
        "runner": {"os": "macOS", "arch": "ARM64"},
    },
    "windows-x86_64": {
        "tier": "optional", "test_job": "test-windows-x86_64", "build_job": "build-windows-x86_64",
        "archive": "zynk-v{version}-windows-x86_64.zip", "member": "zynk.exe",
        "format": "pe", "cpu": "x86_64", "os": "windows", "glibc_max": None,
        "runner": {"os": "Windows", "arch": "X64"},
    },
    "macos-x86_64": {
        "tier": "optional", "test_job": None, "build_job": "build-macos-x86_64",
        "archive": "zynk-v{version}-macos-x86_64.tar.gz", "member": "zynk",
        "format": "macho", "cpu": "x86_64", "os": "macos", "glibc_max": None,
        "runner": {"os": "macOS", "arch": "ARM64"},  # Apple-silicon runner; Rosetta execution only, never eligible
    },
    "linux-aarch64": {
        "tier": "optional", "test_job": None, "build_job": "build-linux-aarch64",
        "archive": "zynk-v{version}-linux-aarch64.tar.gz", "member": "zynk",
        "format": "elf", "cpu": "aarch64", "os": "linux", "glibc_max": LINUX_GLIBC_MAX,
        "runner": {"os": "Linux", "arch": "X64"},  # cross-built, never executed, never eligible
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


_GLIBC_VERSION = re.compile(r"^GLIBC_(\d+(?:\.\d+)+)$")
_GLIBC_TOKEN = re.compile(r"GLIBC_(\d+(?:\.\d+)+)")
_SHT_GNU_VERNEED = 0x6FFFFFFE
_PT_INTERP = 3


def _cstring(data: bytes, offset: int) -> str:
    end = data.find(b"\0", offset)
    if offset < 0 or offset > len(data) or end < 0:
        raise ValueError("string table entry out of bounds")
    return data[offset:end].decode("utf-8", errors="replace")


def _inspect_elf(data: bytes) -> dict:
    """ELF64 facts from the real metadata: CPU from e_machine, the interpreter from PT_INTERP, and the glibc
    requirement from the .gnu.version_r (verneed) entries — never from strings found in the file body."""
    if len(data) < 64:
        raise ValueError("ELF header truncated")
    if data[4] != 2:
        raise ValueError(f"only ELF64 release binaries are expected (class byte {data[4]})")
    endian = {1: "<", 2: ">"}.get(data[5])
    if endian is None:
        raise ValueError("unknown ELF byte order")
    (_, machine, _, _, phoff, shoff, _, _, phentsize, phnum, shentsize, shnum, _) = struct.unpack_from(
        endian + "HHIQQQIHHHHHH", data, 16)
    interpreter = None
    for i in range(phnum):
        p_type, _, p_offset, _, _, p_filesz, _, _ = struct.unpack_from(endian + "IIQQQQQQ", data, phoff + i * phentsize)
        if p_type == _PT_INTERP:
            if p_offset + p_filesz > len(data):
                raise ValueError("PT_INTERP out of bounds")
            interpreter = data[p_offset:p_offset + p_filesz].split(b"\0", 1)[0].decode("utf-8", errors="replace")
    sections = [struct.unpack_from(endian + "IIQQQQIIQQ", data, shoff + i * shentsize) for i in range(shnum)]
    versions: list[str] = []
    for (_, sh_type, _, _, sh_offset, _, sh_link, sh_info, _, _) in sections:
        if sh_type != _SHT_GNU_VERNEED:
            continue
        if sh_link >= len(sections):
            raise ValueError("verneed string table link out of range")
        str_offset = sections[sh_link][4]
        offset = sh_offset
        for _ in range(sh_info):
            _, vn_cnt, _, vn_aux, vn_next = struct.unpack_from(endian + "HHIII", data, offset)
            aux = offset + vn_aux
            for _ in range(vn_cnt):
                _, _, _, vna_name, vna_next = struct.unpack_from(endian + "IHHII", data, aux)
                versions.append(_cstring(data, str_offset + vna_name))
                if vna_next == 0:
                    break
                aux += vna_next
            if vn_next == 0:
                break
            offset += vn_next
    glibc = [m.group(1) for m in (_GLIBC_VERSION.match(v) for v in versions) if m]
    floor = max(glibc, key=_glibc_key) if glibc else None
    if interpreter is None:
        libc = "static"
    elif "ld-musl" in interpreter:
        libc = "musl"
    elif "ld-linux" in interpreter:
        libc = "glibc"
    else:
        libc = "unknown"
    return {
        "format": "elf", "cpu": _ELF_MACHINES.get(machine, f"0x{machine:x}"), "os": "linux",
        "abi": {"class": "ELF64", "libc": libc, "interpreter": interpreter, "glibc_versions": glibc,
                "glibc_floor": floor},
    }


def native_glibc_floor(text: str | None) -> str | None:
    """The highest GLIBC_x.y[.z] token in native tool output (`objdump -T` lines), or None when there is none."""
    if not text:
        return None
    found = _GLIBC_TOKEN.findall(text)
    return max(found, key=_glibc_key) if found else None


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
        if offset + size > len(data):
            raise ValueError("Mach-O load command truncated")
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
    """Format, CPU, OS and ABI facts read from the executable's headers. Never executes anything; every decoding
    failure surfaces as ValueError so a caller can contain it per target."""
    try:
        if data[:4] == b"\x7fELF":
            return _inspect_elf(data)
        if data[:4] == b"\xcf\xfa\xed\xfe":
            return _inspect_macho(data)
        if data[:2] == b"MZ":
            return _inspect_pe(data)
    except (struct.error, IndexError, KeyError, UnicodeDecodeError) as err:
        raise ValueError(f"malformed executable header: {err}") from err
    raise ValueError("not an ELF, Mach-O 64-bit or PE executable")


def extract_single_member(path: pathlib.Path) -> tuple[str, bytes]:
    """The archive's one regular file (name, bytes); anything else is a packaging error."""
    path = pathlib.Path(path)
    try:
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
    except (zipfile.BadZipFile, zipfile.LargeZipFile, tarfile.TarError, EOFError, OSError) as err:
        raise ValueError(f"{path.name}: unreadable archive: {err}") from err
