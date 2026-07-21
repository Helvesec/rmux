#!/usr/bin/env python3
"""Create or verify the deterministic browser WASM release payload."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import tarfile
from io import BytesIO
from pathlib import Path
from typing import Any

SOURCE_SHA = re.compile(r"[0-9a-f]{40}")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
PACKAGE_FILES = (
    "README.md",
    "package.json",
    "rmux_web_crypto_wasm.d.ts",
    "rmux_web_crypto_wasm.js",
    "rmux_web_crypto_wasm_bg.wasm",
    "rmux_web_crypto_wasm_bg.wasm.d.ts",
)


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_identity(source_sha: str, version: str) -> None:
    if SOURCE_SHA.fullmatch(source_sha) is None:
        raise ValueError("source SHA must be lowercase hexadecimal")
    if VERSION.fullmatch(version) is None:
        raise ValueError("release version is not canonical")


def package_files(directory: Path) -> dict[str, Path]:
    root = directory.resolve(strict=True)
    if directory.is_symlink() or not root.is_dir():
        raise ValueError("WASM package must be one real directory")
    result = {name: root / name for name in PACKAGE_FILES}
    for name, path in result.items():
        if path.is_symlink() or not path.is_file() or path.stat().st_size <= 0:
            raise ValueError(f"WASM package file is missing or invalid: {name}")
    return result


def provenance(source_sha: str, version: str, files: dict[str, Path]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "artifact": "rmux-web-crypto WASM production bundle",
        "version": version,
        "source": {
            "repository": "https://github.com/Helvesec/rmux",
            "crate": "crates/rmux-web-crypto",
            "feature_set": ["wasm"],
            "source_commit": source_sha,
        },
        "toolchain": {
            "rustc": "1.94.1",
            "wasm_bindgen": "0.2.123",
            "wasm_pack": "0.13.1",
            "wasm_opt": False,
            "target": "web",
        },
        "build_command": (
            "RUSTUP_TOOLCHAIN=1.94.1 "
            "RUSTFLAGS='--remap-path-prefix=<worktree>=/rmux-source "
            "--remap-path-prefix=<cargo-home>=/cargo "
            "--remap-path-prefix=<rustup-home>=/rustup' "
            "scripts/build-web-crypto-wasm.sh wasm"
        ),
        "artifacts": {
            name: f"sha256:{file_hash(path)}" for name, path in sorted(files.items())
        },
    }


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


def write_tar(path: Path, files: dict[str, Path]) -> None:
    with tarfile.open(path, "w", format=tarfile.PAX_FORMAT) as archive:
        for name, source in sorted(files.items()):
            data = source.read_bytes()
            archive.addfile(tar_info(name, len(data)), BytesIO(data))


def verify_tar(path: Path, files: dict[str, Path]) -> None:
    if path.is_symlink() or not path.is_file() or path.stat().st_size <= 0:
        raise ValueError("WASM bundle must be one non-empty regular file")
    with tarfile.open(path, "r:") as archive:
        members = archive.getmembers()
        if [member.name for member in members] != sorted(files):
            raise ValueError("WASM bundle member set or ordering changed")
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
                raise ValueError("WASM bundle metadata is not deterministic")
            extracted = archive.extractfile(member)
            if extracted is None or extracted.read() != files[member.name].read_bytes():
                raise ValueError(f"WASM bundle bytes differ: {member.name}")


def read_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("WASM provenance is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError("WASM provenance must be an object")
    return value


def paths(args: argparse.Namespace) -> tuple[Path, Path]:
    return (
        args.output_dir / f"rmux-web-crypto-wasm-{args.version}.tar",
        args.output_dir / f"rmux-web-crypto-wasm-{args.version}.provenance.json",
    )


def create(args: argparse.Namespace) -> None:
    validate_identity(args.source_sha, args.version)
    files = package_files(args.package_dir)
    archive, record = paths(args)
    if not args.output_dir.is_dir() or any(
        path.exists() or path.is_symlink() for path in (archive, record)
    ):
        raise ValueError("WASM output directory is missing or outputs already exist")
    write_tar(archive, files)
    record.write_text(
        json.dumps(
            provenance(args.source_sha, args.version, files), indent=2, sort_keys=True
        )
        + "\n",
        encoding="utf-8",
    )
    verify(args)
    print(f"{file_hash(archive)}  {archive.name}")
    print(f"{file_hash(record)}  {record.name}")


def verify(args: argparse.Namespace) -> None:
    validate_identity(args.source_sha, args.version)
    files = package_files(args.package_dir)
    archive, record = paths(args)
    verify_tar(archive, files)
    if read_object(record) != provenance(args.source_sha, args.version, files):
        raise ValueError("WASM provenance differs from the exact package bytes")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("create", "verify"))
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--package-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    arguments = parse_args()
    if arguments.command == "create":
        create(arguments)
    else:
        verify(arguments)
