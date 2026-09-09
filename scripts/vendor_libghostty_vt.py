from __future__ import annotations

import argparse
import json
import posixpath
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
    if ".." in parts:
        raise ValueError(
            f"refusing to extract archive member {name!r}: parent components are not source paths"
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

    Explicit member and namespace validation is the primary guard, including on
    interpreters without extraction filters. Where available, the data filter
    runs as a second check; neither its availability nor its defaults are trusted.
    """
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        by_path = {}
        links = set()
        for member in members:
            check_archive_member(member)
            parts = _path_parts(member.name)
            if parts in by_path:
                raise ValueError(f"refusing to extract duplicate archive member {member.name!r}")
            by_path[parts] = member
            if member.issym() or member.islnk():
                links.add(parts)
        # Check the whole namespace before extracting, independent of member order.
        # File aliases are useful; directory aliases and link chains are not needed
        # by the source dist and can redirect later members or copytree traversal.
        for parts, member in by_path.items():
            if any(parts[:depth] in links for depth in range(1, len(parts))):
                raise ValueError(f"refusing to extract member through a link: {member.name!r}")
            if parts in links:
                base = "/".join(parts[:-1]) if member.issym() else ""
                target = _path_parts(posixpath.normpath(posixpath.join(base, member.linkname)))
                target_member = by_path.get(target)
                if (not target or target[0] != parts[0]
                        or target_member is None or not target_member.isreg()):
                    raise ValueError(f"refusing to extract link outside a regular source member: {member.name!r}")
        if hasattr(tarfile, "data_filter"):
            tar.extractall(destination, filter="data")
        else:
            tar.extractall(destination)


def vendor_libghostty_vt(source_repo: Path, destination: Path) -> VendorMetadata:
    archive = ensure_dist_archive(source_repo)
    root = parse_archive_root(archive)
    metadata = VendorMetadata(
        source_commit=git_head(source_repo), dist_archive=archive.name, extracted_dir=root,
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".vendor-stage-", dir=destination.parent))
    previous = staging / "previous"
    try:
        unpacked = staging / "unpacked"
        unpacked.mkdir()
        extract_archive(archive, unpacked)
        extracted = unpacked / root
        if not extracted.is_dir() or extracted.is_symlink():
            raise FileNotFoundError(f"expected extracted root {extracted}")

        ready = staging / "new"
        shutil.copytree(extracted, ready)
        if destination.exists():
            destination.rename(previous)
        try:
            ready.rename(destination)
        except OSError:
            if previous.exists():
                try:
                    previous.rename(destination)
                except OSError as rollback_error:
                    raise OSError(f"vendor install and rollback failed; backup retained at {previous}") from rollback_error
            raise
        if previous.exists():
            shutil.rmtree(previous)
        return metadata
    finally:
        # If rollback itself failed, never let temporary-directory cleanup delete
        # the only remaining copy of the old vendor tree.
        if not previous.exists():
            shutil.rmtree(staging)


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
