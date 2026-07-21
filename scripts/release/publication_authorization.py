"""Validate publication authorization authority and envelope evidence."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from release_authority import load_authority
from release_evidence import (
    REPOSITORY_ID,
    SHA256,
    STATUS,
    exact_keys,
    file_hash,
    positive_integer,
    require_match,
    validate_artifact_reference,
)

AUTHORIZATION_TYPE = "https://rmux.io/attestations/release-promotion-authorization/v1"
AUTHORIZATION_ENVELOPE_TYPE = AUTHORIZATION_TYPE.replace("attestations", "envelopes")


def authority_enabled(
    *, simulation: bool, activation_ledger: Path, capabilities: tuple[str, ...]
) -> bool:
    return not simulation and load_authority(activation_ledger).permits(capabilities)


def validate_authorization_envelope(
    value: dict[str, Any], predicate_path: Path, predicate: dict[str, Any]
) -> None:
    exact_keys(
        value,
        {
            "schema_version",
            "envelope_type",
            "status",
            "publication_authority",
            "repository_id",
            "source_git_sha",
            "release_ref",
            "release_intent_id",
            "authorization",
            "predicate_sha256",
            "sha256sums_sha256",
            "attestation",
            "authorization_bundle",
            "public_metadata_assets",
            "created_at",
        },
        "authorization envelope",
    )
    authority = predicate["publication_authority"]
    expected_status = "promotion-authorized" if authority is True else STATUS
    if (
        value["schema_version"] != 1
        or value["envelope_type"] != AUTHORIZATION_ENVELOPE_TYPE
        or value["status"] != expected_status
        or value["publication_authority"] is not authority
        or value["repository_id"] != REPOSITORY_ID
        or value["source_git_sha"] != predicate["source_git_sha"]
        or value["release_ref"] != predicate["release"]["ref"]
        or value["release_intent_id"] != predicate["release"]["intent_id"]
        or value["authorization"] != predicate["authorization"]
        or value["predicate_sha256"] != file_hash(predicate_path)
        or value["sha256sums_sha256"] != predicate["sha256sums_sha256"]
    ):
        raise ValueError("authorization envelope does not bind its exact predicate")
    validate_artifact_reference(value["authorization_bundle"], "authorization bundle")
    attestation = value["attestation"]
    if not isinstance(attestation, dict):
        raise ValueError("authorization attestation is missing")
    exact_keys(
        attestation,
        {"attestation_id", "bundle_file", "bundle_sha256"},
        "authorization attestation",
    )
    if attestation["bundle_file"] != "SHA256SUMS.sigstore.json":
        raise ValueError("authorization attestation bundle name changed")
    require_match(attestation["bundle_sha256"], SHA256, "authorization bundle digest")
    metadata = value["public_metadata_assets"]
    if not isinstance(metadata, list) or len(metadata) != 1:
        raise ValueError("authorization must declare exactly one public metadata asset")
    item = metadata[0]
    if not isinstance(item, dict):
        raise ValueError("authorization metadata asset must be an object")
    exact_keys(item, {"name", "role", "size", "sha256"}, "metadata asset")
    if (
        item["name"] != "SHA256SUMS.sigstore.json"
        or item["role"] != "authorization-attestation"
        or item["sha256"] != attestation["bundle_sha256"]
    ):
        raise ValueError("authorization public metadata differs from its attestation")
    positive_integer(item["size"], "authorization metadata size")
