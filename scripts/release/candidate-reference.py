#!/usr/bin/env python3
"""Create or verify the non-circular reference to one sealed candidate."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from release_evidence import (
    DIGEST,
    REPOSITORY_ID,
    file_hash,
    positive_integer,
    read_object,
    require_match,
    validate_candidate_manifest,
    write_object,
)

STATUS = "shadow-non-authoritative"
WORKFLOW_ID = 316223904
WORKFLOW_PATH = ".github/workflows/release-shadow.yml"


def expected(args: argparse.Namespace) -> dict[str, Any]:
    manifest = read_object(args.manifest, "candidate manifest")
    validate_candidate_manifest(manifest)
    if args.manifest_run_attempt != 1:
        raise ValueError("candidate manifest run must be Actions attempt 1")
    if args.manifest_workflow_id != WORKFLOW_ID:
        raise ValueError("candidate manifest workflow ID changed")
    for value, label in (
        (args.manifest_run_id, "manifest run ID"),
        (args.manifest_artifact_id, "manifest artifact ID"),
    ):
        positive_integer(value, label)
    require_match(args.manifest_artifact_digest, DIGEST, "manifest artifact digest")
    return {
        "schema_version": 1,
        "status": STATUS,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": manifest["source_git_sha"],
        "candidate_run_id": manifest["candidate_run_id"],
        "candidate_run_attempt": manifest["candidate_run_attempt"],
        "manifest_run_id": args.manifest_run_id,
        "manifest_run_attempt": 1,
        "manifest_workflow_id": WORKFLOW_ID,
        "manifest_workflow_path": WORKFLOW_PATH,
        "manifest_artifact_id": args.manifest_artifact_id,
        "manifest_artifact_digest": args.manifest_artifact_digest,
        "manifest_sha256": file_hash(args.manifest),
    }


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--manifest-run-id", type=int, required=True)
    parser.add_argument("--manifest-run-attempt", type=int, default=1)
    parser.add_argument("--manifest-workflow-id", type=int, required=True)
    parser.add_argument("--manifest-artifact-id", type=int, required=True)
    parser.add_argument("--manifest-artifact-digest", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    add_arguments(create)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    add_arguments(verify)
    verify.add_argument("--document", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    value = expected(args)
    if args.command == "create":
        write_object(args.output, value)
        print(file_hash(args.output))
        return 0
    if read_object(args.document, "candidate reference") != value:
        raise ValueError("candidate reference changed")
    print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, ValueError) as error:
        print(f"candidate-reference: {error}", file=sys.stderr)
        raise SystemExit(1) from error
