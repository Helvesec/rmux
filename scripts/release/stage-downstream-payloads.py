#!/usr/bin/env python3
"""Materialize exact downstream inputs from one verified canonical candidate."""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

from downstream_channels import CHANNELS, read_object, write_object
from downstream_payload import infer_file_mappings
from downstream_plan import validate_plan
from release_evidence import validate_candidate_manifest

ROOT = Path(__file__).resolve().parents[2]
RELEASE_DATE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}")
STAGED_CHANNELS = tuple(channel for channel in CHANNELS if channel != "rmux_io")


def file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def exact_asset_files(root: Path, expected: set[str]) -> None:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("canonical asset root must be one real directory")
    resolved_root = root.resolve(strict=True)
    actual: set[str] = set()
    for entry in root.rglob("*"):
        if entry.is_symlink():
            raise ValueError("canonical assets cannot contain symlinks")
        if entry.is_dir():
            continue
        if not entry.is_file() or not entry.resolve(strict=True).is_relative_to(
            resolved_root
        ):
            raise ValueError("canonical asset escaped its download root")
        actual.add(entry.relative_to(root).as_posix())
    if actual != expected:
        raise ValueError("canonical asset file set differs from the manifest")


def canonical_files(manifest: dict[str, Any], downloads: Path) -> dict[str, list[Path]]:
    indexed: dict[str, list[Path]] = {}
    for platform in validate_candidate_manifest(manifest):
        source_root = downloads / platform["assets"]["name"] / "assets"
        files = platform.get("files")
        if not isinstance(files, list):
            raise ValueError("candidate platform files are missing")
        expected = {item.get("path") for item in files if isinstance(item, dict)}
        if len(expected) != len(files) or any(
            not isinstance(path, str) for path in expected
        ):
            raise ValueError("candidate platform paths are invalid")
        exact_asset_files(source_root, expected)
        for item in files:
            path = source_root / item["path"]
            if path.stat().st_size != item.get("size") or file_hash(path) != item.get(
                "sha256"
            ):
                raise ValueError("candidate payload bytes differ from the manifest")
            role = item.get("role")
            if not isinstance(role, str):
                raise ValueError("candidate payload role is invalid")
            indexed.setdefault(role, []).append(path)
    for paths in indexed.values():
        paths.sort(key=lambda path: path.name)
    return indexed


def output_directory(root: Path, channel: str) -> Path:
    path = root / channel
    path.mkdir()
    return path


def copy_roles(
    indexed: dict[str, list[Path]], roles: tuple[str, ...], destination: Path
) -> None:
    copied: set[str] = set()
    for role in roles:
        paths = indexed.get(role, [])
        if not paths:
            raise ValueError(f"candidate payload role is missing: {role}")
        for source in paths:
            if source.name in copied:
                raise ValueError("downstream payload filename is duplicated")
            shutil.copyfile(source, destination / source.name)
            copied.add(source.name)


def run_generator(arguments: list[str]) -> None:
    completed = subprocess.run(
        arguments,
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        message = completed.stderr.strip() or completed.stdout.strip()
        raise ValueError(f"downstream metadata generator failed: {message}")


def generated_metadata(
    root: Path,
    *,
    version: str,
    release_ref: str,
    release_date: str,
    checksums: Path,
) -> None:
    for channel in ("homebrew_core", "homebrew_tap"):
        run_generator(
            [
                str(ROOT / "scripts/generate-homebrew-formula.sh"),
                "--version",
                version,
                "--release-tag",
                release_ref,
                "--checksums",
                str(checksums),
                "--output",
                str(root / channel / "rmux.rb"),
            ]
        )
    run_generator(
        [
            str(ROOT / "scripts/generate-scoop-manifest.sh"),
            "--version",
            version,
            "--release-tag",
            release_ref,
            "--checksums",
            str(checksums),
            "--output",
            str(root / "scoop/rmux.json"),
        ]
    )
    run_generator(
        [
            str(ROOT / "scripts/generate-winget-manifest.sh"),
            "--version",
            version,
            "--release-tag",
            release_ref,
            "--release-date",
            release_date,
            "--checksums",
            str(checksums),
            "--output",
            str(root / "winget/Helvesec.RMUX.yaml"),
        ]
    )


def stage(args: argparse.Namespace) -> None:
    if RELEASE_DATE.fullmatch(args.release_date) is None:
        raise ValueError("release date must use YYYY-MM-DD")
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise ValueError("downstream payload output must start absent")
    downloads = args.downloads_dir.resolve(strict=True)
    release_assets = args.release_assets_dir.resolve(strict=True)
    if args.downloads_dir.is_symlink() or args.release_assets_dir.is_symlink():
        raise ValueError("downstream payload inputs cannot be symlinks")
    manifest = read_object(args.manifest.resolve(strict=True), "candidate manifest")
    plan = read_object(args.plan.resolve(strict=True), "downstream plan")
    validate_plan(plan)
    if (
        manifest.get("source_git_sha") != plan["source_git_sha"]
        or manifest.get("planned_release_ref") != plan["release"]["ref"]
        or manifest.get("release_kind") != plan["release"]["kind"]
    ):
        raise ValueError("candidate and downstream plan identities differ")
    release_ref = plan["release"]["ref"]
    version = release_ref.removeprefix("v")
    checksums = release_assets / "SHA256SUMS"
    if checksums.is_symlink() or not checksums.is_file():
        raise ValueError("exact release checksums are missing")
    indexed = canonical_files(manifest, downloads)
    args.output_dir.mkdir()
    for channel in STAGED_CHANNELS:
        output_directory(args.output_dir, channel)
    copy_roles(indexed, ("debian", "rpm"), args.output_dir / "apt_rpm")
    copy_roles(indexed, ("chocolatey-package",), args.output_dir / "chocolatey")
    copy_roles(indexed, ("crate-package-set",), args.output_dir / "crates_io")
    copy_roles(
        indexed, ("snap-amd64", "snap-arm64"), args.output_dir / "snap_candidate"
    )
    copy_roles(
        indexed, ("wasm-byte-set", "wasm-provenance"), args.output_dir / "web_share"
    )
    generated_metadata(
        args.output_dir,
        version=version,
        release_ref=release_ref,
        release_date=args.release_date,
        checksums=checksums,
    )
    snap_entry = next(
        entry for entry in plan["channels"] if entry["name"] == "snap_stable"
    )
    write_object(args.output_dir / "snap_stable/snap-stable-policy.json", snap_entry)
    for channel in STAGED_CHANNELS:
        infer_file_mappings(args.output_dir / channel, channel)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--downloads-dir", type=Path, required=True)
    parser.add_argument("--release-assets-dir", type=Path, required=True)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--release-date", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    try:
        stage(parse_args())
    except (OSError, StopIteration, ValueError) as error:
        print(f"stage-downstream-payloads: {error}", file=sys.stderr)
        raise SystemExit(1) from error
