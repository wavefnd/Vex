#!/usr/bin/env python3

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import os
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - handled before any command runs
    tomllib = None


ROOT = Path(__file__).resolve().parent
TARGET_DIR = ROOT / "target"
DIST_DIR = ROOT / "dist"
BINARY_NAME = "vex"
PACKAGE_DOCUMENTS = (
    "README.md",
    "CHANGELOG.md",
    "LICENSE",
    "NOTICE",
    "COPYRIGHT",
    "THIRD_PARTY_LICENSES.md",
)
CHECKSUM_FILE = "SHA256SUMS"
MINIMUM_ZIP_EPOCH = 315532800  # 1980-01-01T00:00:00Z
MAXIMUM_ZIP_EPOCH = 4354819198  # 2107-12-31T23:59:58Z
VERSION_PATTERN = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$"
)
ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


class ReleaseError(RuntimeError):
    """An actionable release-tool failure."""


@dataclass(frozen=True)
class Target:
    triple: str
    platform: str
    architecture: str
    archive: str

    @property
    def executable_name(self) -> str:
        return f"{BINARY_NAME}.exe" if self.platform == "Windows" else BINARY_NAME


SUPPORTED_TARGETS = {
    target.triple: target
    for target in (
        Target("x86_64-unknown-linux-gnu", "Linux", "amd64", "tar.gz"),
        Target("aarch64-unknown-linux-gnu", "Linux", "arm64", "tar.gz"),
        Target("riscv64gc-unknown-linux-gnu", "Linux", "riscv64", "tar.gz"),
        Target("x86_64-pc-windows-msvc", "Windows", "x64", "zip"),
        Target("x86_64-apple-darwin", "macOS", "Intel", "tar.gz"),
        Target("aarch64-apple-darwin", "macOS", "Apple Silicon", "tar.gz"),
    )
}


def status(action: str, message: str) -> None:
    print(f"{action:>12} {message}", file=sys.stderr)


def command_text(command: Sequence[os.PathLike[str] | str]) -> str:
    return shlex.join(str(part) for part in command)


def run_command(
    command: Sequence[os.PathLike[str] | str],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    status("Running", command_text(command))
    try:
        return subprocess.run(
            [str(part) for part in command],
            cwd=cwd,
            env=env,
            check=True,
            text=True,
            stdout=subprocess.PIPE if capture else None,
            stderr=subprocess.PIPE if capture else None,
        )
    except FileNotFoundError as error:
        raise ReleaseError(f"required tool `{command[0]}` was not found in PATH") from error
    except subprocess.CalledProcessError as error:
        details = ""
        if capture:
            details = (error.stderr or error.stdout or "").strip()
        suffix = f"\n\nCaused by:\n  {details}" if details else ""
        raise ReleaseError(
            f"command failed with status {error.returncode}: {command_text(command)}{suffix}"
        ) from error


def capture_command(
    command: Sequence[os.PathLike[str] | str], *, cwd: Path = ROOT
) -> str:
    return run_command(command, cwd=cwd, capture=True).stdout.strip()


def load_version(manifest_path: Path = ROOT / "Cargo.toml") -> str:
    if tomllib is None:
        raise ReleaseError("Python 3.11 or newer is required to read Cargo.toml")
    try:
        with manifest_path.open("rb") as manifest_file:
            data = tomllib.load(manifest_file)
        version = data["package"]["version"]
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError(f"could not read package version from `{manifest_path}`: {error}") from error
    if not isinstance(version, str) or not VERSION_PATTERN.fullmatch(version):
        raise ReleaseError(f"Cargo.toml contains unsupported package version `{version}`")
    return version


def detect_host_target() -> str:
    override = os.environ.get("VEX_RELEASE_HOST")
    if override:
        return override
    output = capture_command(["rustc", "-vV"])
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    raise ReleaseError("`rustc -vV` did not report a host target")


def select_targets(names: Sequence[str]) -> list[Target]:
    requested = list(names) if names else [detect_host_target()]
    unknown = sorted({name for name in requested if name not in SUPPORTED_TARGETS})
    if unknown:
        known = "\n".join(f"  {name}" for name in SUPPORTED_TARGETS)
        raise ReleaseError(
            f"unsupported target{'' if len(unknown) == 1 else 's'}: {', '.join(unknown)}"
            f"\n\nKnown targets:\n{known}"
        )
    selected: list[Target] = []
    seen: set[str] = set()
    for name in requested:
        if name not in seen:
            selected.append(SUPPORTED_TARGETS[name])
            seen.add(name)
    return selected


def build_environment(target: Target) -> dict[str, str]:
    environment = os.environ.copy()
    if target.triple == "riscv64gc-unknown-linux-gnu":
        variable = "CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER"
        if variable not in environment:
            linker = shutil.which("riscv64-linux-gnu-gcc")
            if linker is None:
                raise ReleaseError(
                    "RISC-V builds require `riscv64-linux-gnu-gcc` in PATH or "
                    f"an explicit `{variable}`"
                )
            environment[variable] = linker
    return environment


def build_targets(targets: Iterable[Target]) -> None:
    for target in targets:
        status("Building", target.triple)
        run_command(
            [
                "cargo",
                "build",
                "--locked",
                "--release",
                "--target",
                target.triple,
            ],
            env=build_environment(target),
        )


def package_name(version: str, target: Target) -> str:
    return f"{BINARY_NAME}-v{version}-{target.triple}"


def archive_path(version: str, target: Target, dist_dir: Path = DIST_DIR) -> Path:
    extension = ".zip" if target.archive == "zip" else ".tar.gz"
    return dist_dir / f"{package_name(version, target)}{extension}"


def source_date_epoch() -> int:
    configured = os.environ.get("SOURCE_DATE_EPOCH")
    if configured is not None:
        try:
            value = int(configured)
        except ValueError as error:
            raise ReleaseError("SOURCE_DATE_EPOCH must be a non-negative integer") from error
        if value < 0:
            raise ReleaseError("SOURCE_DATE_EPOCH must be a non-negative integer")
        return value

    try:
        value = int(capture_command(["git", "log", "-1", "--format=%ct"]))
    except (ReleaseError, ValueError):
        return MINIMUM_ZIP_EPOCH
    return max(value, 0)


def prepare_stage(version: str, target: Target, *, root: Path = ROOT) -> Path:
    source_binary = root / "target" / target.triple / "release" / target.executable_name
    if not source_binary.is_file():
        raise ReleaseError(
            f"release binary is missing: `{source_binary}`\n"
            f"help: run `python3 x.py build {target.triple}` first"
        )

    dist_dir = root / "dist"
    stage = dist_dir / package_name(version, target)
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)

    staged_binary = stage / target.executable_name
    shutil.copyfile(source_binary, staged_binary)
    staged_binary.chmod(0o755)

    for document in PACKAGE_DOCUMENTS:
        source = root / document
        if not source.is_file():
            raise ReleaseError(f"required package document is missing: `{source}`")
        destination = stage / document
        shutil.copyfile(source, destination)
        destination.chmod(0o644)

    return stage


def normalized_tar_info(info: tarfile.TarInfo, epoch: int) -> tarfile.TarInfo:
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = epoch
    info.mode = 0o755 if info.isdir() or info.name.endswith(("/vex", "/vex.exe")) else 0o644
    return info


def create_tar_archive(stage: Path, archive: Path, epoch: int) -> None:
    with archive.open("wb") as raw_archive:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw_archive, mtime=epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                entries = [stage, *sorted(stage.iterdir(), key=lambda path: path.name)]
                for entry in entries:
                    arcname = stage.name if entry == stage else f"{stage.name}/{entry.name}"
                    info = normalized_tar_info(tar.gettarinfo(str(entry), arcname), epoch)
                    if entry.is_dir():
                        tar.addfile(info)
                    elif entry.is_file():
                        with entry.open("rb") as source:
                            tar.addfile(info, source)
                    else:
                        raise ReleaseError(f"unsupported staged entry: `{entry}`")


def zip_timestamp(epoch: int) -> tuple[int, int, int, int, int, int]:
    supported_epoch = min(max(epoch, MINIMUM_ZIP_EPOCH), MAXIMUM_ZIP_EPOCH)
    timestamp = dt.datetime.fromtimestamp(supported_epoch, dt.timezone.utc)
    return (
        timestamp.year,
        timestamp.month,
        timestamp.day,
        timestamp.hour,
        timestamp.minute,
        timestamp.second - timestamp.second % 2,
    )


def write_zip_entry(
    archive: zipfile.ZipFile, name: str, data: bytes, epoch: int, mode: int
) -> None:
    info = zipfile.ZipInfo(name, date_time=zip_timestamp(epoch))
    info.create_system = 3
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = (mode & 0xFFFF) << 16
    archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def create_zip_archive(stage: Path, archive: Path, epoch: int) -> None:
    with zipfile.ZipFile(archive, mode="w") as zipped:
        write_zip_entry(zipped, f"{stage.name}/", b"", epoch, 0o40755)
        for entry in sorted(stage.iterdir(), key=lambda path: path.name):
            if not entry.is_file():
                raise ReleaseError(f"unsupported staged entry: `{entry}`")
            mode = 0o100755 if entry.name in ("vex", "vex.exe") else 0o100644
            write_zip_entry(zipped, f"{stage.name}/{entry.name}", entry.read_bytes(), epoch, mode)


def create_archive(stage: Path, target: Target, epoch: int) -> Path:
    extension = ".zip" if target.archive == "zip" else ".tar.gz"
    archive = stage.parent / f"{stage.name}{extension}"
    archive.unlink(missing_ok=True)
    if target.archive == "zip":
        create_zip_archive(stage, archive, epoch)
    else:
        create_tar_archive(stage, archive, epoch)
    return archive


def expected_archive_entries(stage_name: str, target: Target) -> set[str]:
    return {
        f"{stage_name}/",
        f"{stage_name}/{target.executable_name}",
        *(f"{stage_name}/{document}" for document in PACKAGE_DOCUMENTS),
    }


def read_binary_from_tar(archive: Path, member_name: str) -> tuple[set[str], bytes]:
    with tarfile.open(archive, "r:gz") as packaged:
        members = packaged.getmembers()
        names = {member.name.rstrip("/") + ("/" if member.isdir() else "") for member in members}
        if any(member.issym() or member.islnk() for member in members):
            raise ReleaseError(f"archive contains an unexpected link: `{archive}`")
        member = packaged.getmember(member_name)
        extracted = packaged.extractfile(member)
        if extracted is None:
            raise ReleaseError(f"could not read packaged binary `{member_name}`")
        return names, extracted.read()


def read_binary_from_zip(archive: Path, member_name: str) -> tuple[set[str], bytes]:
    with zipfile.ZipFile(archive) as packaged:
        names = set(packaged.namelist())
        return names, packaged.read(member_name)


def smoke_prefix(target: Target, binary: Path, host: str) -> list[str] | None:
    if target.triple == host:
        return [str(binary)]
    if target.triple == "riscv64gc-unknown-linux-gnu" and host.endswith("-unknown-linux-gnu"):
        emulator = shutil.which("qemu-riscv64")
        sysroot = Path("/usr/riscv64-linux-gnu")
        if emulator and sysroot.is_dir():
            return [emulator, "-L", str(sysroot), str(binary)]
    if target.platform == "Windows" and host.endswith("-unknown-linux-gnu"):
        emulator = shutil.which("wine")
        if emulator:
            return [emulator, str(binary)]
    return None


def strip_ansi(text: str) -> str:
    return ANSI_ESCAPE.sub("", text)


def smoke_binary(binary_data: bytes, target: Target, version: str, host: str) -> bool:
    with tempfile.TemporaryDirectory(prefix="vex-package-smoke-") as temporary:
        binary = Path(temporary) / target.executable_name
        binary.write_bytes(binary_data)
        binary.chmod(0o755)
        prefix = smoke_prefix(target, binary, host)
        if prefix is None:
            status("Skipping", f"execution smoke for {target.triple} on {host}")
            return False

        version_result = run_command([*prefix, "--version"], capture=True)
        version_output = strip_ansi(version_result.stdout).strip()
        if not re.search(rf"\bvex\s+{re.escape(version)}\b", version_output):
            raise ReleaseError(
                f"packaged binary reported unexpected version `{version_output}`; expected `{version}`"
            )

        help_result = run_command([*prefix, "--help"], capture=True)
        if "Vex - Wave package manager" not in strip_ansi(help_result.stdout):
            raise ReleaseError("packaged binary help output did not contain the Vex heading")
        return True


def verify_archive(archive: Path, stage: Path, target: Target, version: str, host: str) -> None:
    binary_member = f"{stage.name}/{target.executable_name}"
    try:
        if target.archive == "zip":
            names, binary_data = read_binary_from_zip(archive, binary_member)
        else:
            names, binary_data = read_binary_from_tar(archive, binary_member)
    except (KeyError, OSError, tarfile.TarError, zipfile.BadZipFile) as error:
        raise ReleaseError(f"could not verify release archive `{archive}`: {error}") from error

    expected = expected_archive_entries(stage.name, target)
    if names != expected:
        missing = sorted(expected - names)
        extra = sorted(names - expected)
        details = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if extra:
            details.append(f"unexpected: {', '.join(extra)}")
        raise ReleaseError(f"archive contents are invalid ({'; '.join(details)})")
    smoke_binary(binary_data, target, version, host)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as artifact:
        while chunk := artifact.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def write_checksums(archives: Iterable[Path], dist_dir: Path = DIST_DIR) -> Path:
    selected = sorted(archives, key=lambda path: path.name)
    if not selected:
        raise ReleaseError("no release archives were provided for checksumming")
    checksum_path = dist_dir / CHECKSUM_FILE
    content = "".join(f"{sha256(archive)}  {archive.name}\n" for archive in selected)
    temporary = checksum_path.with_suffix(".tmp")
    temporary.write_text(content, encoding="utf-8", newline="\n")
    temporary.replace(checksum_path)
    return checksum_path


def collect_release_archives(
    targets: Iterable[Target], version: str, dist_dir: Path = DIST_DIR
) -> list[Path]:
    expected = [archive_path(version, target, dist_dir) for target in targets]
    missing = [path.name for path in expected if not path.is_file()]
    expected_names = {path.name for path in expected}
    candidates = {
        path.name
        for pattern in (
            f"{BINARY_NAME}-v{version}-*.tar.gz",
            f"{BINARY_NAME}-v{version}-*.zip",
        )
        for path in dist_dir.glob(pattern)
        if path.is_file()
    }
    unexpected = sorted(candidates - expected_names)
    if missing or unexpected:
        details = []
        if missing:
            details.append(f"missing: {', '.join(sorted(missing))}")
        if unexpected:
            details.append(f"unexpected: {', '.join(unexpected)}")
        raise ReleaseError(
            "release archive set is incomplete or inconsistent "
            f"({'; '.join(details)})\n"
            "help: build and package exactly the requested release targets"
        )
    return expected


def package_targets(targets: Iterable[Target], version: str, host: str) -> list[Path]:
    epoch = source_date_epoch()
    archives: list[Path] = []
    for target in targets:
        status("Packaging", target.triple)
        stage = prepare_stage(version, target)
        archive: Path | None = None
        try:
            archive = create_archive(stage, target, epoch)
            verify_archive(archive, stage, target, version, host)
        except Exception:
            if archive is not None:
                archive.unlink(missing_ok=True)
            raise
        finally:
            shutil.rmtree(stage, ignore_errors=True)
        archives.append(archive)
        status("Packaged", str(archive.relative_to(ROOT)))
    checksum_path = write_checksums(archives)
    status("Checksums", str(checksum_path.relative_to(ROOT)))
    return archives


def require_clean_tree() -> None:
    dirty = capture_command(["git", "status", "--porcelain", "--untracked-files=normal"])
    if dirty:
        raise ReleaseError(
            "official releases require a clean Git working tree\n"
            "help: commit, stash, or remove local changes before running `x.py release`"
        )


def require_release_tag(version: str) -> None:
    expected = f"v{version}"
    tags = capture_command(["git", "tag", "--points-at", "HEAD"]).splitlines()
    if expected not in tags:
        raise ReleaseError(
            f"official release must run from tag `{expected}`\n"
            f"help: create and check out the annotated `{expected}` tag"
        )
    tag_type = capture_command(["git", "cat-file", "-t", f"refs/tags/{expected}"])
    if tag_type != "tag":
        raise ReleaseError(
            f"official release tag `{expected}` must be annotated, not lightweight\n"
            f"help: recreate `{expected}` with `git tag -s {expected}` or "
            f"`git tag -a {expected}`"
        )


def verify_release_source(version: str) -> None:
    require_clean_tree()
    require_release_tag(version)


def validate_third_party_licenses(
    notice_path: Path = ROOT / "THIRD_PARTY_LICENSES.md",
    lockfile_path: Path = ROOT / "Cargo.lock",
) -> None:
    if tomllib is None:
        raise ReleaseError("Python 3.11 or newer is required to read Cargo.lock")
    try:
        with lockfile_path.open("rb") as lockfile:
            metadata = tomllib.load(lockfile)
        notice = notice_path.read_text(encoding="utf-8")
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError(f"could not validate third-party license inventory: {error}") from error

    packages = sorted(
        (package["name"], package["version"])
        for package in metadata.get("package", [])
        if str(package.get("source", "")).startswith("registry+")
    )
    missing_notices = [
        f"{name} {version}"
        for name, version in packages
        if f"https://crates.io/crates/{name}/{version}" not in notice
    ]
    if missing_notices:
        raise ReleaseError(
            "third-party license inventory is incomplete (missing from "
            "THIRD_PARTY_LICENSES.md: "
            + ", ".join(missing_notices)
            + ")"
        )
    status("Audited", f"third-party licenses for {len(packages)} locked packages")


def run_check_suite() -> None:
    run_command(["cargo", "fmt", "--check"])
    run_python_tests()
    run_command(["cargo", "test", "--locked"])
    run_command(["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"])
    run_command(["cargo", "build", "--locked"])
    validate_third_party_licenses()


def command_check(_: argparse.Namespace) -> None:
    run_check_suite()


def command_test(_: argparse.Namespace) -> None:
    run_python_tests()
    run_command(["cargo", "test", "--locked"])


def command_build(args: argparse.Namespace) -> None:
    build_targets(select_targets(args.targets))


def command_package(args: argparse.Namespace) -> None:
    targets = select_targets(args.targets)
    package_targets(targets, load_version(), detect_host_target())


def command_checksum(args: argparse.Namespace) -> None:
    targets = select_targets(args.targets)
    checksum_path = write_checksums(
        collect_release_archives(targets, load_version()),
    )
    status("Checksums", str(checksum_path.relative_to(ROOT)))


def command_verify_release(_: argparse.Namespace) -> None:
    version = load_version()
    verify_release_source(version)
    status("Verified", f"clean source at annotated tag v{version}")


def command_release(args: argparse.Namespace) -> None:
    version = load_version()
    verify_release_source(version)
    targets = select_targets(args.targets)
    run_check_suite()
    build_targets(targets)
    package_targets(targets, version, detect_host_target())


def command_clean(_: argparse.Namespace) -> None:
    status("Cleaning", "Cargo build artifacts")
    run_command(["cargo", "clean"])
    if DIST_DIR.exists():
        status("Cleaning", str(DIST_DIR.relative_to(ROOT)))
        shutil.rmtree(DIST_DIR)


def command_list_targets(_: argparse.Namespace) -> None:
    try:
        host = detect_host_target()
    except ReleaseError:
        host = "<unknown>"
    print(f"Host: {host}")
    print("Supported release targets:")
    for target in SUPPORTED_TARGETS.values():
        marker = " (native default)" if target.triple == host else ""
        print(
            f"  {target.triple:<36} {target.platform:<8} "
            f"{target.architecture:<13} {target.archive}{marker}"
        )


def add_target_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "targets",
        nargs="*",
        metavar="TARGET",
        help="release target triple; defaults to the rustc host",
    )


def run_python_tests() -> None:
    run_command(
        [sys.executable, "-m", "unittest", "discover", "-s", "tests/xpy", "-v"]
    )


def create_parser(version: str) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Build and package reproducible Vex release artifacts."
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {version}")
    commands = parser.add_subparsers(dest="command")

    check = commands.add_parser("check", help="run the complete local validation suite")
    check.set_defaults(handler=command_check)

    test = commands.add_parser("test", help="run release-tool and Rust tests")
    test.set_defaults(handler=command_test)

    build = commands.add_parser("build", help="build release binaries")
    add_target_arguments(build)
    build.set_defaults(handler=command_build)

    package = commands.add_parser("package", help="package existing release binaries")
    add_target_arguments(package)
    package.set_defaults(handler=command_package)

    checksum = commands.add_parser(
        "checksum", help="verify a complete archive set and write SHA256SUMS"
    )
    add_target_arguments(checksum)
    checksum.set_defaults(handler=command_checksum)

    verify_release = commands.add_parser(
        "verify-release", help="verify clean source and the annotated version tag"
    )
    verify_release.set_defaults(handler=command_verify_release)

    release = commands.add_parser(
        "release", help="validate, build, and package an official tagged release"
    )
    add_target_arguments(release)
    release.set_defaults(handler=command_release)

    clean = commands.add_parser("clean", help="remove Cargo and release-tool artifacts")
    clean.set_defaults(handler=command_clean)

    list_targets = commands.add_parser("list-targets", help="show supported release targets")
    list_targets.set_defaults(handler=command_list_targets)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    if sys.version_info < (3, 11):
        print("error: x.py requires Python 3.11 or newer", file=sys.stderr)
        return 1
    try:
        version = load_version()
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    parser = create_parser(version)
    args = parser.parse_args(argv)
    if not hasattr(args, "handler"):
        parser.print_help()
        return 0
    try:
        args.handler(args)
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
