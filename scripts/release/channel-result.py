#!/usr/bin/env python3
"""Create or verify disarmed downstream result predicates and envelopes."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    CHANNELS,
    REPOSITORY_ID,
    RESULT_ENVELOPE_TYPE,
    RESULT_PREDICATE_TYPE,
    SHA40,
    SHA256,
    STATUS,
    canonical_file_hash,
    exact_keys,
    file_hash,
    match,
    read_object,
    target_for_channel,
    timestamp,
    validate_artifact,
    validate_embedded_receipt,
    validate_release,
    validate_request,
    write_object,
)
from downstream_result import (
    result_state,
    validate_mutation_state,
    validate_producer,
    validate_remote_identity,
    validate_target_evidence,
)


PREDICATE_KEYS = {
    "schema_version",
    "predicate_type",
    "status",
    "downstream_authority",
    "execution_authority",
    "repository_id",
    "source_git_sha",
    "release",
    "receipt",
    "request_sha256",
    "payload_set_sha256",
    "idempotency_key",
    "channel",
    "target",
    "producer",
    "subject",
    "state",
    "started_at",
    "mutation_started",
    "remote_request_id",
    "target_evidence",
    "observed_at",
}


def validate_predicate(
    value: dict[str, Any],
    request: dict[str, Any],
    request_path: Path,
) -> dict[str, Any]:
    if not isinstance(request, dict) or not isinstance(request_path, Path):
        raise ValueError("exact request document is required for result validation")
    validate_request(request)
    request_path = request_path.resolve(strict=True)
    exact_keys(value, PREDICATE_KEYS, "channel result predicate")
    if (
        value["schema_version"] != 1
        or value["predicate_type"] != RESULT_PREDICATE_TYPE
        or value["status"] != STATUS
        or value["downstream_authority"] is not False
        or value["execution_authority"] is not False
        or value["repository_id"] != REPOSITORY_ID
    ):
        raise ValueError("channel result predicate must remain disarmed")
    source_sha = match(value["source_git_sha"], SHA40, "result source SHA")
    release = validate_release(value["release"], source_sha)
    validate_embedded_receipt(value["receipt"], source_sha, release)
    match(value["request_sha256"], SHA256, "result request digest")
    match(value["payload_set_sha256"], SHA256, "result payload digest")
    match(
        value["idempotency_key"],
        re.compile(r"rmux-downstream-v1:[0-9a-f]{64}"),
        "result idempotency key",
    )
    channel = value["channel"]
    if channel not in CHANNELS:
        raise ValueError("channel result names an unknown channel")
    expected_target = target_for_channel(channel)
    if value["target"] != expected_target:
        raise ValueError("result target differs from the pinned channel target")
    state = result_state(value["state"])
    subject = value["subject"]
    if not isinstance(subject, dict):
        raise ValueError("channel result subject must be an object")
    exact_keys(subject, {"name", "sha256"}, "channel result subject")
    if subject["name"] != "downstream-channel-target-evidence.json":
        raise ValueError("channel result subject name changed")
    match(subject["sha256"], SHA256, "channel result subject digest")
    if subject["sha256"] != canonical_file_hash(value["target_evidence"]):
        raise ValueError("channel result subject does not bind exact target evidence")
    validate_producer(value["producer"], channel)
    validate_mutation_state(
        state, value["mutation_started"], value["remote_request_id"]
    )
    validate_target_evidence(
        value["target_evidence"],
        channel=channel,
        state=state,
        expected_target=expected_target,
        expected_version=release["ref"][1:],
    )
    validate_remote_identity(value["target_evidence"], value["remote_request_id"])
    started = timestamp(value["started_at"], "result started_at")
    timestamp(value["observed_at"], "result observed_at")
    if value["target_evidence"]["observed_at"] != value["observed_at"]:
        raise ValueError("target and result observation timestamps differ")
    if timestamp(value["observed_at"], "result observed_at") < started:
        raise ValueError("channel result predates its start")
    expected = {
        "source_git_sha": request["source_git_sha"],
        "release": request["release"],
        "receipt": request["receipt"],
        "request_sha256": file_hash(request_path),
        "payload_set_sha256": request["payload_set_sha256"],
        "idempotency_key": request["idempotency_key"],
        "channel": request["channel"],
        "target": request["target"],
    }
    for field, expected_value in expected.items():
        if value[field] != expected_value:
            raise ValueError(f"result changed exact request field {field}")
    request_started = timestamp(request["requested_at"], "request requested_at")
    request_expires = timestamp(request["expires_at"], "request expires_at")
    if not request_started <= started <= request_expires:
        raise ValueError("result mutation start falls outside request TTL")
    return value


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


def validate_envelope(value: dict[str, Any]) -> dict[str, Any]:
    exact_keys(
        value,
        {
            "schema_version",
            "envelope_type",
            "status",
            "downstream_authority",
            "execution_authority",
            "repository_id",
            "source_git_sha",
            "release_ref",
            "channel",
            "request_sha256",
            "predicate_sha256",
            "attestation",
            "result_bundle",
            "created_at",
        },
        "channel result envelope",
    )
    if (
        value["schema_version"] != 1
        or value["envelope_type"] != RESULT_ENVELOPE_TYPE
        or value["status"] != STATUS
        or value["downstream_authority"] is not False
        or value["execution_authority"] is not False
        or value["repository_id"] != REPOSITORY_ID
    ):
        raise ValueError("channel result envelope must remain disarmed")
    match(value["request_sha256"], SHA256, "envelope request digest")
    match(value["predicate_sha256"], SHA256, "envelope predicate digest")
    validate_artifact(value["result_bundle"], "result bundle")
    attestation = value["attestation"]
    if not isinstance(attestation, dict):
        raise ValueError("result attestation must be an object")
    exact_keys(
        attestation,
        {"attestation_id", "bundle_file", "bundle_sha256"},
        "result attestation",
    )
    if (
        not isinstance(attestation["attestation_id"], str)
        or not 1 <= len(attestation["attestation_id"]) <= 256
        or attestation["bundle_file"] != "downstream-channel-result.sigstore.json"
    ):
        raise ValueError("result attestation identity changed")
    match(attestation["bundle_sha256"], SHA256, "result attestation digest")
    timestamp(value["created_at"], "result envelope created_at")
    return value


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
    return validate_envelope(value)


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
