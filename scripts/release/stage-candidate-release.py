#!/usr/bin/env python3
"""Stage publishable bytes from one exact canonical candidate download."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

from release_evidence import publishable_assets, validate_candidate_manifest

REPOSITORY_ID = 1239918790
SHA40 = re.compile(r"[0-9a-f]{40}")
SAFE_PATH = re.compile(r"[A-Za-z0-9._+@=-]+(?:/[A-Za-z0-9._+@=-]+)*")


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise ValueError(f"{label} keys changed")


def resolution_artifacts(
    resolution: dict[str, Any], candidate_run_id: int, source_sha: str
) -> dict[tuple[str, str | None], dict[str, Any]]:
    exact_keys(
        resolution,
        {
            "schema_version",
            "repository_id",
            "candidate_run",
            "expected_artifact_count",
            "artifacts",
        },
        "candidate artifact resolution",
    )
    run = resolution.get("candidate_run")
    artifacts = resolution.get("artifacts")
    if (
        resolution["schema_version"] != 1
        or resolution["repository_id"] != REPOSITORY_ID
        or resolution["expected_artifact_count"] != 11
        or not isinstance(run, dict)
        or run.get("id") != candidate_run_id
        or run.get("attempt") != 1
        or run.get("head_sha") != source_sha
        or run.get("status") != "completed"
        or run.get("conclusion") != "success"
        or not isinstance(artifacts, list)
        or len(artifacts) != 11
    ):
        raise ValueError("candidate artifact resolution identity changed")
    indexed: dict[tuple[str, str | None], dict[str, Any]] = {}
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            raise ValueError("resolved artifact must be an object")
        exact_keys(
            artifact,
            {
                "role",
                "platform_key",
                "artifact_id",
                "name",
                "archive_digest",
                "size_in_bytes",
            },
            "resolved artifact",
        )
        key = (artifact["role"], artifact["platform_key"])
        if key in indexed:
            raise ValueError("resolved artifact role and platform are duplicated")
        indexed[key] = artifact
    if len(indexed) != 11:
        raise ValueError("resolved artifact identities are not unique")
    return indexed


def exact_asset_files(root: Path, expected: set[str]) -> None:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("canonical asset root must be one real directory")
    actual: set[str] = set()
    resolved_root = root.resolve(strict=True)
    for entry in root.rglob("*"):
        if entry.is_symlink():
            raise ValueError("canonical assets cannot contain symlinks")
        if entry.is_dir():
            continue
        if not entry.is_file() or not entry.resolve(strict=True).is_relative_to(resolved_root):
            raise ValueError("canonical asset escaped its download root")
        actual.add(entry.relative_to(root).as_posix())
    if actual != expected:
        raise ValueError("canonical asset file set differs from the manifest")


def stage(args: argparse.Namespace) -> str:
    if args.candidate_run_id <= 0 or SHA40.fullmatch(args.source_sha) is None:
        raise ValueError("candidate identity is not canonical")
    manifest = read_object(args.manifest, "candidate manifest")
    platforms = validate_candidate_manifest(manifest)
    if (
        manifest["repository_id"] != REPOSITORY_ID
        or manifest["candidate_run_id"] != args.candidate_run_id
        or manifest["source_git_sha"] != args.source_sha
    ):
        raise ValueError("candidate manifest identity differs")
    resolution = resolution_artifacts(
        read_object(args.resolution, "candidate artifact resolution"),
        args.candidate_run_id,
        args.source_sha,
    )
    downloads = args.downloads_dir.resolve(strict=True)
    if downloads.is_symlink() or not downloads.is_dir():
        raise ValueError("candidate downloads must be one real directory")
    if args.output.exists() or args.output.is_symlink():
        raise ValueError("refusing to overwrite staged release assets")
    if not args.output.parent.is_dir():
        raise ValueError("staged release parent directory does not exist")
    args.output.mkdir()

    records: list[tuple[str, str]] = []
    for platform in platforms:
        key = platform["platform_key"]
        for role, field in (
            ("canonical-assets", "assets"),
            ("canonical-provenance", "provenance"),
        ):
            if platform.get(field) != resolution.get((role, key)):
                raise ValueError(f"{key} manifest does not bind the live {role} archive")
        source_root = downloads / platform["assets"]["name"] / "assets"
        files = platform.get("files")
        if not isinstance(files, list):
            raise ValueError(f"{key} manifest files are missing")
        expected_paths = {
            item.get("path") for item in files if isinstance(item, dict)
        }
        if (
            len(expected_paths) != len(files)
            or any(
                not isinstance(path, str) or SAFE_PATH.fullmatch(path) is None
                for path in expected_paths
            )
        ):
            raise ValueError(f"{key} manifest file paths are invalid")
        exact_asset_files(source_root, expected_paths)
        for item in files:
            source = source_root / item["path"]
            raw = source.read_bytes()
            if len(raw) != item.get("size") or hashlib.sha256(raw).hexdigest() != item.get(
                "sha256"
            ):
                raise ValueError(f"{key} candidate asset bytes differ")
            if item.get("role") == "checksums":
                continue
            name = Path(item["path"]).name
            if name != item["path"] or (args.output / name).exists():
                raise ValueError("candidate public asset name is unsafe or duplicated")
            (args.output / name).write_bytes(raw)
            records.append((name, item["sha256"]))

    records.sort()
    sums = args.output / "SHA256SUMS"
    sums.write_text(
        "".join(f"{digest}  {name}\n" for name, digest in records),
        encoding="ascii",
    )
    authorized = publishable_assets(manifest, sums)
    expected_names = {item["name"] for item in authorized}
    if {entry.name for entry in args.output.iterdir()} != expected_names:
        raise ValueError("staged asset set differs from the authorized manifest")
    return file_hash(sums)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--resolution", type=Path, required=True)
    parser.add_argument("--downloads-dir", type=Path, required=True)
    parser.add_argument("--candidate-run-id", type=int, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    try:
        print(stage(parse_args()))
    except ValueError as error:
        print(f"candidate-staging: {error}", file=sys.stderr)
        raise SystemExit(1) from error
