"""Exact post-upload references for downstream channel result evidence."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from downstream_channels import (
    CHANNELS,
    REPOSITORY_ID,
    SHA40,
    SHA256,
    exact_keys,
    file_hash,
    match,
    positive,
    read_object,
    target_for_channel,
    timestamp,
    validate_artifact,
    validate_downstream_authority,
    validate_embedded_receipt,
    validate_execution_authority,
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
from downstream_result_document import validate_envelope, validate_predicate

REFERENCE_KEYS = {
    "schema_version",
    "status",
    "downstream_authority",
    "execution_authority",
    "repository_id",
    "source_git_sha",
    "release",
    "receipt",
    "plan_sha256",
    "channel",
    "producer",
    "request_sha256",
    "payload_set_sha256",
    "idempotency_key",
    "state",
    "mutation_started",
    "remote_request_id",
    "public_live",
    "target_evidence",
    "predicate_bundle",
    "predicate_sha256",
    "envelope_bundle",
    "envelope_sha256",
    "attestation",
    "verified_at",
}


def _artifact_reference(
    value: dict[str, Any], *, expected_name: str, source_sha: str, run_id: int
) -> dict[str, Any]:
    exact_keys(
        value,
        {
            "artifact_id",
            "name",
            "digest",
            "size_in_bytes",
            "run_id",
            "source_git_sha",
        },
        "result artifact API metadata",
    )
    if (
        value["name"] != expected_name
        or value["source_git_sha"] != source_sha
        or value["run_id"] != run_id
    ):
        raise ValueError("result artifact API identity changed")
    reference = {
        "artifact_id": value["artifact_id"],
        "name": value["name"],
        "archive_digest": value["digest"],
        "size_in_bytes": value["size_in_bytes"],
    }
    validate_artifact(reference, "result artifact reference")
    return reference


def validate_reference(value: dict[str, Any]) -> dict[str, Any]:
    exact_keys(value, REFERENCE_KEYS, "channel result reference")
    downstream_active = validate_downstream_authority(value)
    execution_active = validate_execution_authority(
        value, downstream_active=downstream_active
    )
    if value["schema_version"] != 1 or value["repository_id"] != REPOSITORY_ID:
        raise ValueError("channel result reference identity changed")
    source_sha = match(value["source_git_sha"], SHA40, "result reference source SHA")
    release = validate_release(value["release"], source_sha)
    validate_embedded_receipt(value["receipt"], source_sha, release)
    match(value["plan_sha256"], SHA256, "result reference plan digest")
    channel = value["channel"]
    if channel not in CHANNELS:
        raise ValueError("result reference channel is unknown")
    producer = validate_producer(value["producer"], channel)
    match(value["request_sha256"], SHA256, "result reference request digest")
    match(value["payload_set_sha256"], SHA256, "result reference payload digest")
    if not isinstance(value["idempotency_key"], str) or not value[
        "idempotency_key"
    ].startswith("rmux-downstream-v1:"):
        raise ValueError("result reference idempotency key changed")
    match(
        value["idempotency_key"].removeprefix("rmux-downstream-v1:"),
        SHA256,
        "result reference idempotency digest",
    )
    state = result_state(value["state"])
    if (
        state not in {"blocked", "denied-by-policy", "prepared"}
        and not execution_active
    ):
        raise ValueError("channel result reference lacks execution authority")
    validate_mutation_state(
        state, value["mutation_started"], value["remote_request_id"]
    )
    target = target_for_channel(channel)
    validate_target_evidence(
        value["target_evidence"],
        channel=channel,
        state=state,
        expected_target=target,
        expected_version=release["ref"][1:],
    )
    validate_remote_identity(value["target_evidence"], value["remote_request_id"])
    if value["public_live"] is not value["target_evidence"]["public_live"]:
        raise ValueError("result reference public state differs from target evidence")
    predicate = validate_artifact(value["predicate_bundle"], "predicate bundle")
    envelope = validate_artifact(value["envelope_bundle"], "envelope bundle")
    expected_predicate_name = (
        f"rmux-downstream-{channel}-result-{source_sha}-{release['id']}"
    )
    expected_envelope_name = (
        f"rmux-downstream-{channel}-result-envelope-{source_sha}-{release['id']}"
    )
    if (
        predicate["name"] != expected_predicate_name
        or envelope["name"] != expected_envelope_name
    ):
        raise ValueError("result reference artifact names changed")
    match(value["predicate_sha256"], SHA256, "result reference predicate digest")
    match(value["envelope_sha256"], SHA256, "result reference envelope digest")
    attestation = value["attestation"]
    if not isinstance(attestation, dict):
        raise ValueError("result reference attestation must be an object")
    exact_keys(
        attestation,
        {"attestation_id", "bundle_file", "bundle_sha256"},
        "result reference attestation",
    )
    if (
        not isinstance(attestation["attestation_id"], str)
        or not attestation["attestation_id"]
        or attestation["bundle_file"] != "downstream-channel-result.sigstore.json"
    ):
        raise ValueError("result reference attestation identity changed")
    match(attestation["bundle_sha256"], SHA256, "result attestation digest")
    timestamp(value["verified_at"], "result reference verified_at")
    positive(producer["run_id"], "result producer run ID")
    return value


def create_reference(
    *,
    request_path: Path,
    predicate_path: Path,
    envelope_path: Path,
    predicate_artifact_path: Path,
    envelope_artifact_path: Path,
    verified_at: str,
) -> dict[str, Any]:
    for path, label in (
        (request_path, "channel request"),
        (predicate_path, "channel result predicate"),
        (envelope_path, "channel result envelope"),
        (predicate_artifact_path, "predicate artifact metadata"),
        (envelope_artifact_path, "envelope artifact metadata"),
    ):
        if path.is_symlink():
            raise ValueError(f"{label} cannot be a symlink")
    request_path = request_path.resolve(strict=True)
    predicate_path = predicate_path.resolve(strict=True)
    envelope_path = envelope_path.resolve(strict=True)
    request = read_object(request_path, "channel request")
    predicate = read_object(predicate_path, "channel result predicate")
    envelope = read_object(envelope_path, "channel result envelope")
    validate_request(request)
    validate_predicate(predicate, request, request_path)
    validate_envelope(envelope, predicate=predicate, predicate_path=predicate_path)
    source_sha = predicate["source_git_sha"]
    channel = predicate["channel"]
    release = predicate["release"]
    run_id = predicate["producer"]["run_id"]
    predicate_artifact = _artifact_reference(
        read_object(predicate_artifact_path, "predicate artifact metadata"),
        expected_name=f"rmux-downstream-{channel}-result-{source_sha}-{release['id']}",
        source_sha=source_sha,
        run_id=run_id,
    )
    envelope_artifact = _artifact_reference(
        read_object(envelope_artifact_path, "envelope artifact metadata"),
        expected_name=(
            f"rmux-downstream-{channel}-result-envelope-{source_sha}-{release['id']}"
        ),
        source_sha=source_sha,
        run_id=run_id,
    )
    if envelope["result_bundle"] != predicate_artifact:
        raise ValueError("result envelope does not name the exact predicate artifact")
    verified = timestamp(verified_at, "result reference verified_at")
    if verified < timestamp(envelope["created_at"], "result envelope created_at"):
        raise ValueError("result reference predates its exact envelope")
    value = {
        "schema_version": 1,
        "status": predicate["status"],
        "downstream_authority": predicate["downstream_authority"],
        "execution_authority": predicate["execution_authority"],
        "repository_id": REPOSITORY_ID,
        "source_git_sha": source_sha,
        "release": release,
        "receipt": request["receipt"],
        "plan_sha256": request["plan_sha256"],
        "channel": channel,
        "producer": predicate["producer"],
        "request_sha256": predicate["request_sha256"],
        "payload_set_sha256": predicate["payload_set_sha256"],
        "idempotency_key": predicate["idempotency_key"],
        "state": predicate["state"],
        "mutation_started": predicate["mutation_started"],
        "remote_request_id": predicate["remote_request_id"],
        "public_live": predicate["target_evidence"]["public_live"],
        "target_evidence": predicate["target_evidence"],
        "predicate_bundle": predicate_artifact,
        "predicate_sha256": file_hash(predicate_path),
        "envelope_bundle": envelope_artifact,
        "envelope_sha256": file_hash(envelope_path),
        "attestation": envelope["attestation"],
        "verified_at": verified_at,
    }
    return validate_reference(value)


def write_reference(path: Path, value: dict[str, Any]) -> None:
    write_object(path, validate_reference(value))
