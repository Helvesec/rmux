#!/usr/bin/env python3
"""Build or verify the exact crates.io package set for one candidate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tarfile
from io import BytesIO
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
SOURCE_SHA = re.compile(r"[0-9a-f]{40}")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def cargo_metadata() -> dict[str, Any]:
    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise ValueError(f"cargo metadata failed: {completed.stderr.strip()}")
    value = json.loads(completed.stdout)
    if not isinstance(value, dict):
        raise ValueError("cargo metadata did not return an object")
    return value


def publishable_packages(
    metadata: dict[str, Any], version: str
) -> list[dict[str, Any]]:
    members = set(metadata.get("workspace_members", []))
    values = []
    for package in metadata.get("packages", []):
        if not isinstance(package, dict) or package.get("id") not in members:
            continue
        publish = package.get("publish")
        if publish == [] or (isinstance(publish, list) and "crates-io" not in publish):
            continue
        if package.get("version") != version:
            raise ValueError(
                f"publishable crate version differs: {package.get('name')}"
            )
        values.append(package)
    names = [package.get("name") for package in values]
    if any(not isinstance(name, str) for name in names) or len(names) != len(
        set(names)
    ):
        raise ValueError("publishable crate names are invalid or duplicated")
    if not values:
        raise ValueError("workspace has no publishable crates")
    return values


def dependency_order(
    packages: list[dict[str, Any]],
) -> tuple[list[str], dict[str, list[str]]]:
    names = {package["name"] for package in packages}
    dependencies: dict[str, list[str]] = {}
    for package in packages:
        local = {
            dependency["name"]
            for dependency in package.get("dependencies", [])
            if isinstance(dependency, dict)
            and dependency.get("kind") != "dev"
            and dependency.get("path") is not None
            and dependency.get("name") in names
        }
        dependencies[package["name"]] = sorted(local)
    remaining = {name: set(values) for name, values in dependencies.items()}
    order: list[str] = []
    while remaining:
        ready = sorted(name for name, deps in remaining.items() if not deps)
        if not ready:
            raise ValueError("publishable crate dependency graph contains a cycle")
        order.extend(ready)
        for name in ready:
            del remaining[name]
        for deps in remaining.values():
            deps.difference_update(ready)
    return order, dependencies


def run_cargo_package(names: list[str], target: Path) -> None:
    if target.exists() or target.is_symlink():
        raise ValueError("crate package target must start absent")
    env = {**os.environ, "CARGO_TARGET_DIR": str(target)}
    command = ["cargo", "package"]
    for name in names:
        command.extend(("--package", name))
    command.extend(("--locked", "--no-verify"))
    completed = subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise ValueError(
            "cargo package failed for canonical workspace set: "
            f"{completed.stderr.strip()}"
        )


def tar_info(name: str, size: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name)
    info.size = size
    info.mode = 0o644
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    return info


def build_manifest(
    source_sha: str,
    version: str,
    order: list[str],
    dependencies: dict[str, list[str]],
    package_dir: Path,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    entries: list[dict[str, Any]] = []
    payloads: dict[str, bytes] = {}
    expected_names = {f"{name}-{version}.crate" for name in order}
    actual_names = {path.name for path in package_dir.glob("*.crate")}
    if actual_names != expected_names:
        raise ValueError("cargo package output set differs from publishable crates")
    for name in order:
        filename = f"{name}-{version}.crate"
        path = package_dir / filename
        if path.is_symlink() or not path.is_file() or path.stat().st_size <= 0:
            raise ValueError(f"crate package is missing or invalid: {filename}")
        data = path.read_bytes()
        payloads[f"crates/{filename}"] = data
        entries.append(
            {
                "name": name,
                "version": version,
                "file": filename,
                "size": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "workspace_dependencies": dependencies[name],
            }
        )
    manifest = {
        "schema_version": 1,
        "repository_id": 1239918790,
        "source_git_sha": source_sha,
        "version": version,
        "publish_order": order,
        "package_count": len(entries),
        "packages": entries,
    }
    payloads["crate-package-set.json"] = (
        json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    ).encode()
    return manifest, payloads


def write_tar(path: Path, payloads: dict[str, bytes]) -> None:
    with tarfile.open(path, "w", format=tarfile.PAX_FORMAT) as archive:
        for name, data in sorted(payloads.items()):
            archive.addfile(tar_info(name, len(data)), BytesIO(data))


def verify_tar(path: Path, payloads: dict[str, bytes]) -> None:
    if path.is_symlink() or not path.is_file() or path.stat().st_size <= 0:
        raise ValueError("crate package set must be one non-empty regular file")
    with tarfile.open(path, "r:") as archive:
        members = archive.getmembers()
        if [member.name for member in members] != sorted(payloads):
            raise ValueError("crate package set members differ")
        for member in members:
            if (
                not member.isfile()
                or member.mode != 0o644
                or member.mtime != 0
                or member.uid != 0
                or member.gid != 0
                or member.uname
                or member.gname
            ):
                raise ValueError("crate package set metadata is not deterministic")
            extracted = archive.extractfile(member)
            if extracted is None or extracted.read() != payloads[member.name]:
                raise ValueError(f"crate package bytes differ: {member.name}")


def validate_identity(args: argparse.Namespace) -> None:
    if SOURCE_SHA.fullmatch(args.source_sha) is None:
        raise ValueError("source SHA must be lowercase hexadecimal")
    if VERSION.fullmatch(args.version) is None:
        raise ValueError("release version is not canonical")


def execute(args: argparse.Namespace) -> None:
    validate_identity(args)
    metadata = cargo_metadata()
    packages = publishable_packages(metadata, args.version)
    order, dependencies = dependency_order(packages)
    if args.command == "create":
        run_cargo_package(order, args.target_dir)
    package_dir = args.target_dir / "package"
    manifest, payloads = build_manifest(
        args.source_sha, args.version, order, dependencies, package_dir
    )
    output = args.output_dir / f"rmux-{args.version}-crate-package-set.tar"
    if args.command == "create":
        if not args.output_dir.is_dir() or output.exists() or output.is_symlink():
            raise ValueError("crate set output directory is missing or output exists")
        write_tar(output, payloads)
    verify_tar(output, payloads)
    if json.loads(payloads["crate-package-set.json"]) != manifest:
        raise ValueError("crate package manifest is not canonical")
    print(f"{file_hash(output)}  {output.name}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("create", "verify"))
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    execute(parse_args())
