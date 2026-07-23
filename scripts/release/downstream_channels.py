#!/usr/bin/env python3
"""Fail-closed primitives for downstream release evidence."""

from __future__ import annotations

import hashlib
import json
import re
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from strict_json import read_json_object
from release_authority import (
    DISARMED_EVIDENCE_STATUS,
    DOWNSTREAM_AUTHORIZED_STATUS,
    evidence_status,
    validate_evidence_authority,
)

ROOT = Path(__file__).resolve().parents[2]
RELEASE_DIR = ROOT / ".github" / "release"
CHANNEL_CONTRACT = RELEASE_DIR / "downstream-channel-contract.json"
CHANNEL_POLICY = RELEASE_DIR / "channel-policy.json"
REPOSITORY_CONTRACT = RELEASE_DIR / "downstream-repositories.json"
REPOSITORY_ID = 1239918790
STATUS = DISARMED_EVIDENCE_STATUS
AUTHORIZED_STATUS = DOWNSTREAM_AUTHORIZED_STATUS
CHANNELS = (
    "apt_rpm",
    "chocolatey",
    "crates_io",
    "homebrew_core",
    "homebrew_tap",
    "rmux_io",
    "scoop",
    "snap_candidate",
    "snap_stable",
    "web_share",
    "winget",
)
RUNNER_IMAGES = {"ubuntu-22.04", "ubuntu-22.04-arm", "windows-latest"}
SHA40 = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
SAFE_NAME = re.compile(r"[A-Za-z0-9._+@=-]+")
SAFE_EXTERNAL_ID = re.compile(r"[A-Za-z0-9._:/#@+-]+")
INTENT = re.compile(r"[A-Za-z0-9._:-]{8,128}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
RESULT_PREDICATE_TYPE = (
    "https://rmux.io/attestations/release-downstream-channel-result/v1"
)
RESULT_ENVELOPE_TYPE = "https://rmux.io/envelopes/release-downstream-channel-result/v1"


def downstream_status(active: bool) -> str:
    return evidence_status(active, AUTHORIZED_STATUS)


def validate_downstream_authority(
    value: dict[str, Any], *, include_execution: bool = False
) -> bool:
    fields = ["downstream_authority"]
    if include_execution:
        fields.append("execution_authority")
    return validate_evidence_authority(
        value,
        authority_fields=fields,
        active_status=AUTHORIZED_STATUS,
    )


def validate_execution_authority(
    value: dict[str, Any], *, downstream_active: bool, include_enabled: bool = False
) -> bool:
    fields = ["execution_authority"]
    if include_enabled:
        fields.append("execution_enabled")
    authorities = [value.get(field) for field in fields]
    if any(type(authority) is not bool for authority in authorities):
        raise ValueError("execution authority fields must be boolean")
    if len(set(authorities)) != 1:
        raise ValueError("execution authority fields disagree")
    active = authorities[0]
    if active and not downstream_active:
        raise ValueError("execution authority cannot exceed downstream authority")
    return active


def read_object(path: Path, label: str) -> dict[str, Any]:
    return read_json_object(path, label)


def write_object(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def canonical_hash(value: Any) -> str:
    encoded = json.dumps(
        value, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def canonical_file_hash(value: Any) -> str:
    rendered = json.dumps(value, indent=2, sort_keys=True) + "\n"
    return hashlib.sha256(rendered.encode("utf-8")).hexdigest()


def file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValueError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def positive(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def match(value: Any, pattern: re.Pattern[str], label: str) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise ValueError(f"{label} is not canonical")
    return value


def timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be a canonical UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"{label} is not a valid timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{label} must use UTC")
    rendered = parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    if rendered != value:
        raise ValueError(f"{label} must be canonically encoded")
    return parsed


def validate_artifact(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object")
    exact_keys(
        value,
        {"artifact_id", "name", "archive_digest", "size_in_bytes"},
        label,
    )
    positive(value["artifact_id"], f"{label} artifact ID")
    match(value["name"], SAFE_NAME, f"{label} name")
    match(value["archive_digest"], DIGEST, f"{label} archive digest")
    positive(value["size_in_bytes"], f"{label} size")
    return value


def validate_release(value: Any, source_sha: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("release identity must be an object")
    exact_keys(
        value,
        {"id", "ref", "intent_id", "kind", "tag_object_sha", "immutable"},
        "release identity",
    )
    positive(value["id"], "release ID")
    ref = match(value["ref"], RELEASE_REF, "release ref")
    match(value["intent_id"], INTENT, "release intent ID")
    match(value["tag_object_sha"], SHA40, "tag object SHA")
    if value["kind"] not in {"rc", "stable"}:
        raise ValueError("release kind must be rc or stable")
    if (value["kind"] == "rc") is not ("-rc." in ref):
        raise ValueError("release kind and ref disagree")
    if value["immutable"] is not True or not match(source_sha, SHA40, "source SHA"):
        raise ValueError("downstream release must be immutable and SHA-bound")
    return value


def validate_embedded_receipt(
    value: dict[str, Any], source_sha: str, release: dict[str, Any]
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("embedded receipt must be an object")
    exact_keys(
        value,
        {
            "run_id",
            "run_attempt",
            "workflow_id",
            "workflow_path",
            "predicate_bundle",
            "predicate_sha256",
            "envelope_bundle",
            "envelope_sha256",
            "attestation",
            "verified_at",
        },
        "embedded receipt",
    )
    positive(value["run_id"], "receipt run ID")
    positive(value["workflow_id"], "receipt workflow ID")
    if value["run_attempt"] != 1 or value["workflow_path"] != (
        ".github/workflows/release-receipt.yml"
    ):
        raise ValueError("receipt must be an attempt-1 release-receipt run")
    predicate = validate_artifact(value["predicate_bundle"], "receipt bundle")
    envelope = validate_artifact(value["envelope_bundle"], "receipt envelope")
    expected = f"rmux-publication-receipt-{source_sha}-{release['id']}"
    expected_envelope = (
        f"rmux-publication-receipt-envelope-{source_sha}-{release['id']}"
    )
    if predicate["name"] != expected or envelope["name"] != expected_envelope:
        raise ValueError("receipt artifact names do not bind release identity")
    match(value["predicate_sha256"], SHA256, "receipt predicate digest")
    match(value["envelope_sha256"], SHA256, "receipt envelope digest")
    attestation = value["attestation"]
    if not isinstance(attestation, dict):
        raise ValueError("receipt attestation must be an object")
    exact_keys(
        attestation,
        {"attestation_id", "bundle_file", "bundle_sha256"},
        "receipt attestation",
    )
    if (
        not isinstance(attestation["attestation_id"], str)
        or not 1 <= len(attestation["attestation_id"]) <= 256
        or attestation["bundle_file"] != "publication-receipt.sigstore.json"
    ):
        raise ValueError("receipt attestation identity changed")
    match(attestation["bundle_sha256"], SHA256, "receipt attestation digest")
    timestamp(value["verified_at"], "receipt verified_at")
    return value


def validate_receipt_reference(value: dict[str, Any]) -> dict[str, Any]:
    exact_keys(
        value,
        {
            "schema_version",
            "status",
            "downstream_authority",
            "repository_id",
            "source_git_sha",
            "release",
            "receipt",
            "predicate_bundle",
            "predicate_sha256",
            "envelope_bundle",
            "envelope_sha256",
            "attestation",
            "verified_at",
        },
        "receipt reference",
    )
    validate_downstream_authority(value)
    if value["schema_version"] != 1 or value["repository_id"] != REPOSITORY_ID:
        raise ValueError("publication receipt reference identity changed")
    source_sha = match(value["source_git_sha"], SHA40, "receipt source SHA")
    release = validate_release(value["release"], source_sha)
    embedded = {
        **value["receipt"],
        "predicate_bundle": value["predicate_bundle"],
        "predicate_sha256": value["predicate_sha256"],
        "envelope_bundle": value["envelope_bundle"],
        "envelope_sha256": value["envelope_sha256"],
        "attestation": value["attestation"],
        "verified_at": value["verified_at"],
    }
    validate_embedded_receipt(embedded, source_sha, release)
    return value


def load_contract() -> dict[str, Any]:
    contract = read_object(CHANNEL_CONTRACT, "downstream channel contract")
    channels = contract.get("channels")
    if not isinstance(channels, list):
        raise ValueError("downstream channel contract is missing channels")
    names = [item.get("name") for item in channels if isinstance(item, dict)]
    if names != list(CHANNELS):
        raise ValueError("downstream channels must be exhaustive, sorted, and unique")
    result_evidence = contract.get("result_evidence", {})
    retry = contract.get("retry", {})
    receipt_gate = contract.get("receipt_gate", {})
    if (
        contract.get("status") != "atomic-authority-bound"
        or contract.get("activation", {}).get("required_value")
        != "matches-atomic-ledger"
        or contract.get("execution", {}).get("privileged_job_condition")
        != "release-activation-ledger"
        or contract.get("execution", {}).get("rmux_io_last") is not True
        or receipt_gate.get("required_downstream_authority") != "matches-atomic-ledger"
        or retry.get("native_rebuild_allowed") is not False
        or retry.get("maximum_retry_depth") != 1
        or retry.get("retryable_states") != ["failed-transient", "prepared"]
        or result_evidence.get("result_reference_ready") is not True
        or result_evidence.get("result_reference_schema")
        != ".github/release/schemas/downstream-channel-result-reference.schema.json"
        or result_evidence.get("attestation_verification_ready") is not True
        or result_evidence.get("attestation_verifier")
        != "scripts/release/verify-channel-result-attestation.py"
        or result_evidence.get("result_aggregation_ready") is not True
        or result_evidence.get("aggregation_blockers") != []
        or result_evidence.get("summary_schema")
        != ".github/release/schemas/downstream-channel-summary.schema.json"
        or result_evidence.get("summary_phases") != ["pre-site", "final"]
        or result_evidence.get("pre_site_result_count") != 10
        or result_evidence.get("final_result_count") != 11
        or result_evidence.get("rmux_io_pre_site_digest_field")
        != "pre_site_summary_sha256"
    ):
        raise ValueError("downstream channel contract is not fail-closed")
    return contract


def contract_channels() -> dict[str, dict[str, Any]]:
    return {item["name"]: item for item in load_contract()["channels"]}


def target_for_channel(channel: str) -> dict[str, Any]:
    contracted = contract_channels()[channel]
    key = contracted["target_repository_key"]
    services = {
        "chocolatey": "community.chocolatey.org",
        "crates_io": "crates.io",
        "snap_candidate": "snapcraft.io/candidate",
        "snap_stable": "snapcraft.io/stable",
    }
    if key is None:
        return {
            "target_kind": contracted["target_kind"],
            "repository_key": None,
            "repository_id": None,
            "repository_full_name": None,
            "default_branch": None,
            "path": None,
            "external_service": services[channel],
        }
    registry = read_object(REPOSITORY_CONTRACT, "downstream repository registry")
    matches = [
        item for item in registry.get("repositories", []) if item.get("key") == key
    ]
    if len(matches) != 1:
        raise ValueError(f"missing unique pinned repository for channel {channel}")
    repository = matches[0]
    return {
        "target_kind": contracted["target_kind"],
        "repository_key": key,
        "repository_id": repository["id"],
        "repository_full_name": repository["full_name"],
        "default_branch": repository["default_branch"],
        "path": repository["required_path"],
        "external_service": None,
    }


def validate_payload(value: dict[str, Any], channel: str, source_sha: str) -> None:
    from downstream_payload import validate_payload_document

    validate_payload_document(value, channel=channel, source_sha=source_sha)


def validate_request(value: dict[str, Any]) -> dict[str, Any]:
    exact_keys(
        value,
        {
            "schema_version",
            "status",
            "downstream_authority",
            "execution_authority",
            "execution_enabled",
            "repository_id",
            "source_git_sha",
            "release",
            "receipt",
            "plan_sha256",
            "plan_entry",
            "channel",
            "operation",
            "retry_depth",
            "idempotency_key",
            "retry_of_request_sha256",
            "payload_artifact",
            "payload_set_sha256",
            "pre_site_summary_sha256",
            "target",
            "previous_result",
            "rebuild_native",
            "requested_at",
            "expires_at",
        },
        "channel request",
    )
    downstream_active = validate_downstream_authority(value)
    execution_active = validate_execution_authority(
        value,
        downstream_active=downstream_active,
        include_enabled=True,
    )
    if (
        value["schema_version"] != 1
        or value["repository_id"] != REPOSITORY_ID
        or value["rebuild_native"] is not False
    ):
        raise ValueError("channel request identity changed")
    source_sha = match(value["source_git_sha"], SHA40, "request source SHA")
    validate_release(value["release"], source_sha)
    validate_embedded_receipt(value["receipt"], source_sha, value["release"])
    channel = value["channel"]
    if channel not in CHANNELS:
        raise ValueError("channel request names an unknown channel")
    plan_entry = value["plan_entry"]
    if not isinstance(plan_entry, dict):
        raise ValueError("channel request plan entry must be an object")
    exact_keys(
        plan_entry,
        {
            "name",
            "phase",
            "policy_decision",
            "explicit_opt_in",
            "execution_decision",
            "execution_enabled",
            "payload_ready",
            "payload_roles",
            "depends_on",
            "blockers",
        },
        "channel request plan entry",
    )
    if (
        plan_entry["name"] != channel
        or type(plan_entry["execution_enabled"]) is not bool
    ):
        raise ValueError("channel request plan entry identity changed")
    decision = plan_entry["execution_decision"]
    expected_execution = decision == "enabled"
    if decision not in {"blocked", "denied", "disarmed", "enabled"}:
        raise ValueError("channel request plan decision is invalid")
    if (
        plan_entry["execution_enabled"] is not expected_execution
        or execution_active is not expected_execution
        or (decision == "enabled" and not downstream_active)
        or (decision == "disarmed" and downstream_active)
        or (
            decision in {"disarmed", "enabled"}
            and (
                plan_entry["payload_ready"] is not True or plan_entry["blockers"] != []
            )
        )
    ):
        raise ValueError("channel request authority differs from its exact plan entry")
    from downstream_payload import validate_payload_document

    validate_payload_document(
        value["payload_artifact"],
        channel=channel,
        source_sha=source_sha,
        receipt_predicate_sha256=value["receipt"]["predicate_sha256"],
        release=value["release"],
    )
    if value["payload_set_sha256"] != canonical_hash(
        value["payload_artifact"]["files"]
    ):
        raise ValueError("payload set digest differs from its exact files")
    pre_site_digest = value["pre_site_summary_sha256"]
    if channel == "rmux_io":
        match(pre_site_digest, SHA256, "pre-site summary digest")
        summary_files = [
            item
            for item in value["payload_artifact"]["files"]
            if item["role"] == "channel-truth-summary"
        ]
        if (
            len(summary_files) != 1
            or summary_files[0]["name"] != "pre-site-channel-summary.json"
            or summary_files[0]["sha256"] != pre_site_digest
        ):
            raise ValueError("rmux.io payload does not bind the exact pre-site summary")
    elif pre_site_digest is not None:
        raise ValueError("non-site channel cannot bind a pre-site summary")
    expected_target = target_for_channel(channel)
    if value["target"] != expected_target:
        raise ValueError("channel target differs from the pinned contract")
    match(value["plan_sha256"], SHA256, "plan digest")
    match(
        value["idempotency_key"],
        re.compile(r"rmux-downstream-v1:[0-9a-f]{64}"),
        "idempotency key",
    )
    key_material = {
        "receipt_predicate_sha256": value["receipt"]["predicate_sha256"],
        "channel": channel,
        "release_ref": value["release"]["ref"],
        "payload_set_sha256": value["payload_set_sha256"],
        "target": expected_target,
    }
    if value["idempotency_key"] != f"rmux-downstream-v1:{canonical_hash(key_material)}":
        raise ValueError("idempotency key does not bind receipt, target, and payload")
    if value["operation"] not in {"initial", "retry"}:
        raise ValueError("request operation is invalid")
    if type(value["retry_depth"]) is not int:
        raise ValueError("request retry depth must be an integer")
    if value["operation"] == "initial" and (
        value["retry_depth"] != 0
        or value["retry_of_request_sha256"] is not None
        or value["previous_result"] is not None
    ):
        raise ValueError("initial request cannot name retry evidence")
    if value["operation"] == "retry":
        if value["retry_depth"] != 1:
            raise ValueError("downstream retry depth must equal one")
        match(value["retry_of_request_sha256"], SHA256, "retry request digest")
        previous = value["previous_result"]
        if not isinstance(previous, dict):
            raise ValueError("retry request must bind the previous result")
        exact_keys(
            previous,
            {
                "predicate_artifact_id",
                "predicate_artifact_digest",
                "envelope_artifact_id",
                "envelope_artifact_digest",
                "predicate_sha256",
                "envelope_sha256",
                "request_sha256",
                "state",
                "mutation_started",
                "remote_request_id",
            },
            "previous result",
        )
        positive(previous["predicate_artifact_id"], "previous predicate artifact ID")
        positive(previous["envelope_artifact_id"], "previous envelope artifact ID")
        match(
            previous["predicate_artifact_digest"],
            DIGEST,
            "previous predicate artifact digest",
        )
        match(
            previous["envelope_artifact_digest"],
            DIGEST,
            "previous envelope artifact digest",
        )
        match(previous["predicate_sha256"], SHA256, "previous predicate digest")
        match(previous["envelope_sha256"], SHA256, "previous envelope digest")
        match(previous["request_sha256"], SHA256, "previous request digest")
        if previous["request_sha256"] != value["retry_of_request_sha256"]:
            raise ValueError("previous result does not bind the retry origin")
        from downstream_result import validate_retryable_previous

        validate_retryable_previous(previous)
    requested = timestamp(value["requested_at"], "request requested_at")
    expires = timestamp(value["expires_at"], "request expires_at")
    retention = timestamp(
        value["payload_artifact"]["retention_expires_at"], "payload expiry"
    )
    if expires <= requested or expires - requested > timedelta(hours=24):
        raise ValueError("channel request TTL must be in (0, 24h]")
    if requested < timestamp(
        value["payload_artifact"]["created_at"], "payload created_at"
    ):
        raise ValueError("channel request predates its exact payload evidence")
    if expires > retention:
        raise ValueError("channel request outlives its exact payload")
    return value
