#!/usr/bin/env python3
"""Fail-closed activation and attestation checks for release publication."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

REPOSITORY = "Helvesec/rmux"
PREDICATE_TYPE = "https://rmux.io/attestations/release-promotion-authorization/v1"
SIGNER_WORKFLOW = "Helvesec/rmux/.github/workflows/release-promote.yml"
MAX_VERIFICATION_BYTES = 8 * 1024 * 1024
CAPABILITIES = {
    "downstream_channels",
    "github_release_publication",
    "policy_audit",
    "promotion_authorization",
    "publication_receipt",
    "signed_tag_creation",
}


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def require_publication_activation(path: Path) -> None:
    ledger = read_object(path, "release activation ledger")
    expected_keys = {
        "schema_version",
        "status",
        "description",
        "cutover_pr",
        "runtime_override_allowed",
        "capabilities",
    }
    capabilities = ledger.get("capabilities")
    prerequisites = {
        "github_release_publication",
        "policy_audit",
        "promotion_authorization",
        "publication_receipt",
        "signed_tag_creation",
    }
    if (
        set(ledger) != expected_keys
        or ledger.get("schema_version") != 1
        or ledger.get("status") != "active"
        or ledger.get("cutover_pr") != "PR8"
        or ledger.get("runtime_override_allowed") is not False
        or not isinstance(capabilities, dict)
        or set(capabilities) != CAPABILITIES
        or any(type(value) is not bool for value in capabilities.values())
        or any(capabilities.get(capability) is not True for capability in prerequisites)
    ):
        raise ValueError(
            "GitHub Release publication prerequisites are not activated by PR8"
        )


def verify_promotion_attestation(
    *,
    gh: Path,
    assets: dict[str, Path],
    bundle: Path,
    predicate: dict[str, Any],
    source_git_sha: str,
    release_ref: str,
) -> None:
    verifier = gh.resolve(strict=True)
    if verifier.is_symlink() or not verifier.is_file():
        raise ValueError("GitHub attestation verifier must be one regular file")
    if bundle.is_symlink() or not bundle.is_file():
        raise ValueError("attestation bundle must be one regular file")
    expected_subjects = {
        record["name"]: record["sha256"]
        for record in predicate["assets"]
        if record["role"] != "checksums"
    }
    if not expected_subjects or set(assets) != set(expected_subjects):
        raise ValueError("promotion attestation subject set is not exhaustive")
    for name in sorted(expected_subjects):
        subject = assets[name]
        if subject.is_symlink() or not subject.is_file():
            raise ValueError(f"attested subject must be one regular file: {name}")
        result = subprocess.run(
            [
                str(verifier),
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
                source_git_sha,
                "--source-digest",
                source_git_sha,
                "--source-ref",
                f"refs/tags/{release_ref}",
                "--predicate-type",
                PREDICATE_TYPE,
                "--deny-self-hosted-runners",
                "--format",
                "json",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if result.returncode != 0:
            raise ValueError("promotion attestation verification failed closed")
        if len(result.stdout) > MAX_VERIFICATION_BYTES:
            raise ValueError("promotion attestation verification output is too large")
        try:
            output = json.loads(result.stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError(
                "promotion attestation verification output is invalid"
            ) from error
        if not isinstance(output, list) or len(output) != 1:
            raise ValueError("exactly one promotion attestation must verify")
        verification = output[0].get("verificationResult")
        statement = (
            verification.get("statement") if isinstance(verification, dict) else None
        )
        raw_subjects = statement.get("subject") if isinstance(statement, dict) else None
        if not isinstance(raw_subjects, list):
            raise ValueError("verified promotion attestation has no subjects")
        actual_subjects = {
            item.get("name"): item.get("digest", {}).get("sha256")
            for item in raw_subjects
            if isinstance(item, dict) and isinstance(item.get("digest"), dict)
        }
        if (
            len(actual_subjects) != len(raw_subjects)
            or actual_subjects != expected_subjects
            or not isinstance(statement, dict)
            or statement.get("predicateType") != PREDICATE_TYPE
            or statement.get("predicate") != predicate
        ):
            raise ValueError("verified promotion attestation statement changed")
