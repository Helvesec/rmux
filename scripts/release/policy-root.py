#!/usr/bin/env python3
"""Compute the RMUX release-policy root from immutable Git blob bytes."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DOMAIN = b"RMUX-RELEASE-POLICY\x00\x01"


def git(root: Path, *arguments: str) -> bytes:
    completed = subprocess.run(
        ["git", *arguments], cwd=root, check=False, capture_output=True
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise ValueError(f"git {' '.join(arguments)} failed: {detail}")
    return completed.stdout


def encode_field(value: bytes) -> bytes:
    return len(value).to_bytes(8, "big") + value


def validate_path(value: Any) -> str:
    if not isinstance(value, str) or not value or value.startswith("/"):
        raise ValueError(f"policy path must be non-empty and relative: {value!r}")
    path = Path(value)
    if ".." in path.parts or "\x00" in value or "\n" in value or "\r" in value:
        raise ValueError(f"policy path is not canonical: {value!r}")
    if path.as_posix() != value:
        raise ValueError(f"policy path must use canonical POSIX separators: {value!r}")
    return value


def calculate(root: Path, contract_path: Path, source_sha: str) -> dict[str, Any]:
    if len(source_sha) != 40 or any(
        char not in "0123456789abcdef" for char in source_sha
    ):
        raise ValueError(
            "source SHA must be exactly 40 lowercase hexadecimal characters"
        )
    resolved = git(root, "rev-parse", f"{source_sha}^{{commit}}").decode().strip()
    if resolved != source_sha:
        raise ValueError(f"source SHA did not resolve exactly: {resolved}")
    contract = json.loads(contract_path.read_text(encoding="utf-8"))
    raw_paths = contract.get("policy_paths")
    if not isinstance(raw_paths, list):
        raise ValueError("contract policy_paths must be an array")
    paths = [validate_path(value) for value in raw_paths]
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise ValueError("contract policy_paths must be sorted and unique")

    root_hash = hashlib.sha256()
    root_hash.update(DOMAIN)
    records: list[dict[str, Any]] = []
    for path in paths:
        blob_oid = git(root, "rev-parse", f"{source_sha}:{path}").decode().strip()
        object_type = git(root, "cat-file", "-t", blob_oid).decode().strip()
        if object_type != "blob":
            raise ValueError(f"policy path is not a Git blob: {path} ({object_type})")
        content = git(root, "cat-file", "blob", blob_oid)
        content_sha256 = hashlib.sha256(content).hexdigest()
        path_bytes = path.encode("utf-8")
        oid_bytes = blob_oid.encode("ascii")
        hash_bytes = content_sha256.encode("ascii")
        size_bytes = len(content).to_bytes(8, "big")
        root_hash.update(encode_field(path_bytes))
        root_hash.update(encode_field(oid_bytes))
        root_hash.update(encode_field(size_bytes))
        root_hash.update(encode_field(hash_bytes))
        records.append(
            {
                "path": path,
                "size": len(content),
                "blob_oid": blob_oid,
                "sha256": content_sha256,
            }
        )
    return {
        "schema_version": 1,
        "algorithm": "sha256-length-delimited-v1",
        "source_git_sha": source_sha,
        "release_policy_sha256": root_hash.hexdigest(),
        "records": records,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--repository-root", type=Path, default=ROOT)
    parser.add_argument(
        "--contract",
        type=Path,
        default=ROOT / ".github" / "release" / "candidate-contract.json",
    )
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    report = calculate(
        args.repository_root.resolve(), args.contract.resolve(), args.source_sha
    )
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
