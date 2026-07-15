#!/usr/bin/env python3
"""Copy authenticated packages from the published APT/RPM repositories."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile


class HistoryError(RuntimeError):
    pass


@dataclass(frozen=True, order=True)
class StableVersion:
    major: int
    minor: int
    patch: int

    @classmethod
    def parse(cls, value: str) -> StableVersion | None:
        match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value)
        if match is None:
            return None
        return cls(*(int(part) for part in match.groups()))

    def __str__(self) -> str:
        return f"{self.major}.{self.minor}.{self.patch}"


@dataclass(frozen=True)
class AuthenticatedPackage:
    version: StableVersion
    architecture: str
    path: Path


def run_checked(command: list[str], *, capture_stdout: bool = False) -> str:
    environment = os.environ.copy()
    environment.update({"LANG": "C", "LC_ALL": "C"})
    try:
        result = subprocess.run(
            command,
            check=True,
            env=environment,
            stdout=subprocess.PIPE if capture_stdout else subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = ""
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            detail = f": {error.stderr.strip()}"
        raise HistoryError(f"command failed ({command[0]}){detail}") from error
    return result.stdout if capture_stdout else ""


def gpg_primary_fingerprint(key: str, role: str) -> str:
    output = run_checked(
        ["gpg", "--batch", "--with-colons", "--fingerprint", key],
        capture_stdout=True,
    )
    fingerprints: list[str] = []
    expect_primary_fingerprint = False
    for line in output.splitlines():
        fields = line.split(":")
        record_type = fields[0]
        if record_type == "pub":
            expect_primary_fingerprint = True
            continue
        if record_type == "sub":
            expect_primary_fingerprint = False
            continue
        if record_type != "fpr" or not expect_primary_fingerprint:
            continue
        if len(fields) <= 9 or not fields[9]:
            raise HistoryError(f"{role} has a malformed primary fingerprint")
        fingerprint = fields[9].upper()
        if len(fingerprint) not in (40, 64) or any(
            character not in "0123456789ABCDEF" for character in fingerprint
        ):
            raise HistoryError(f"{role} has a malformed primary fingerprint")
        fingerprints.append(fingerprint)
        expect_primary_fingerprint = False

    unique = sorted(set(fingerprints))
    if len(unique) != 1:
        raise HistoryError(
            f"{role} selector must resolve to exactly one primary key; found {len(unique)}"
        )
    return unique[0]


def verify_apt_release(release: Path, signature: Path, key: str) -> None:
    expected = gpg_primary_fingerprint(key, "APT signing key")
    output = run_checked(
        [
            "gpg",
            "--batch",
            "--status-fd",
            "1",
            "--verify",
            str(signature),
            str(release),
        ],
        capture_stdout=True,
    )
    valid_fingerprints: set[str] = set()
    for line in output.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[0] == "[GNUPG:]" and fields[1] == "VALIDSIG":
            for field in fields[2:]:
                normalized = field.upper()
                if len(normalized) in (40, 64) and all(
                    character in "0123456789ABCDEF" for character in normalized
                ):
                    valid_fingerprints.add(normalized)
    if expected not in valid_fingerprints:
        raise HistoryError("APT Release was not signed by the configured key")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def release_sha256_entries(release: Path) -> dict[str, tuple[str, int]]:
    entries: dict[str, tuple[str, int]] = {}
    in_sha256 = False
    for line in release.read_text(encoding="utf-8").splitlines():
        if line == "SHA256:":
            in_sha256 = True
            continue
        if in_sha256 and line and not line.startswith(" "):
            in_sha256 = False
        if not in_sha256 or not line.strip():
            continue
        fields = line.split()
        if len(fields) != 3:
            raise HistoryError("malformed SHA256 entry in APT Release")
        digest, raw_size, relative = fields
        try:
            size = int(raw_size)
        except ValueError as error:
            raise HistoryError("invalid size in APT Release") from error
        if relative in entries:
            raise HistoryError(f"duplicate APT Release entry: {relative}")
        entries[relative] = (digest.lower(), size)
    return entries


def safe_repository_file(root: Path, relative: str) -> Path:
    posix_path = PurePosixPath(relative)
    if posix_path.is_absolute() or ".." in posix_path.parts or not posix_path.parts:
        raise HistoryError(f"unsafe repository path: {relative}")
    candidate = root.joinpath(*posix_path.parts).resolve()
    try:
        candidate.relative_to(root.resolve())
    except ValueError as error:
        raise HistoryError(f"repository path escapes its root: {relative}") from error
    return candidate


def verify_index(index: Path, expected_hash: str, expected_size: int) -> None:
    if not index.is_file():
        raise HistoryError(f"APT index is missing: {index}")
    if index.stat().st_size != expected_size or sha256(index) != expected_hash:
        raise HistoryError(f"APT index failed its signed Release checksum: {index}")


def package_fields(paragraph: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line in paragraph.splitlines():
        if not line or line.startswith((" ", "\t")):
            continue
        key, separator, value = line.partition(":")
        if not separator:
            raise HistoryError("malformed APT Packages field")
        if key in fields:
            raise HistoryError(f"duplicate APT Packages field: {key}")
        fields[key] = value.strip()
    return fields


def copy_without_replacing(source: Path, staging: Path) -> bool:
    destination = staging / source.name
    if destination.exists():
        if (
            not destination.is_file()
            or destination.stat().st_size != source.stat().st_size
            or sha256(destination) != sha256(source)
        ):
            raise HistoryError(
                f"retained package collides with different release asset: {source.name}"
            )
        return True
    shutil.copy2(source, destination)
    return True


def authenticated_apt_packages(
    repository: Path, suite: str, key: str
) -> list[AuthenticatedPackage]:
    apt_root = repository / "debian"
    suite_root = apt_root / "dists" / suite
    release = suite_root / "Release"
    signature = suite_root / "Release.gpg"
    if not release.is_file() or not signature.is_file():
        raise HistoryError("published APT Release metadata is incomplete")
    verify_apt_release(release, signature, key)

    package_records = 0
    authenticated: list[AuthenticatedPackage] = []
    entries = release_sha256_entries(release)
    indexes = sorted(
        relative
        for relative in entries
        if relative.endswith("/Packages") and "/binary-" in relative
    )
    if not indexes:
        raise HistoryError("signed APT Release contains no Packages indexes")

    seen_sources: set[Path] = set()
    for relative in indexes:
        index = safe_repository_file(suite_root, relative)
        expected_hash, expected_size = entries[relative]
        verify_index(index, expected_hash, expected_size)
        for paragraph in index.read_text(encoding="utf-8").split("\n\n"):
            if not paragraph.strip():
                continue
            fields = package_fields(paragraph)
            if fields.get("Package") != "rmux":
                continue
            package_records += 1
            try:
                raw_version = fields["Version"]
                architecture = fields["Architecture"]
                filename = fields["Filename"]
                expected_package_hash = fields["SHA256"].lower()
                expected_package_size = int(fields["Size"])
            except (KeyError, ValueError) as error:
                raise HistoryError(
                    "RMUX APT entry lacks valid Version/Architecture/Filename/Size/SHA256"
                ) from error
            if not architecture:
                raise HistoryError("RMUX APT entry has an empty architecture")
            version = StableVersion.parse(raw_version)
            if version is None:
                # RC repositories are never published by this workflow. Ignore
                # any legacy prerelease entries rather than treating them as N-1.
                continue
            if not filename.startswith("pool/") or not filename.endswith(".deb"):
                raise HistoryError(f"unexpected RMUX APT package path: {filename}")
            package = safe_repository_file(apt_root, filename)
            if package in seen_sources:
                continue
            seen_sources.add(package)
            if not package.is_file():
                raise HistoryError(f"APT package is missing: {filename}")
            if (
                package.stat().st_size != expected_package_size
                or sha256(package) != expected_package_hash
            ):
                raise HistoryError(f"APT package failed its signed index checksum: {filename}")
            authenticated.append(AuthenticatedPackage(version, architecture, package))
    if package_records == 0:
        raise HistoryError("published APT repository contains no RMUX packages")
    return authenticated


def authenticated_rpm_packages(
    repository: Path, key: str
) -> list[AuthenticatedPackage]:
    rpm_root = repository / "rpm"
    packages = sorted(rpm_root.glob("rmux-*.rpm"))
    if not packages:
        raise HistoryError("published RPM repository contains no RMUX packages")
    rpmkeys = shutil.which("rpmkeys") or shutil.which("rpm")
    rpm = shutil.which("rpm")
    if rpmkeys is None or rpm is None:
        raise HistoryError("rpm and rpmkeys are required to authenticate RPM history")

    authenticated: list[AuthenticatedPackage] = []
    with tempfile.TemporaryDirectory(prefix="rmux-rpm-history-") as temp:
        temp_root = Path(temp)
        rpmdb = temp_root / "rpmdb"
        rpmdb.mkdir()
        public_key = temp_root / "rpm-signing-key.asc"
        fingerprint = gpg_primary_fingerprint(key, "RPM signing key")
        exported = run_checked(
            ["gpg", "--batch", "--armor", "--export", fingerprint],
            capture_stdout=True,
        )
        if not exported.strip():
            raise HistoryError("unable to export the configured RPM signing key")
        public_key.write_text(exported, encoding="utf-8")
        run_checked([rpmkeys, "--dbpath", str(rpmdb), "--import", str(public_key)])

        for package in packages:
            signature_status = run_checked(
                [rpmkeys, "--dbpath", str(rpmdb), "--checksig", str(package)],
                capture_stdout=True,
            )
            status_words = set(re.findall(r"[a-z]+", signature_status.lower()))
            has_signature = "signature" in status_words or "signatures" in status_words
            if not has_signature or "ok" not in status_words or "not" in status_words:
                raise HistoryError(
                    f"RPM package is not authenticated by the configured key: {package.name}"
                )
            identity = run_checked(
                [rpm, "-qp", "--qf", "%{NAME}\n%{VERSION}\n%{ARCH}\n", str(package)],
                capture_stdout=True,
            ).splitlines()
            if len(identity) != 3 or identity[0] != "rmux" or not identity[2]:
                raise HistoryError(f"unexpected RPM package identity: {package.name}")
            version = StableVersion.parse(identity[1])
            if version is None:
                # Keep prerelease packages out of the stable N/N-1 set.
                continue
            authenticated.append(AuthenticatedPackage(version, identity[2], package))
    return authenticated


def latest_predecessor(
    packages: list[AuthenticatedPackage], current: StableVersion, manager: str
) -> StableVersion | None:
    versions = {package.version for package in packages}
    newer = sorted(version for version in versions if version > current)
    if newer:
        raise HistoryError(
            f"refusing to replace newer {manager} release {newer[-1]} with {current}"
        )
    predecessors = [version for version in versions if version < current]
    return max(predecessors, default=None)


def retain_predecessor(
    packages: list[AuthenticatedPackage],
    predecessor: StableVersion | None,
    required_architectures: set[str],
    manager: str,
    staging: Path,
) -> int:
    if predecessor is None:
        return 0
    available_architectures = {
        package.architecture for package in packages if package.version == predecessor
    }
    missing = sorted(required_architectures - available_architectures)
    if missing:
        raise HistoryError(
            f"stable {manager} predecessor {predecessor} lacks architecture(s): "
            + ", ".join(missing)
        )
    retained = 0
    for package in packages:
        if package.version != predecessor:
            continue
        if required_architectures and package.architecture not in required_architectures:
            continue
        if copy_without_replacing(package.path, staging):
            retained += 1
    return retained


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-dir", required=True, type=Path)
    parser.add_argument("--staging-dir", required=True, type=Path)
    parser.add_argument("--apt-signing-key", required=True)
    parser.add_argument("--rpm-signing-key", required=True)
    parser.add_argument(
        "--current-version",
        required=True,
        help="stable MAJOR.MINOR.PATCH being published; RCs are not eligible",
    )
    parser.add_argument("--apt-architecture", action="append", default=[])
    parser.add_argument("--rpm-architecture", action="append", default=[])
    parser.add_argument("--suite", default="stable")
    args = parser.parse_args()

    repository = args.repository_dir.resolve()
    staging = args.staging_dir.resolve()
    if not repository.is_dir():
        raise HistoryError(f"repository directory not found: {repository}")
    if not staging.is_dir():
        raise HistoryError(f"staging directory not found: {staging}")
    current = StableVersion.parse(args.current_version)
    if current is None:
        raise HistoryError("--current-version must be stable MAJOR.MINOR.PATCH (not an RC)")

    apt_packages = authenticated_apt_packages(repository, args.suite, args.apt_signing_key)
    rpm_packages = authenticated_rpm_packages(repository, args.rpm_signing_key)
    apt_predecessor = latest_predecessor(apt_packages, current, "APT")
    rpm_predecessor = latest_predecessor(rpm_packages, current, "RPM")
    if apt_predecessor != rpm_predecessor:
        raise HistoryError(
            "APT and RPM repositories disagree on the latest stable predecessor: "
            f"APT={apt_predecessor}, RPM={rpm_predecessor}"
        )
    apt_count = retain_predecessor(
        apt_packages,
        apt_predecessor,
        set(args.apt_architecture),
        "APT",
        staging,
    )
    rpm_count = retain_predecessor(
        rpm_packages,
        rpm_predecessor,
        set(args.rpm_architecture),
        "RPM",
        staging,
    )
    if apt_predecessor is not None and (apt_count == 0 or rpm_count == 0):
        raise HistoryError("stable predecessor has no retainable APT or RPM packages")
    print(f"current_version={current}")
    print(f"retained_predecessor={apt_predecessor}")
    print(f"retained_apt_packages={apt_count}")
    print(f"retained_rpm_packages={rpm_count}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except HistoryError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
