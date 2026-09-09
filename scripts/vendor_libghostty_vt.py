from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import tarfile
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path, PurePosixPath


@dataclass
class VendorMetadata:
    source_commit: str
    dist_archive: str
    extracted_dir: str


def parse_archive_root(archive: Path) -> str:
    with tarfile.open(archive, "r:gz") as tar:
        roots = {
            member.name.split("/", 1)[0]
            for member in tar.getmembers()
            if member.name and member.name != "."
        }
    if len(roots) != 1:
        raise ValueError(f"expected exactly one archive root in {archive}, found {sorted(roots)}")
    return next(iter(roots))


def git_head(repo: Path) -> str:
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()


def require_clean_checkout(repo: Path) -> None:
    status = subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=repo,
        text=True,
    ).strip()
    if status:
        raise ValueError(f"refusing to vendor from dirty checkout {repo}:\n{status}")


def ensure_dist_archive(source_repo: Path) -> Path:
    require_clean_checkout(source_repo)
    head = git_head(source_repo)[:9]
    subprocess.run(
        ["zig", "build", "dist", "-Demit-lib-vt", "-Doptimize=ReleaseFast"],
        cwd=source_repo,
        check=True,
    )
    require_clean_checkout(source_repo)
    dist_dir = source_repo / "zig-out" / "dist"
    archives = sorted(dist_dir.glob(f"libghostty-vt-*+{head}.tar.gz"))
    if not archives:
        raise FileNotFoundError(
            f"no libghostty-vt dist archive for HEAD {head} found in {dist_dir}"
        )
    return archives[-1]


def _path_parts(name: str) -> tuple[str, ...]:
    """The meaningful components of an archive path, `.` and empty segments dropped."""
    return tuple(part for part in name.replace("\\", "/").split("/") if part not in ("", "."))


def _escapes_root(parts: tuple[str, ...]) -> bool:
    """True when walking ``parts`` ever steps above the directory it started in."""
    depth = 0
    for part in parts:
        if part == "..":
            depth -= 1
            if depth < 0:
                return True
        else:
            depth += 1
    return False


def check_archive_member(member: tarfile.TarInfo) -> None:
    """Refuse any member that could write outside the extraction root, or is not a plain file.

    `parse_archive_root` only ever looked at the FIRST path component, so a member such as
    `libghostty-vt-1.0.0/../../escaped.txt` shared the expected root and still escaped
    (INSPECTOR-D03-003 / INSPECTOR-B1-003).
    """
    name = member.name
    if not _path_parts(name):
        raise ValueError(f"refusing to extract archive member with an empty name: {name!r}")
    if name.startswith("/") or PurePosixPath(name).is_absolute():
        raise ValueError(f"refusing to extract absolute archive member {name!r}")
    parts = _path_parts(name)
    if _escapes_root(parts):
        raise ValueError(
            f"refusing to extract archive member {name!r}: it escapes the extraction root"
        )
    if member.ischr() or member.isblk() or member.isfifo() or member.isdev():
        raise ValueError(
            f"refusing to extract archive member {name!r}: device and fifo entries are not source"
        )
    if not (member.isreg() or member.isdir() or member.issym() or member.islnk()):
        raise ValueError(
            f"refusing to extract archive member {name!r} of unsupported type {member.type!r}"
        )
    if member.issym() or member.islnk():
        target = member.linkname
        if not target:
            raise ValueError(f"refusing to extract link member {name!r} with an empty target")
        if target.startswith("/") or PurePosixPath(target).is_absolute():
            raise ValueError(
                f"refusing to extract link member {name!r}: absolute target {target!r}"
            )
        # A symlink target is relative to the link's own directory; a hard link's is relative to
        # the archive root.
        base = parts[:-1] if member.issym() else ()
        if _escapes_root(base + _path_parts(target)):
            raise ValueError(
                f"refusing to extract link member {name!r}: target {target!r} escapes the "
                "extraction root"
            )


def extract_archive(archive: Path, destination: Path) -> None:
    """Extract the dist archive into ``destination`` after validating every member.

    The explicit per-member validation is the primary guard: `tarfile`'s extraction filters only
    exist from Python 3.12 and only became the default in 3.14, so on an older interpreter a plain
    `extractall` still honours `..` components, absolute names, links pointing anywhere and device
    entries. Where the filter is available it runs as well, as a second, independent check.
    """
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            check_archive_member(member)
        if hasattr(tarfile, "data_filter"):
            tar.extractall(destination, filter="data")
        else:
            tar.extractall(destination)


def vendor_libghostty_vt(source_repo: Path, destination: Path) -> VendorMetadata:
    archive = ensure_dist_archive(source_repo)
    root = parse_archive_root(archive)

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_dir_path = Path(temp_dir)
        extract_archive(archive, temp_dir_path)

        extracted = temp_dir_path / root
        if not extracted.exists():
            raise FileNotFoundError(f"expected extracted root {extracted}")

        if destination.exists():
            shutil.rmtree(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(extracted, destination)

    return VendorMetadata(
        source_commit=git_head(source_repo),
        dist_archive=archive.name,
        extracted_dir=root,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Vendor the pinned libghostty-vt source dist into zynk")
    parser.add_argument(
        "--source-repo",
        default="../ghostty",
        help="Path to a local ghostty checkout",
    )
    parser.add_argument(
        "--destination",
        default="vendor/libghostty-vt",
        help="Destination directory for the extracted libghostty-vt source dist",
    )
    parser.add_argument(
        "--metadata",
        default="vendor/libghostty-vt.vendor.json",
        help="Path to write vendoring metadata JSON",
    )
    args = parser.parse_args()

    repo = Path(args.source_repo).resolve()
    destination = Path(args.destination).resolve()
    metadata_path = Path(args.metadata).resolve()

    metadata = vendor_libghostty_vt(repo, destination)
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    metadata_path.write_text(json.dumps(asdict(metadata), indent=2) + "\n")

    print(f"vendored {metadata.extracted_dir} from {metadata.source_commit} into {destination}")


if __name__ == "__main__":
    main()
