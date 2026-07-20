"""Resolve and validate the exact Actions artifact namespace for a candidate."""

from __future__ import annotations

import re
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from github_actions import get_repository, get_run, gh_api, read_json

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
WORKFLOW_ID = 277622540
WORKFLOW_PATH = ".github/workflows/ci.yml"
WORKFLOW_NAME = "CI"
PLATFORMS = (
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
)
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


@dataclass(frozen=True)
class ArtifactSpec:
    role: str
    platform_key: str | None
    name: str


@dataclass(frozen=True)
class ResolutionRequest:
    repository: str
    candidate_run_id: int
    source_sha: str
    repository_json: Path | None = None
    run_json: Path | None = None
    artifacts_json: Path | None = None


def expected_specs(source_sha: str) -> tuple[ArtifactSpec, ...]:
    assets = tuple(
        ArtifactSpec(
            "canonical-assets", platform, f"rmux-canonical-{platform}-{source_sha}"
        )
        for platform in PLATFORMS
    )
    provenance = tuple(
        ArtifactSpec(
            "canonical-provenance",
            platform,
            f"rmux-canonical-provenance-{platform}-{source_sha}",
        )
        for platform in PLATFORMS
    )
    return (
        ArtifactSpec("fast-proof", None, f"rmux-fast-proof-{source_sha}"),
        *assets,
        *provenance,
    )


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValueError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def require_equal(actual: Any, expected: Any, label: str) -> None:
    if actual != expected:
        raise ValueError(f"{label} mismatch: expected {expected!r}, got {actual!r}")


def artifact_array(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict):
        artifacts = value.get("artifacts")
        total = value.get("total_count")
        if type(total) is not int or total < 0:
            raise ValueError("artifact fixture has no valid total_count")
        if not isinstance(artifacts, list) or len(artifacts) != total:
            raise ValueError("artifact fixture cardinality differs from total_count")
        value = artifacts
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise ValueError("artifact fixture must be an artifact array")
    return value


def list_run_artifacts(repository: str, run_id: int) -> list[dict[str, Any]]:
    artifacts: list[dict[str, Any]] = []
    expected_total: int | None = None
    page = 1
    while True:
        response = gh_api(
            f"repos/{repository}/actions/runs/{run_id}/artifacts?per_page=100&page={page}"
        )
        if not isinstance(response, dict) or not isinstance(
            response.get("artifacts"), list
        ):
            raise ValueError("run artifact response has no artifacts array")
        total = response.get("total_count")
        if type(total) is not int or total < 0:
            raise ValueError("run artifact response has no valid total_count")
        if expected_total is None:
            expected_total = total
        elif total != expected_total:
            raise ValueError("artifact total changed during pagination")
        current = response["artifacts"]
        artifacts.extend(current)
        if len(artifacts) >= expected_total:
            break
        if not current:
            raise ValueError("artifact pagination ended early")
        page += 1
    if len(artifacts) != expected_total:
        raise ValueError("artifact pagination returned the wrong cardinality")
    return artifacts


def load_resolution_inputs(
    request: ResolutionRequest,
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]], bool]:
    fixtures = (
        request.repository_json,
        request.run_json,
        request.artifacts_json,
    )
    if any(fixtures) and not all(fixtures):
        raise ValueError(
            "repository, run, and artifact fixtures must be provided together"
        )
    if all(fixtures):
        repository = read_json(request.repository_json)
        run = read_json(request.run_json)
        if not isinstance(repository, dict) or not isinstance(run, dict):
            raise ValueError("repository and run fixtures must be objects")
        return (
            repository,
            run,
            artifact_array(read_json(request.artifacts_json)),
            True,
        )
    repository = get_repository(request.repository)
    run = get_run(request.repository, request.candidate_run_id)
    artifacts = list_run_artifacts(request.repository, request.candidate_run_id)
    run_after = get_run(request.repository, request.candidate_run_id)
    for field in (
        "id",
        "workflow_id",
        "path",
        "name",
        "event",
        "head_branch",
        "head_sha",
        "run_attempt",
        "status",
        "conclusion",
        "updated_at",
    ):
        if run.get(field) != run_after.get(field):
            raise ValueError(
                f"candidate run changed while artifacts were listed: {field}"
            )
    return repository, run_after, artifacts, False


def verify_run(
    repository: dict[str, Any], run: dict[str, Any], run_id: int, source_sha: str
) -> dict[str, Any]:
    require_equal(repository.get("id"), REPOSITORY_ID, "repository ID")
    require_equal(repository.get("full_name"), REPOSITORY, "repository name")
    expected = {
        "id": run_id,
        "workflow_id": WORKFLOW_ID,
        "path": WORKFLOW_PATH,
        "name": WORKFLOW_NAME,
        "event": "workflow_dispatch",
        "head_branch": "main",
        "head_sha": source_sha,
        "run_attempt": 1,
        "status": "completed",
        "conclusion": "success",
    }
    for field, value in expected.items():
        require_equal(run.get(field), value, f"candidate run {field}")
    for field in ("repository", "head_repository"):
        identity = run.get(field)
        if not isinstance(identity, dict):
            raise ValueError(f"candidate run {field} identity is missing")
        require_equal(identity.get("id"), REPOSITORY_ID, f"candidate run {field} ID")
        require_equal(
            identity.get("full_name"), REPOSITORY, f"candidate run {field} name"
        )
    return {
        "id": run_id,
        "attempt": 1,
        "workflow_id": WORKFLOW_ID,
        "workflow_path": WORKFLOW_PATH,
        "workflow_name": WORKFLOW_NAME,
        "event": "workflow_dispatch",
        "branch": "main",
        "head_sha": source_sha,
        "status": "completed",
        "conclusion": "success",
    }


def verify_artifact(
    artifact: dict[str, Any], spec: ArtifactSpec, run_id: int, source_sha: str
) -> dict[str, Any]:
    artifact_id = artifact.get("id")
    if type(artifact_id) is not int or artifact_id <= 0:
        raise ValueError(f"artifact {spec.name} has no positive numeric ID")
    require_equal(artifact.get("name"), spec.name, "artifact name")
    digest = artifact.get("digest")
    if not isinstance(digest, str) or DIGEST.fullmatch(digest) is None:
        raise ValueError(f"artifact {spec.name} has no canonical archive digest")
    if artifact.get("expired") is not False:
        raise ValueError(f"artifact {spec.name} is expired")
    size = artifact.get("size_in_bytes")
    if type(size) is not int or size <= 0:
        raise ValueError(f"artifact {spec.name} has no positive size")
    workflow = artifact.get("workflow_run")
    if not isinstance(workflow, dict):
        raise ValueError(f"artifact {spec.name} has no workflow identity")
    expected_workflow = {
        "id": run_id,
        "repository_id": REPOSITORY_ID,
        "head_repository_id": REPOSITORY_ID,
        "head_sha": source_sha,
    }
    for field, expected in expected_workflow.items():
        require_equal(workflow.get(field), expected, f"artifact {spec.name} {field}")
    return {
        "role": spec.role,
        "platform_key": spec.platform_key,
        "artifact_id": artifact_id,
        "name": spec.name,
        "archive_digest": digest,
        "size_in_bytes": size,
    }


def resolve_candidate_artifacts(request: ResolutionRequest) -> dict[str, Any]:
    repository, run, artifacts, fixture_mode = load_resolution_inputs(request)
    run_proof = verify_run(
        repository, run, request.candidate_run_id, request.source_sha
    )
    names = [artifact.get("name") for artifact in artifacts]
    if any(not isinstance(name, str) for name in names):
        raise ValueError("run artifact names must be strings")
    ids = [artifact.get("id") for artifact in artifacts]
    duplicate_names = sorted(
        name for name, count in Counter(names).items() if count != 1
    )
    duplicate_ids = sorted(item for item, count in Counter(ids).items() if count != 1)
    if duplicate_names or duplicate_ids:
        raise ValueError(
            f"run artifacts contain duplicates: names={duplicate_names}, "
            f"ids={duplicate_ids}"
        )
    specs = expected_specs(request.source_sha)
    if len(artifacts) != len(specs):
        raise ValueError(
            f"candidate run must contain exactly eleven artifacts, got {len(artifacts)}"
        )
    expected_names = {spec.name for spec in specs}
    by_name = {artifact.get("name"): artifact for artifact in artifacts}
    missing = sorted(expected_names - set(by_name))
    unexpected = sorted(set(by_name) - expected_names)
    if missing or unexpected:
        raise ValueError(
            "candidate artifact names differ: "
            f"missing={missing}, unexpected={unexpected}"
        )
    resolved = [
        verify_artifact(
            by_name[spec.name],
            spec,
            request.candidate_run_id,
            request.source_sha,
        )
        for spec in specs
    ]
    if not fixture_mode:
        for item in resolved:
            current = gh_api(
                f"repos/{request.repository}/actions/artifacts/{item['artifact_id']}"
            )
            if not isinstance(current, dict):
                raise ValueError("artifact revalidation response is not an object")
            spec = next(spec for spec in specs if spec.name == item["name"])
            if (
                verify_artifact(
                    current, spec, request.candidate_run_id, request.source_sha
                )
                != item
            ):
                raise ValueError(f"artifact changed after listing: {item['name']}")
    return {
        "schema_version": 1,
        "repository_id": REPOSITORY_ID,
        "candidate_run": run_proof,
        "expected_artifact_count": 11,
        "artifacts": resolved,
    }


def load_candidate_resolution(
    path: Path, run_id: int, source_sha: str
) -> dict[str, Any]:
    value = read_json(path)
    if not isinstance(value, dict):
        raise ValueError("candidate artifact resolution must be an object")
    exact_keys(
        value,
        {
            "schema_version",
            "repository_id",
            "candidate_run",
            "expected_artifact_count",
            "artifacts",
        },
        "candidate artifact resolution",
    )
    if value["schema_version"] != 1 or value["repository_id"] != REPOSITORY_ID:
        raise ValueError("candidate artifact resolution identity changed")
    if value["expected_artifact_count"] != 11:
        raise ValueError("candidate artifact resolution cardinality changed")
    expected_run = {
        "id": run_id,
        "attempt": 1,
        "workflow_id": WORKFLOW_ID,
        "workflow_path": WORKFLOW_PATH,
        "workflow_name": WORKFLOW_NAME,
        "event": "workflow_dispatch",
        "branch": "main",
        "head_sha": source_sha,
        "status": "completed",
        "conclusion": "success",
    }
    require_equal(value.get("candidate_run"), expected_run, "resolved candidate run")
    artifacts = value.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 11:
        raise ValueError("resolution must contain exactly eleven candidate artifacts")
    specs = expected_specs(source_sha)
    for item, spec in zip(artifacts, specs, strict=True):
        if not isinstance(item, dict):
            raise ValueError("resolved artifact must be an object")
        exact_keys(
            item,
            {
                "role",
                "platform_key",
                "artifact_id",
                "name",
                "archive_digest",
                "size_in_bytes",
            },
            "resolved artifact",
        )
        require_equal(item["role"], spec.role, "resolved artifact role")
        require_equal(item["platform_key"], spec.platform_key, "resolved platform")
        require_equal(item["name"], spec.name, "resolved artifact name")
        if type(item["artifact_id"]) is not int or item["artifact_id"] <= 0:
            raise ValueError("resolved artifact ID is invalid")
        if (
            not isinstance(item["archive_digest"], str)
            or DIGEST.fullmatch(item["archive_digest"]) is None
        ):
            raise ValueError("resolved artifact digest is invalid")
        if type(item["size_in_bytes"]) is not int or item["size_in_bytes"] <= 0:
            raise ValueError("resolved artifact size is invalid")
    ids = [item["artifact_id"] for item in artifacts]
    if len(ids) != len(set(ids)):
        raise ValueError("resolved artifact IDs are not unique")
    return value
