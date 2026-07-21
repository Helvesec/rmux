#!/usr/bin/env python3
"""Verify the exact publication-receipt subject and custom attestation."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

from strict_json import read_json_object


REPOSITORY = "Helvesec/rmux"
SIGNER_WORKFLOW = "Helvesec/rmux/.github/workflows/release-receipt.yml"
PREDICATE_TYPE = "https://rmux.io/attestations/release-publication-receipt/v1"
SHA40 = re.compile(r"[0-9a-f]{40}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
MAX_OUTPUT_BYTES = 8 * 1024 * 1024


def regular_file(path: Path, label: str) -> Path:
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not resolved.is_file():
        raise ValueError(f"{label} must be one regular file")
    return resolved


def read_object(path: Path, label: str) -> dict[str, Any]:
    return read_json_object(path, label)


def file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(args: argparse.Namespace) -> None:
    gh = regular_file(args.gh, "GitHub attestation verifier")
    subject = regular_file(args.release_state, "release-state subject")
    bundle = regular_file(args.bundle, "receipt attestation bundle")
    predicate_path = regular_file(args.predicate, "receipt predicate")
    if subject.name != "release-state.json":
        raise ValueError("receipt subject name changed")
    if bundle.name != "publication-receipt.sigstore.json":
        raise ValueError("receipt attestation bundle name changed")
    if predicate_path.name != "publication-receipt-predicate.json":
        raise ValueError("receipt predicate name changed")
    if SHA40.fullmatch(args.source_sha) is None:
        raise ValueError("receipt source SHA is invalid")
    if RELEASE_REF.fullmatch(args.release_ref) is None:
        raise ValueError("receipt release ref is invalid")
    predicate = read_object(predicate_path, "receipt predicate")
    if (
        predicate.get("predicate_type") != PREDICATE_TYPE
        or predicate.get("repository_id") != 1239918790
        or predicate.get("source_git_sha") != args.source_sha
        or predicate.get("release", {}).get("ref") != args.release_ref
        or predicate.get("status") != "disarmed-non-authoritative"
        or predicate.get("downstream_authority") is not False
    ):
        raise ValueError("receipt predicate identity or authority changed")
    command = [
        str(gh),
        "attestation",
        "verify",
        str(subject),
        "--bundle",
        str(bundle),
        "--repo",
        REPOSITORY,
        "--signer-workflow",
        SIGNER_WORKFLOW,
        "--signer-digest",
        args.source_sha,
        "--source-digest",
        args.source_sha,
        "--source-ref",
        f"refs/tags/{args.release_ref}",
        "--predicate-type",
        PREDICATE_TYPE,
        "--deny-self-hosted-runners",
        "--format",
        "json",
    ]
    result = subprocess.run(
        command,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise ValueError("receipt attestation verification failed closed")
    if len(result.stdout) > MAX_OUTPUT_BYTES:
        raise ValueError("receipt attestation verification output is too large")
    try:
        output = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("receipt attestation verification output is invalid") from error
    if not isinstance(output, list) or len(output) != 1:
        raise ValueError("exactly one receipt attestation must verify")
    verification = output[0].get("verificationResult")
    if not isinstance(verification, dict):
        raise ValueError("receipt attestation has no verification result")
    statement = verification.get("statement")
    signature = verification.get("signature")
    timestamps = verification.get("verifiedTimestamps")
    if (
        not isinstance(statement, dict)
        or not isinstance(signature, dict)
        or not isinstance(signature.get("certificate"), dict)
        or not isinstance(timestamps, list)
        or not timestamps
    ):
        raise ValueError("receipt attestation lacks signed verification evidence")
    expected_subject = [
        {"name": "release-state.json", "digest": {"sha256": file_hash(subject)}}
    ]
    if (
        statement.get("subject") != expected_subject
        or statement.get("predicateType") != PREDICATE_TYPE
        or statement.get("predicate") != predicate
    ):
        raise ValueError("verified receipt statement differs from exact local bytes")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--gh", type=Path, required=True)
    parser.add_argument("--release-state", type=Path, required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-ref", required=True)
    return parser.parse_args()


def main() -> int:
    verify(parse_args())
    print("receipt-attestation-ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"verify-receipt-attestation: {error}", file=sys.stderr)
        raise SystemExit(1) from error
