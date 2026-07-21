#!/usr/bin/env python3
"""Bind an authorized publication receipt to exact downstream evidence."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    file_hash,
    read_object,
    validate_receipt_reference,
    write_object,
)

REPOSITORY_ID = 1239918790
RECEIPT_WORKFLOW = ".github/workflows/release-receipt.yml"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--envelope", type=Path, required=True)
    parser.add_argument("--predicate-artifact", type=Path, required=True)
    parser.add_argument("--envelope-artifact", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-id", type=int, required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--release-kind", choices=("rc", "stable"), required=True)
    parser.add_argument("--receipt-run-id", type=int, required=True)
    parser.add_argument("--receipt-workflow-id", type=int, required=True)
    parser.add_argument("--predicate-sha256", required=True)
    parser.add_argument("--envelope-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def exact_file_set(root: Path, expected: set[str]) -> None:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("receipt artifact root must be one real directory")
    paths = list(root.rglob("*"))
    if any(path.is_symlink() for path in paths):
        raise ValueError("receipt artifact contains a symlink")
    names = {path.relative_to(root).as_posix() for path in paths if path.is_file()}
    if names != expected:
        raise ValueError(f"receipt artifact file set differs: {sorted(names)}")


def artifact_reference(value: dict[str, Any]) -> dict[str, Any]:
    return {
        "artifact_id": value["artifact_id"],
        "name": value["name"],
        "archive_digest": value["digest"],
        "size_in_bytes": value["size_in_bytes"],
    }


def build(args: argparse.Namespace) -> None:
    exact_file_set(
        args.predicate.parent,
        {
            "publication-receipt-predicate.json",
            "publication-receipt.sigstore.json",
            "release-state.json",
        },
    )
    exact_file_set(args.envelope.parent, {"publication-receipt-envelope.json"})
    predicate = read_object(args.predicate, "publication receipt predicate")
    envelope = read_object(args.envelope, "publication receipt envelope")
    predicate_artifact = artifact_reference(
        read_object(args.predicate_artifact, "receipt artifact metadata")
    )
    envelope_artifact = artifact_reference(
        read_object(args.envelope_artifact, "receipt envelope artifact metadata")
    )
    receipt = predicate.get("receipt")
    release = predicate.get("release")
    expected_receipt = {
        "run_id": args.receipt_run_id,
        "run_attempt": 1,
        "workflow_id": args.receipt_workflow_id,
        "workflow_path": RECEIPT_WORKFLOW,
    }
    if (
        predicate.get("status") != "downstream-authorized"
        or predicate.get("downstream_authority") is not True
        or predicate.get("repository_id") != REPOSITORY_ID
        or predicate.get("source_git_sha") != args.source_sha
        or receipt != expected_receipt
        or not isinstance(release, dict)
    ):
        raise ValueError("publication receipt predicate identity differs")
    if (
        release.get("id") != args.release_id
        or release.get("ref") != args.release_ref
        or release.get("kind") != args.release_kind
        or release.get("immutable") is not True
    ):
        raise ValueError("publication receipt release identity differs")
    predicate_sha = file_hash(args.predicate)
    envelope_sha = file_hash(args.envelope)
    if predicate_sha != args.predicate_sha256 or envelope_sha != args.envelope_sha256:
        raise ValueError("publication receipt document digest differs")
    if (
        envelope.get("status") != "downstream-authorized"
        or envelope.get("downstream_authority") is not True
        or envelope.get("source_git_sha") != args.source_sha
        or envelope.get("release_id") != args.release_id
        or envelope.get("release_ref") != args.release_ref
        or envelope.get("receipt") != receipt
        or envelope.get("predicate_sha256") != predicate_sha
        or envelope.get("receipt_bundle") != predicate_artifact
    ):
        raise ValueError("publication receipt envelope identity differs")
    attestation = envelope.get("attestation")
    if (
        not isinstance(attestation, dict)
        or attestation.get("bundle_file") != "publication-receipt.sigstore.json"
        or attestation.get("bundle_sha256") != file_hash(args.bundle)
    ):
        raise ValueError("publication receipt attestation differs")
    reference = {
        "schema_version": 1,
        "status": "downstream-authorized",
        "downstream_authority": True,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": args.source_sha,
        "release": release,
        "receipt": receipt,
        "predicate_bundle": predicate_artifact,
        "predicate_sha256": predicate_sha,
        "envelope_bundle": envelope_artifact,
        "envelope_sha256": envelope_sha,
        "attestation": attestation,
        "verified_at": predicate["verified_at"],
    }
    validate_receipt_reference(reference)
    write_object(args.output, reference)


if __name__ == "__main__":
    try:
        build(parse_args())
    except (KeyError, OSError, ValueError) as error:
        print(f"build-downstream-receipt-reference: {error}", file=sys.stderr)
        raise SystemExit(1) from error
