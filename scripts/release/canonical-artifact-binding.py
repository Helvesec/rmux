#!/usr/bin/env python3
"""Create and verify the post-upload binding for a canonical asset artifact."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path
from typing import Any

SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
PLATFORMS = {
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"binding is not valid UTF-8 JSON: {path}") from error
    if not isinstance(value, dict):
        raise ValueError("binding must be a JSON object")
    return value


def validate_inputs(args: argparse.Namespace) -> None:
    if SHA.fullmatch(args.source_sha) is None:
        raise ValueError("source SHA is not canonical")
    if args.candidate_run_id <= 0 or args.assets_artifact_id <= 0:
        raise ValueError("run and artifact IDs must be positive")
    if args.platform_key not in PLATFORMS:
        raise ValueError("platform key is not canonical")
    expected_name = f"rmux-canonical-{args.platform_key}-{args.source_sha}"
    if args.assets_artifact_name != expected_name:
        raise ValueError("asset artifact name does not match platform and source")
    if DIGEST.fullmatch(args.assets_artifact_digest) is None:
        raise ValueError("asset artifact digest is not canonical")
    if re.fullmatch(r"[0-9a-f]{64}", args.build_record_sha256) is None:
        raise ValueError("build record digest is not canonical")
    if not args.attestation_id or len(args.attestation_id) > 256:
        raise ValueError("attestation ID is invalid")
    for path, label in (
        (args.build_record, "build record"),
        (args.attestation_bundle, "attestation bundle"),
    ):
        if path.is_symlink():
            raise ValueError(f"{label} must be one non-empty regular file")
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.stat().st_size <= 0:
            raise ValueError(f"{label} must be one non-empty regular file")
    if sha256_file(args.build_record) != args.build_record_sha256:
        raise ValueError("build record digest changed before binding")


def expected_binding(args: argparse.Namespace) -> dict[str, Any]:
    validate_inputs(args)
    return {
        "schema_version": 1,
        "repository_id": 1239918790,
        "source_git_sha": args.source_sha,
        "candidate_run_id": args.candidate_run_id,
        "platform_key": args.platform_key,
        "assets": {
            "artifact_id": args.assets_artifact_id,
            "artifact_name": args.assets_artifact_name,
            "artifact_digest": args.assets_artifact_digest,
        },
        "build_record_sha256": args.build_record_sha256,
        "attestation": {
            "attestation_id": args.attestation_id,
            "bundle_file": "build-provenance.sigstore.json",
            "bundle_sha256": sha256_file(args.attestation_bundle),
        },
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("create", "verify"))
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--candidate-run-id", type=int, required=True)
    parser.add_argument("--platform-key", required=True)
    parser.add_argument("--assets-artifact-id", type=int, required=True)
    parser.add_argument("--assets-artifact-name", required=True)
    parser.add_argument("--assets-artifact-digest", required=True)
    parser.add_argument("--build-record", type=Path, required=True)
    parser.add_argument("--build-record-sha256", required=True)
    parser.add_argument("--attestation-id", required=True)
    parser.add_argument("--attestation-bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--binding", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    expected = expected_binding(args)
    if args.command == "create":
        if args.output is None or args.binding is not None:
            raise ValueError("create requires --output and forbids --binding")
        args.output.write_text(
            json.dumps(expected, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(sha256_file(args.output))
    else:
        if args.binding is None or args.output is not None:
            raise ValueError("verify requires --binding and forbids --output")
        if read_object(args.binding) != expected:
            raise ValueError("canonical artifact binding changed")
        print(sha256_file(args.binding))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
