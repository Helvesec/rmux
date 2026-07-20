#!/usr/bin/env python3
"""Verify one exact downstream channel result and its custom attestation."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

from downstream_channels import (
    RESULT_PREDICATE_TYPE,
    file_hash,
    read_object,
)
from downstream_result_document import validate_envelope, validate_predicate

REPOSITORY = "Helvesec/rmux"
MAX_OUTPUT_BYTES = 8 * 1024 * 1024


def regular_file(path: Path, label: str) -> Path:
    if path.is_symlink():
        raise ValueError(f"{label} cannot be a symlink")
    resolved = path.resolve(strict=True)
    if not resolved.is_file():
        raise ValueError(f"{label} must be one regular file")
    return resolved


def verify(args: argparse.Namespace) -> None:
    gh = regular_file(args.gh, "GitHub attestation verifier")
    request_path = regular_file(args.request, "channel request")
    target_path = regular_file(args.target_evidence, "channel target evidence")
    bundle_path = regular_file(args.bundle, "channel result attestation bundle")
    predicate_path = regular_file(args.predicate, "channel result predicate")
    envelope_path = regular_file(args.envelope, "channel result envelope")
    expected_names = {
        request_path: "downstream-channel-request.json",
        target_path: "downstream-channel-target-evidence.json",
        bundle_path: "downstream-channel-result.sigstore.json",
        predicate_path: "downstream-channel-result-predicate.json",
        envelope_path: "downstream-channel-result-envelope.json",
    }
    for path, expected in expected_names.items():
        if path.name != expected:
            raise ValueError(f"channel result file name changed: {path.name}")

    request = read_object(request_path, "channel request")
    predicate = read_object(predicate_path, "channel result predicate")
    envelope = read_object(envelope_path, "channel result envelope")
    validate_predicate(predicate, request, request_path)
    validate_envelope(envelope, predicate=predicate, predicate_path=predicate_path)
    if (
        predicate["source_git_sha"] != args.source_sha
        or predicate["release"]["ref"] != args.release_ref
        or predicate["channel"] != args.channel
        or predicate["target_evidence"]
        != read_object(target_path, "channel target evidence")
        or predicate["subject"]["sha256"] != file_hash(target_path)
        or envelope["attestation"]["bundle_sha256"] != file_hash(bundle_path)
    ):
        raise ValueError("channel result identity differs from exact local bytes")
    signer_path = predicate["producer"]["workflow_path"]
    command = [
        str(gh),
        "attestation",
        "verify",
        str(target_path),
        "--bundle",
        str(bundle_path),
        "--repo",
        REPOSITORY,
        "--signer-workflow",
        f"{REPOSITORY}/{signer_path}",
        "--signer-digest",
        args.source_sha,
        "--source-digest",
        args.source_sha,
        "--source-ref",
        f"refs/tags/{args.release_ref}",
        "--predicate-type",
        RESULT_PREDICATE_TYPE,
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
        raise ValueError("channel result attestation verification failed closed")
    if len(result.stdout) > MAX_OUTPUT_BYTES:
        raise ValueError("channel result attestation output is too large")
    try:
        output = json.loads(result.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("channel result attestation output is invalid") from error
    if not isinstance(output, list) or len(output) != 1:
        raise ValueError("exactly one channel result attestation must verify")
    verification = output[0].get("verificationResult")
    if not isinstance(verification, dict):
        raise ValueError("channel result attestation has no verification result")
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
        raise ValueError("channel result lacks signed verification evidence")
    expected_subject = [
        {
            "name": "downstream-channel-target-evidence.json",
            "digest": {"sha256": file_hash(target_path)},
        }
    ]
    if (
        statement.get("subject") != expected_subject
        or statement.get("predicateType") != RESULT_PREDICATE_TYPE
        or statement.get("predicate") != predicate
    ):
        raise ValueError("verified channel result differs from exact local bytes")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--gh", type=Path, required=True)
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--target-evidence", type=Path, required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--envelope", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--channel", required=True)
    return parser.parse_args()


def main() -> int:
    verify(parse_args())
    print("channel-result-attestation-ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"verify-channel-result-attestation: {error}", file=sys.stderr)
        raise SystemExit(1) from error
