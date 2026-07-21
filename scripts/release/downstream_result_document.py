"""Typed validation for downstream result predicates and envelopes."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from downstream_channels import (
    CHANNELS,
    REPOSITORY_ID,
    RESULT_ENVELOPE_TYPE,
    RESULT_PREDICATE_TYPE,
    SHA40,
    SHA256,
    canonical_file_hash,
    exact_keys,
    file_hash,
    match,
    target_for_channel,
    timestamp,
    validate_artifact,
    validate_downstream_authority,
    validate_embedded_receipt,
    validate_execution_authority,
    validate_release,
    validate_request,
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
    if request_path.is_symlink():
        raise ValueError("exact request document cannot be a symlink")
    request_path = request_path.resolve(strict=True)
    exact_keys(value, PREDICATE_KEYS, "channel result predicate")
    downstream_active = validate_downstream_authority(value)
    execution_active = validate_execution_authority(
        value, downstream_active=downstream_active
    )
    if (
        value["schema_version"] != 1
        or value["predicate_type"] != RESULT_PREDICATE_TYPE
        or value["repository_id"] != REPOSITORY_ID
    ):
        raise ValueError("channel result predicate identity changed")
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
    if (
        state not in {"blocked", "denied-by-policy", "prepared"}
        and not execution_active
    ):
        raise ValueError("channel mutation result lacks execution authority")
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
    observed = timestamp(value["observed_at"], "result observed_at")
    if value["target_evidence"]["observed_at"] != value["observed_at"]:
        raise ValueError("target and result observation timestamps differ")
    if observed < started:
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
    if (
        downstream_active is not request["downstream_authority"]
        or execution_active is not request["execution_authority"]
    ):
        raise ValueError("result authority differs from its exact request")
    request_started = timestamp(request["requested_at"], "request requested_at")
    request_expires = timestamp(request["expires_at"], "request expires_at")
    if not request_started <= started <= request_expires:
        raise ValueError("result mutation start falls outside request TTL")
    return value


def validate_envelope(
    value: dict[str, Any],
    *,
    predicate: dict[str, Any] | None = None,
    predicate_path: Path | None = None,
) -> dict[str, Any]:
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
    downstream_active = validate_downstream_authority(value)
    execution_active = validate_execution_authority(
        value, downstream_active=downstream_active
    )
    if (
        value["schema_version"] != 1
        or value["envelope_type"] != RESULT_ENVELOPE_TYPE
        or value["repository_id"] != REPOSITORY_ID
    ):
        raise ValueError("channel result envelope identity changed")
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
    if (predicate is None) is not (predicate_path is None):
        raise ValueError("predicate and predicate path must be supplied together")
    if predicate is not None and predicate_path is not None:
        if predicate_path.is_symlink():
            raise ValueError("result predicate cannot be a symlink")
        resolved = predicate_path.resolve(strict=True)
        expected = {
            "source_git_sha": predicate["source_git_sha"],
            "release_ref": predicate["release"]["ref"],
            "channel": predicate["channel"],
            "request_sha256": predicate["request_sha256"],
            "predicate_sha256": file_hash(resolved),
        }
        for field, expected_value in expected.items():
            if value[field] != expected_value:
                raise ValueError(f"result envelope changed exact field {field}")
        if (
            downstream_active is not predicate["downstream_authority"]
            or execution_active is not predicate["execution_authority"]
        ):
            raise ValueError("result envelope authority differs from its predicate")
    return value
