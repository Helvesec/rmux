"""Exact file mappings for RMUX-owned downstream repositories."""

from __future__ import annotations

import json
import re
import tarfile
from pathlib import Path

VERSION = r"[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?"
WASM_ARCHIVE = re.compile(rf"rmux-web-crypto-wasm-{VERSION}\.tar")
WASM_PROVENANCE = re.compile(rf"rmux-web-crypto-wasm-{VERSION}\.provenance\.json")
PACKAGE_FILES = (
    "README.md",
    "package.json",
    "rmux_web_crypto_wasm.d.ts",
    "rmux_web_crypto_wasm.js",
    "rmux_web_crypto_wasm_bg.wasm",
    "rmux_web_crypto_wasm_bg.wasm.d.ts",
)


def exact_files(root: Path) -> list[Path]:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("owned repository payload must be one real directory")
    resolved = root.resolve(strict=True)
    files: list[Path] = []
    for entry in root.rglob("*"):
        if entry.is_symlink():
            raise ValueError("owned repository payload cannot contain symlinks")
        if entry.is_dir():
            continue
        if not entry.is_file() or not entry.resolve(strict=True).is_relative_to(
            resolved
        ):
            raise ValueError("owned repository payload escaped its root")
        files.append(entry)
    return sorted(files, key=lambda path: path.relative_to(root).as_posix())


def single_file(root: Path, name: str, target: str) -> dict[str, bytes]:
    files = exact_files(root)
    if [path.relative_to(root).as_posix() for path in files] != [name]:
        raise ValueError("owned repository payload file set differs")
    data = files[0].read_bytes()
    if not data:
        raise ValueError("owned repository payload file is empty")
    return {target: data}


def wasm_files(root: Path, *, source_sha: str, version: str) -> dict[str, bytes]:
    files = exact_files(root)
    if len(files) != 2:
        raise ValueError("web share payload must contain one archive and provenance")
    archive = next((path for path in files if WASM_ARCHIVE.fullmatch(path.name)), None)
    provenance_path = next(
        (path for path in files if WASM_PROVENANCE.fullmatch(path.name)), None
    )
    if archive is None or provenance_path is None:
        raise ValueError("web share payload filenames are not canonical")
    try:
        provenance = json.loads(provenance_path.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("web share provenance is not valid UTF-8 JSON") from error
    source = provenance.get("source") if isinstance(provenance, dict) else None
    if (
        not isinstance(provenance, dict)
        or provenance.get("version") != version
        or not isinstance(source, dict)
        or source.get("source_commit") != source_sha
    ):
        raise ValueError("web share provenance identity differs")
    updates: dict[str, bytes] = {}
    with tarfile.open(archive, "r:") as bundle:
        members = bundle.getmembers()
        if [member.name for member in members] != sorted(PACKAGE_FILES):
            raise ValueError("web share archive member set differs")
        for member in members:
            if not member.isfile() or Path(member.name).name != member.name:
                raise ValueError("web share archive contains an unsafe member")
            extracted = bundle.extractfile(member)
            if extracted is None:
                raise ValueError("web share archive member cannot be read")
            data = extracted.read()
            if not data:
                raise ValueError("web share archive member is empty")
            updates[f"src/scripts/share/wasm/{member.name}"] = data
    updates["src/scripts/share/wasm/PROVENANCE.json"] = provenance_path.read_bytes()
    return updates


def repository_updates(
    channel: str, root: Path, *, source_sha: str, version: str
) -> dict[str, bytes]:
    if channel == "homebrew_tap":
        return single_file(root, "rmux.rb", "Formula/rmux.rb")
    if channel == "scoop":
        return single_file(root, "rmux.json", "bucket/rmux.json")
    if channel == "web_share":
        return wasm_files(root, source_sha=source_sha, version=version)
    raise ValueError("channel is not an RMUX-owned repository writer")
