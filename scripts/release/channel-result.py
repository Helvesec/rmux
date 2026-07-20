#!/usr/bin/env python3
"""Create or verify disarmed downstream result predicates and envelopes."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    REPOSITORY_ID,
    RESULT_ENVELOPE_TYPE,
    RESULT_PREDICATE_TYPE,
    STATUS,
    canonical_file_hash,
    file_hash,
    read_object,
    timestamp,
    validate_artifact,
    validate_request,
    write_object,
)
from downstream_result_document import validate_envelope, validate_predicate
from downstream_result import (
    result_state,
    validate_mutation_state,
    validate_producer,
    validate_target_evidence,
)


def expected_predicate(args: argparse.Namespace) -> dict[str, Any]:
    request_path = args.request.resolve(strict=True)
    request = read_object(request_path, "downstream channel request")
    validate_request(request)
    started = timestamp(args.started_at, "result started_at")
    observed = timestamp(args.observed_at, "result observed_at")
    requested = timestamp(request["requested_at"], "request requested_at")
    expires = timestamp(request["expires_at"], "request expires_at")
    if not requested <= started <= expires or observed < started:
        raise ValueError("channel result timing falls outside its request")
    producer = read_object(args.producer, "channel result producer")
    validate_producer(producer, request["channel"])
    state = result_state(args.state)
    validate_mutation_state(state, args.mutation_started, args.remote_request_id)
    target = read_object(args.target_evidence, "channel target evidence")
    validate_target_evidence(
        target,
        channel=request["channel"],
        state=state,
        expected_target=request["target"],
        expected_version=request["release"]["ref"][1:],
    )
    if target["observed_at"] != args.observed_at:
        raise ValueError("target evidence timestamp differs from result")
    target_digest = canonical_file_hash(target)
    if file_hash(args.target_evidence.resolve(strict=True)) != target_digest:
        raise ValueError("target evidence must use canonical JSON encoding")
    value = {
        "schema_version": 1,
        "predicate_type": RESULT_PREDICATE_TYPE,
        "status": STATUS,
        "downstream_authority": False,
        "execution_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": request["source_git_sha"],
        "release": request["release"],
        "receipt": request["receipt"],
        "request_sha256": file_hash(request_path),
        "payload_set_sha256": request["payload_set_sha256"],
        "idempotency_key": request["idempotency_key"],
        "channel": request["channel"],
        "target": request["target"],
        "producer": producer,
        "subject": {
            "name": "downstream-channel-target-evidence.json",
            "sha256": target_digest,
        },
        "state": state,
        "started_at": args.started_at,
        "mutation_started": args.mutation_started,
        "remote_request_id": args.remote_request_id,
        "target_evidence": target,
        "observed_at": args.observed_at,
    }
    return validate_predicate(value, request, request_path)


def expected_envelope(args: argparse.Namespace) -> dict[str, Any]:
    request_path = args.request.resolve(strict=True)
    request = read_object(request_path, "downstream channel request")
    predicate_path = args.predicate.resolve(strict=True)
    predicate = read_object(predicate_path, "channel result predicate")
    validate_predicate(predicate, request, request_path)
    bundle_path = args.attestation_bundle.resolve(strict=True)
    if bundle_path.name != "downstream-channel-result.sigstore.json":
        raise ValueError("result attestation bundle name changed")
    result_bundle = read_object(args.bundle_artifact, "result bundle artifact")
    validate_artifact(result_bundle, "result bundle")
    expected_name = (
        f"rmux-downstream-{predicate['channel']}-result-"
        f"{predicate['source_git_sha']}-{predicate['release']['id']}"
    )
    if result_bundle["name"] != expected_name:
        raise ValueError("result bundle name differs from its exact release")
    created = timestamp(args.created_at, "result envelope created_at")
    if created < timestamp(predicate["observed_at"], "result observed_at"):
        raise ValueError("result envelope predates its predicate")
    value = {
        "schema_version": 1,
        "envelope_type": RESULT_ENVELOPE_TYPE,
        "status": STATUS,
        "downstream_authority": False,
        "execution_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": predicate["source_git_sha"],
        "release_ref": predicate["release"]["ref"],
        "channel": predicate["channel"],
        "request_sha256": predicate["request_sha256"],
        "predicate_sha256": file_hash(predicate_path),
        "attestation": {
            "attestation_id": args.attestation_id,
            "bundle_file": "downstream-channel-result.sigstore.json",
            "bundle_sha256": file_hash(bundle_path),
        },
        "result_bundle": result_bundle,
        "created_at": args.created_at,
    }
    return validate_envelope(value, predicate=predicate, predicate_path=predicate_path)


def predicate_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--producer", type=Path, required=True)
    parser.add_argument("--state", required=True)
    parser.add_argument("--started-at", required=True)
    parser.add_argument("--mutation-started", action="store_true")
    parser.add_argument("--remote-request-id")
    parser.add_argument("--target-evidence", type=Path, required=True)
    parser.add_argument("--observed-at", required=True)


def envelope_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--attestation-id", required=True)
    parser.add_argument("--attestation-bundle", type=Path, required=True)
    parser.add_argument("--bundle-artifact", type=Path, required=True)
    parser.add_argument("--created-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    for command in ("create-predicate", "verify-predicate"):
        child = commands.add_parser(command)
        predicate_arguments(child)
        child.add_argument(
            "--output" if command.startswith("create") else "--document",
            type=Path,
            required=True,
        )
    for command in ("create-envelope", "verify-envelope"):
        child = commands.add_parser(command)
        envelope_arguments(child)
        child.add_argument(
            "--output" if command.startswith("create") else "--document",
            type=Path,
            required=True,
        )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    expected = (
        expected_predicate(args)
        if args.command.endswith("predicate")
        else expected_envelope(args)
    )
    if args.command.startswith("create"):
        write_object(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "downstream result evidence")
        if actual != expected:
            raise ValueError("downstream result evidence changed")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"channel-result: {error}", file=sys.stderr)
        raise SystemExit(1) from error
