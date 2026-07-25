#!/usr/bin/env python3
"""Validated GitHub identities used by Linux repository recovery."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any
from urllib.parse import quote

from downstream_channels import DIGEST, SHA40, timestamp
from github_actions import gh_api, read_json

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
RECEIPT_WORKFLOW_ID = 316435347
RECEIPT_WORKFLOW_PATH = ".github/workflows/release-receipt.yml"
SIGNER_WORKFLOW_PATH = ".github/workflows/release-linux-repository-build.yml"
ACTIVE_RUN_STATUSES = frozenset(
    {"queued", "in_progress", "requested", "waiting", "pending"}
)
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")


def positive(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def canonical_digest(value: Any, label: str) -> str:
    if not isinstance(value, str) or DIGEST.fullmatch(value) is None:
        raise ValueError(f"{label} must be a canonical SHA-256 digest")
    return value


def canonical_sha(value: Any, label: str) -> str:
    if not isinstance(value, str) or SHA40.fullmatch(value) is None:
        raise ValueError(f"{label} must be a lowercase Git SHA")
    return value


def canonical_release_ref(value: Any) -> str:
    if not isinstance(value, str) or RELEASE_REF.fullmatch(value) is None:
        raise ValueError("release ref must identify one stable release")
    return value


def repository_identity(value: dict[str, Any], label: str) -> None:
    if (
        value.get("repository", {}).get("id") != REPOSITORY_ID
        or value.get("head_repository", {}).get("id") != REPOSITORY_ID
    ):
        raise ValueError(f"{label} repository identity mismatch")


def validate_failed_receipt_run(
    value: dict[str, Any], *, run_id: int, source_sha: str, release_ref: str
) -> int:
    wanted = {
        "id": run_id,
        "workflow_id": RECEIPT_WORKFLOW_ID,
        "path": RECEIPT_WORKFLOW_PATH,
        "event": "workflow_dispatch",
        "run_attempt": 1,
        "head_sha": source_sha,
        "head_branch": release_ref,
        "status": "completed",
        "conclusion": "failure",
    }
    for field, expected in wanted.items():
        if value.get(field) != expected:
            raise ValueError(f"failed receipt run {field} mismatch")
    repository_identity(value, "failed receipt run")
    return positive(value["workflow_id"], "receipt workflow ID")


def validate_signer_run(
    value: dict[str, Any], *, run_id: int, protected_main_sha: str
) -> int:
    wanted = {
        "id": run_id,
        "path": SIGNER_WORKFLOW_PATH,
        "event": "workflow_dispatch",
        "run_attempt": 1,
        "head_sha": protected_main_sha,
        "head_branch": "main",
    }
    for field, expected in wanted.items():
        if value.get(field) != expected:
            raise ValueError(f"signer run {field} mismatch")
    if (
        value.get("status") not in ACTIVE_RUN_STATUSES
        or value.get("conclusion") is not None
    ):
        raise ValueError("signer run is not the active current attempt")
    repository_identity(value, "signer run")
    return positive(value.get("workflow_id"), "derived signer workflow ID")


def validate_artifact(
    value: dict[str, Any],
    *,
    artifact_id: int,
    name: str,
    digest: str,
    run_id: int,
    source_sha: str,
) -> dict[str, Any]:
    wanted = {
        "id": artifact_id,
        "name": name,
        "digest": digest,
        "expired": False,
    }
    for field, expected in wanted.items():
        if value.get(field) != expected:
            raise ValueError(f"artifact {field} mismatch")
    positive(value.get("size_in_bytes"), "artifact size")
    created_at = timestamp(value.get("created_at"), "artifact created_at")
    updated_at = timestamp(value.get("updated_at"), "artifact updated_at")
    timestamp(value.get("expires_at"), "artifact expires_at")
    if updated_at < created_at:
        raise ValueError("artifact update predates creation")
    workflow = value.get("workflow_run")
    if not isinstance(workflow, dict):
        raise ValueError("artifact workflow identity is missing")
    expected_workflow = {
        "id": run_id,
        "repository_id": REPOSITORY_ID,
        "head_repository_id": REPOSITORY_ID,
        "head_sha": source_sha,
    }
    for field, expected in expected_workflow.items():
        if workflow.get(field) != expected:
            raise ValueError(f"artifact workflow {field} mismatch")
    return value


def artifact_list(value: Any, label: str) -> list[dict[str, Any]]:
    if not isinstance(value, dict) or not isinstance(value.get("artifacts"), list):
        raise ValueError(f"{label} artifact listing is malformed")
    artifacts = value["artifacts"]
    total = value.get("total_count")
    if type(total) is not int or total < 0 or total != len(artifacts):
        raise ValueError(f"{label} artifact listing cardinality differs")
    if not all(isinstance(item, dict) for item in artifacts):
        raise ValueError(f"{label} artifact listing contains a non-object")
    ids = [item.get("id") for item in artifacts]
    if any(type(item) is not int or item <= 0 for item in ids) or len(ids) != len(
        set(ids)
    ):
        raise ValueError(f"{label} artifact IDs are missing or duplicated")
    return artifacts


def result_artifact_names(source_sha: str, release_id: int) -> tuple[str, ...]:
    prefix = f"rmux-downstream-apt_rpm-result-{source_sha}-{release_id}"
    return (
        prefix,
        f"rmux-downstream-apt_rpm-result-envelope-{source_sha}-{release_id}",
        f"rmux-downstream-apt_rpm-result-reference-{source_sha}-{release_id}",
    )


def reject_prior_linux_publication(
    artifacts: list[dict[str, Any]], *, source_sha: str, release_id: int
) -> None:
    forbidden = set(result_artifact_names(source_sha, release_id))
    found = sorted(
        name
        for item in artifacts
        if isinstance(name := item.get("name"), str) and name in forbidden
    )
    if found:
        raise ValueError(
            "failed receipt already produced Linux repository result evidence: "
            + ", ".join(found)
        )


def reject_prior_recovery_result(
    artifacts: list[dict[str, Any]], *, source_sha: str, release_id: int
) -> None:
    forbidden = set(result_artifact_names(source_sha, release_id))
    found = sorted(
        name
        for item in artifacts
        if isinstance(name := item.get("name"), str) and name in forbidden
    )
    if found:
        raise ValueError(
            "Linux repository recovery result already exists: " + ", ".join(found)
        )


def load_run(run_id: int, fixture: Path | None) -> dict[str, Any]:
    value = (
        read_json(fixture)
        if fixture is not None
        else gh_api(f"repos/{REPOSITORY}/actions/runs/{run_id}")
    )
    if not isinstance(value, dict):
        raise ValueError("workflow run response is not an object")
    return value


def load_artifact(artifact_id: int, fixture: Path | None) -> dict[str, Any]:
    value = (
        read_json(fixture)
        if fixture is not None
        else gh_api(f"repos/{REPOSITORY}/actions/artifacts/{artifact_id}")
    )
    if not isinstance(value, dict):
        raise ValueError("artifact response is not an object")
    return value


def load_artifact_pages(endpoint: str, label: str) -> list[dict[str, Any]]:
    artifacts: list[dict[str, Any]] = []
    expected_total: int | None = None
    page = 1
    while True:
        separator = "&" if "?" in endpoint else "?"
        response = gh_api(f"{endpoint}{separator}per_page=100&page={page}")
        if not isinstance(response, dict) or not isinstance(
            response.get("artifacts"), list
        ):
            raise ValueError(f"{label} artifact response has no artifacts array")
        total = response.get("total_count")
        if type(total) is not int or total < 0:
            raise ValueError(f"{label} artifact response has no valid total_count")
        if expected_total is None:
            expected_total = total
        elif total != expected_total:
            raise ValueError(f"{label} artifact total changed during pagination")
        artifacts.extend(response["artifacts"])
        if len(artifacts) >= expected_total:
            break
        if not response["artifacts"]:
            raise ValueError(f"{label} artifact pagination ended early")
        page += 1
    return artifact_list(
        {"total_count": expected_total, "artifacts": artifacts}, label
    )


def load_run_artifacts(run_id: int, fixture: Path | None) -> list[dict[str, Any]]:
    if fixture is not None:
        return artifact_list(read_json(fixture), "receipt")
    return load_artifact_pages(
        f"repos/{REPOSITORY}/actions/runs/{run_id}/artifacts", "receipt"
    )


def load_result_artifacts(
    source_sha: str, release_id: int, fixture: Path | None
) -> list[dict[str, Any]]:
    if fixture is not None:
        return artifact_list(read_json(fixture), "recovery result")
    artifacts: list[dict[str, Any]] = []
    for name in result_artifact_names(source_sha, release_id):
        artifacts.extend(
            load_artifact_pages(
                f"repos/{REPOSITORY}/actions/artifacts?name={quote(name, safe='')}",
                "recovery result",
            )
        )
    ids = [item["id"] for item in artifacts]
    if len(ids) != len(set(ids)):
        raise ValueError("recovery result artifact query returned duplicate IDs")
    return artifacts


def matching_artifact(
    artifacts: list[dict[str, Any]], artifact_id: int
) -> dict[str, Any]:
    matches = [item for item in artifacts if item.get("id") == artifact_id]
    if len(matches) != 1:
        raise ValueError("receipt artifact ID is missing or duplicated")
    return matches[0]
