"""Safe reader for one canonical RMUX crates.io package set."""

from __future__ import annotations

import hashlib
import json
import tarfile
from pathlib import Path
from typing import Any

MAX_PACKAGE_SET_BYTES = 256 * 1024 * 1024
MAX_MEMBER_BYTES = 32 * 1024 * 1024
MAX_MEMBERS = 128


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def unpack(package_set: Path, output: Path) -> dict[str, Any]:
    if package_set.is_symlink() or not package_set.is_file():
        raise ValueError("crate package set must be one regular file")
    if package_set.stat().st_size > MAX_PACKAGE_SET_BYTES:
        raise ValueError("crate package set exceeds the release size limit")
    if output.exists() or output.is_symlink():
        raise ValueError("crate package extraction root must start absent")
    output.mkdir()
    with tarfile.open(package_set, "r:") as archive:
        members = archive.getmembers()
        names = [member.name for member in members]
        if (
            not members
            or len(members) > MAX_MEMBERS
            or names != sorted(names)
            or len(names) != len(set(names))
        ):
            raise ValueError("crate package set members are not sorted and unique")
        for member in members:
            parts = Path(member.name).parts
            if (
                not member.isfile()
                or not parts
                or any(part in {"", ".", ".."} for part in parts)
                or Path(member.name).is_absolute()
                or member.size <= 0
                or member.size > MAX_MEMBER_BYTES
            ):
                raise ValueError("crate package set contains an unsafe member")
            extracted = archive.extractfile(member)
            if extracted is None:
                raise ValueError("crate package set member cannot be read")
            destination = output.joinpath(*parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(extracted.read())
    manifest_path = output / "crate-package-set.json"
    try:
        value = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("crate package manifest is invalid") from error
    if not isinstance(value, dict):
        raise ValueError("crate package manifest must be an object")
    return value


def validate(
    manifest: dict[str, Any], root: Path, *, source_sha: str, version: str
) -> list[dict[str, Any]]:
    packages = manifest.get("packages")
    order = manifest.get("publish_order")
    if (
        manifest.get("schema_version") != 1
        or manifest.get("repository_id") != 1239918790
        or manifest.get("source_git_sha") != source_sha
        or manifest.get("version") != version
        or not isinstance(packages, list)
        or not isinstance(order, list)
        or manifest.get("package_count") != len(packages)
    ):
        raise ValueError("crate package manifest identity changed")
    by_name: dict[str, dict[str, Any]] = {}
    expected_files = {"crate-package-set.json"}
    for package in packages:
        if not isinstance(package, dict):
            raise ValueError("crate package entry must be an object")
        expected_keys = {
            "name",
            "version",
            "file",
            "size",
            "sha256",
            "workspace_dependencies",
        }
        if set(package) != expected_keys:
            raise ValueError("crate package entry fields changed")
        name = package["name"]
        filename = package["file"]
        dependencies = package["workspace_dependencies"]
        if (
            not isinstance(name, str)
            or not name
            or package["version"] != version
            or filename != f"{name}-{version}.crate"
            or not isinstance(dependencies, list)
            or dependencies != sorted(set(dependencies))
            or name in by_name
        ):
            raise ValueError("crate package entry is not canonical")
        path = root / "crates" / filename
        if (
            path.is_symlink()
            or not path.is_file()
            or path.stat().st_size != package["size"]
            or file_hash(path) != package["sha256"]
        ):
            raise ValueError("crate package bytes differ from their manifest")
        expected_files.add(f"crates/{filename}")
        by_name[name] = package
    if (
        not all(isinstance(name, str) for name in order)
        or len(order) != len(set(order))
        or set(order) != set(by_name)
    ):
        raise ValueError("crate publish order differs from package entries")
    actual_files = {
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    }
    if (
        any(path.is_symlink() for path in root.rglob("*"))
        or actual_files != expected_files
    ):
        raise ValueError("crate package extraction file set differs")
    published: set[str] = set()
    ordered: list[dict[str, Any]] = []
    for name in order:
        package = by_name.get(name)
        if package is None or not set(package["workspace_dependencies"]) <= published:
            raise ValueError("crate publish order violates workspace dependencies")
        ordered.append(package)
        published.add(name)
    if len(ordered) != len(packages):
        raise ValueError("crate publish order is incomplete")
    return ordered
