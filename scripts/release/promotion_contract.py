#!/usr/bin/env python3
"""Validate the disarmed tag, promotion, and receipt contracts."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
ATOMIC_SCHEMA_STATUS = "atomic-authority-bound"

EXPECTED_PUBLICATION = {
    "enabled": False,
    "promoter_concurrency": "rmux-release-promote-${release_ref}",
    "promoter_trigger": "workflow_dispatch",
    "promoter_workflow": ".github/workflows/release-promote.yml",
    "public_triggers": False,
    "publication_permissions": False,
    "publication_secrets": False,
    "receipt_concurrency": "rmux-release-receipt-${release_ref}",
    "receipt_required_run_attempt": 1,
    "receipt_trigger": "workflow_dispatch",
    "receipt_workflow": ".github/workflows/release-receipt.yml",
    "tag_authoring_concurrency": "rmux-release-tag-authoring-${release_ref}",
    "tag_authoring_trigger": "workflow_dispatch",
    "tag_authoring_workflow": ".github/workflows/release-tag-authoring.yml",
}

RELEASE_WORKFLOWS = {
    "tag authoring": (
        ".github/workflows/release-tag-authoring.yml",
        "workflow_dispatch",
        "rmux-release-tag-authoring-${{ inputs.release_ref }}",
    ),
    "promoter": (
        ".github/workflows/release-promote.yml",
        "workflow_dispatch",
        "rmux-release-promote-${{ inputs.release_ref }}",
    ),
    "receipt": (
        ".github/workflows/release-receipt.yml",
        "workflow_dispatch",
        "rmux-release-receipt-${{ inputs.release_ref }}",
    ),
}

SCHEMA_REQUIRED_FIELDS = {
    "promotion-authorization-predicate.schema.json": {
        "candidate",
        "signed_tag",
        "policy_audit",
        "authorization",
        "assets",
    },
    "promotion-authorization-envelope.schema.json": {
        "predicate_sha256",
        "attestation",
        "authorization_bundle",
        "public_metadata_assets",
    },
    "publication-receipt-predicate.schema.json": {
        "release",
        "candidate",
        "policy_audit",
        "authorization",
        "receipt",
        "assets",
    },
    "publication-receipt-envelope.schema.json": {
        "predicate_sha256",
        "attestation",
        "receipt_bundle",
    },
}


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON object key: {key!r}")
        value[key] = item
    return value


def _load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {path.relative_to(ROOT)}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path.relative_to(ROOT)} must contain an object")
    return value


def _required_strings(value: Any, label: str) -> set[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"{label} must be an array of strings")
    if len(value) != len(set(value)):
        raise ValueError(f"{label} contains duplicates")
    return set(value)


def validate_candidate_policy(value: Any) -> None:
    """Bind the disabled publication policy to its exact workflows."""
    if value != EXPECTED_PUBLICATION:
        raise ValueError("candidate delta acquired publication authority")
    for label, (path, trigger, concurrency_group) in RELEASE_WORKFLOWS.items():
        text = (ROOT / path).read_text(encoding="utf-8")
        if f"on:\n  {trigger}:" not in text:
            raise ValueError(f"release {label} trigger changed")
        other = "workflow_dispatch" if trigger == "workflow_call" else "workflow_call"
        if f"\n  {other}:" in text:
            raise ValueError(f"release {label} gained a second trigger")
        if (
            f"group: {concurrency_group}" not in text
            or "cancel-in-progress: false" not in text
        ):
            raise ValueError(f"release {label} concurrency changed")
    receipt = (ROOT / EXPECTED_PUBLICATION["receipt_workflow"]).read_text(
        encoding="utf-8"
    )
    if 'test "$GITHUB_RUN_ATTEMPT" = 1' not in receipt:
        raise ValueError("release receipt must reject every rerun")


def validate_schemas(schema_dir: Path) -> None:
    """Validate the split signed predicates and post-upload envelopes."""
    schemas = {
        filename: _load(schema_dir / filename) for filename in SCHEMA_REQUIRED_FIELDS
    }
    for filename, required_fields in SCHEMA_REQUIRED_FIELDS.items():
        schema = schemas[filename]
        if schema.get("x-rmux-status") != ATOMIC_SCHEMA_STATUS:
            raise ValueError(f"{filename} lost its atomic authority binding")
        if len(schema.get("oneOf", [])) != 2:
            raise ValueError(f"{filename} must define disarmed and authorized states")
        if schema.get("additionalProperties") is not False:
            raise ValueError(f"{filename} must fail closed on unknown fields")
        required = _required_strings(schema.get("required"), f"{filename}.required")
        missing = required_fields - required
        if missing:
            raise ValueError(f"{filename} is missing required fields {sorted(missing)}")

    for workflow in sorted((ROOT / ".github" / "workflows").glob("*.y*ml")):
        text = workflow.read_text(encoding="utf-8")
        referenced = sorted(name for name in schemas if name in text)
        if referenced:
            raise ValueError(
                f"{workflow.relative_to(ROOT)} consumes draft authority schemas: "
                f"{referenced}"
            )

    receipt_identity = schemas["publication-receipt-predicate.schema.json"]["$defs"][
        "receipt_identity"
    ]["properties"]
    if receipt_identity["run_attempt"].get("const") != 1:
        raise ValueError("receipt-only runs must be attempt 1")
    authorization_identity = schemas["promotion-authorization-predicate.schema.json"][
        "$defs"
    ]["authorization_identity"]["properties"]
    if authorization_identity["run_attempt"].get("const") != 1:
        raise ValueError("promotion authorization runs must be attempt 1")

    for filename in (
        "promotion-authorization-predicate.schema.json",
        "publication-receipt-predicate.schema.json",
    ):
        properties = schemas[filename]["properties"]
        if any(
            name in properties
            for name in (
                "attestation_id",
                "authorization_bundle_artifact_id",
                "receipt_bundle_artifact_id",
            )
        ):
            raise ValueError(f"{filename} contains post-signature identifiers")
    for filename in (
        "promotion-authorization-envelope.schema.json",
        "publication-receipt-envelope.schema.json",
    ):
        properties = schemas[filename]["properties"]
        if "envelope_sha256" in properties or "envelope_artifact_id" in properties:
            raise ValueError(f"{filename} contains a self-reference")
