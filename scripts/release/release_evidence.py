#!/usr/bin/env python3
"""Shared fail-closed primitives for disarmed release evidence records."""

from __future__ import annotations

import hashlib
import json
import re
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

REPOSITORY_ID = 1239918790
REPOSITORY = "Helvesec/rmux"
POLICY_AUDIT_APP_ID = 4344532
POLICY_AUDIT_INSTALLATION_ID = 147749910
PROMOTION_WORKFLOW_ID = 316435346
PROMOTION_WORKFLOW_PATH = ".github/workflows/release-promote.yml"
SIMULATION_WORKFLOW_ID = 316591947
SIMULATION_WORKFLOW_PATH = ".github/workflows/release-promotion-simulation.yml"
STATUS = "disarmed-non-authoritative"
SHA40 = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
INTENT = re.compile(r"[A-Za-z0-9._:-]{8,128}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
SAFE_NAME = re.compile(r"[A-Za-z0-9._+@=-]+")
FINGERPRINT = re.compile(r"SHA256:[A-Za-z0-9+/]{43}")
PLATFORMS = {
    "linux-aarch64",
    "linux-x86_64",
    "macos-aarch64",
    "macos-x86_64",
    "windows-x86_64",
}
PLATFORM_ROLES = {
    "linux-x86_64": {
        "archive",
        "checksums",
        "crate-package-set",
        "debian",
        "rpm",
        "snap-amd64",
        "wasm-byte-set",
        "wasm-provenance",
    },
    "linux-aarch64": {"archive", "checksums", "debian", "rpm", "snap-arm64"},
    "macos-x86_64": {"archive", "checksums"},
    "macos-aarch64": {"archive", "checksums"},
    "windows-x86_64": {"archive", "checksums", "chocolatey-package"},
}
PUBLIC_ASSET_ROLES = {"archive", "debian", "rpm", "snap-amd64", "snap-arm64"}


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON: {path}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def write_object(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValueError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def positive_integer(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def require_match(value: Any, pattern: re.Pattern[str], label: str) -> str:
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
    if render_timestamp(parsed) != value:
        raise ValueError(f"{label} is not canonically encoded")
    return parsed


def render_timestamp(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def validate_file(path: Path, label: str) -> Path:
    if path.is_symlink():
        raise ValueError(f"{label} must be one non-empty regular file")
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or resolved.stat().st_size <= 0:
        raise ValueError(f"{label} must be one non-empty regular file")
    return resolved


def validate_candidate_reference(
    reference: dict[str, Any], manifest_path: Path, manifest: dict[str, Any]
) -> dict[str, Any]:
    keys = {
        "schema_version",
        "status",
        "repository_id",
        "source_git_sha",
        "candidate_run_id",
        "candidate_run_attempt",
        "manifest_run_id",
        "manifest_run_attempt",
        "manifest_workflow_id",
        "manifest_workflow_path",
        "manifest_artifact_id",
        "manifest_artifact_digest",
        "manifest_sha256",
    }
    exact_keys(reference, keys, "candidate reference")
    if (
        reference["schema_version"] != 1
        or reference["status"] != "shadow-non-authoritative"
        or reference["repository_id"] != REPOSITORY_ID
        or reference["candidate_run_attempt"] != 1
        or reference["manifest_run_attempt"] != 1
        or reference["manifest_workflow_id"] != 316223904
        or reference["manifest_workflow_path"] != ".github/workflows/release-shadow.yml"
    ):
        raise ValueError("candidate reference identity changed")
    for field in (
        "candidate_run_id",
        "manifest_run_id",
        "manifest_workflow_id",
        "manifest_artifact_id",
    ):
        positive_integer(reference[field], f"candidate reference {field}")
    require_match(reference["source_git_sha"], SHA40, "candidate source SHA")
    require_match(
        reference["manifest_artifact_digest"], DIGEST, "candidate artifact digest"
    )
    require_match(reference["manifest_sha256"], SHA256, "candidate manifest digest")
    if (
        file_hash(validate_file(manifest_path, "candidate manifest"))
        != reference["manifest_sha256"]
    ):
        raise ValueError("candidate manifest file digest changed")
    if any(
        reference[field] != manifest[manifest_field]
        for field, manifest_field in (
            ("repository_id", "repository_id"),
            ("source_git_sha", "source_git_sha"),
            ("candidate_run_id", "candidate_run_id"),
            ("candidate_run_attempt", "candidate_run_attempt"),
        )
    ):
        raise ValueError("candidate reference does not bind the exact manifest")
    return reference


def validate_candidate_manifest(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    required = {
        "schema_version",
        "repository_id",
        "source_git_sha",
        "candidate_run_id",
        "candidate_run_attempt",
        "release_intent_id",
        "planned_release_ref",
        "release_kind",
        "release_version",
        "is_prerelease",
        "release_policy",
        "created_at",
        "expires_at",
        "artifacts",
    }
    if not required.issubset(manifest):
        raise ValueError("candidate manifest is missing promotion identity fields")
    if (
        manifest["schema_version"] != 1
        or manifest["repository_id"] != REPOSITORY_ID
        or manifest["candidate_run_attempt"] != 1
        or manifest["release_kind"] not in {"rc", "stable"}
    ):
        raise ValueError(
            "candidate manifest is not an RC or stable attempt-1 candidate"
        )
    require_match(manifest["source_git_sha"], SHA40, "candidate source SHA")
    positive_integer(manifest["candidate_run_id"], "candidate run ID")
    require_match(manifest["release_intent_id"], INTENT, "release intent ID")
    release_ref = require_match(
        manifest["planned_release_ref"], RELEASE_REF, "planned release ref"
    )
    if manifest["release_version"] != release_ref[1:]:
        raise ValueError("candidate release version differs from its ref")
    is_rc = "-rc." in release_ref
    if manifest["is_prerelease"] is not is_rc:
        raise ValueError("candidate prerelease state differs from its ref")
    if (manifest["release_kind"] == "rc") is not is_rc:
        raise ValueError("candidate release kind differs from its ref")
    created = timestamp(manifest["created_at"], "candidate created_at")
    expires = timestamp(manifest["expires_at"], "candidate expires_at")
    if expires <= created:
        raise ValueError("candidate expiry does not follow creation")
    policy = manifest["release_policy"]
    if not isinstance(policy, dict):
        raise ValueError("candidate release policy must be an object")
    require_match(policy.get("sha256"), SHA256, "candidate release policy root")
    artifacts = manifest["artifacts"]
    if not isinstance(artifacts, list) or len(artifacts) != 5:
        raise ValueError("candidate must contain exactly five platform artifacts")
    platform_keys = [
        item.get("platform_key") for item in artifacts if isinstance(item, dict)
    ]
    if set(platform_keys) != PLATFORMS or len(platform_keys) != len(set(platform_keys)):
        raise ValueError("candidate platform cardinality changed")
    return artifacts


def publishable_assets(
    manifest: dict[str, Any], sha256sums: Path
) -> list[dict[str, Any]]:
    artifacts = validate_candidate_manifest(manifest)
    assets: list[dict[str, Any]] = []
    names: set[str] = set()
    for artifact in artifacts:
        platform = artifact["platform_key"]
        files = artifact.get("files")
        if not isinstance(files, list) or not files:
            raise ValueError(f"candidate {platform} files are missing")
        expected_roles = PLATFORM_ROLES[platform]
        actual_roles = [item.get("role") for item in files if isinstance(item, dict)]
        if set(actual_roles) != expected_roles or len(actual_roles) != len(
            expected_roles
        ):
            raise ValueError(f"candidate {platform} asset roles changed")
        for item in files:
            if not isinstance(item, dict):
                raise ValueError("candidate asset must be an object")
            exact_keys(item, {"path", "role", "size", "sha256"}, "candidate asset")
            if item["role"] not in PUBLIC_ASSET_ROLES:
                continue
            name = require_match(item["path"], SAFE_NAME, "candidate public asset name")
            if name in names:
                raise ValueError(f"candidate public asset name is duplicated: {name}")
            names.add(name)
            size = positive_integer(item["size"], f"candidate asset {name} size")
            digest = require_match(
                item["sha256"], SHA256, f"candidate asset {name} digest"
            )
            assets.append(
                {
                    "name": name,
                    "platform_key": platform,
                    "role": item["role"],
                    "size": size,
                    "sha256": digest,
                }
            )
    assets.sort(key=lambda item: item["name"])
    sums_path = validate_file(sha256sums, "global SHA256SUMS")
    try:
        lines = sums_path.read_text(encoding="ascii").splitlines()
    except UnicodeDecodeError as error:
        raise ValueError("global SHA256SUMS must be ASCII") from error
    expected_lines = [f"{item['sha256']}  {item['name']}" for item in assets]
    if lines != expected_lines:
        raise ValueError(
            "global SHA256SUMS does not cover the exact candidate asset set"
        )
    assets.append(
        {
            "name": "SHA256SUMS",
            "platform_key": None,
            "role": "checksums",
            "size": sums_path.stat().st_size,
            "sha256": file_hash(sums_path),
        }
    )
    return sorted(assets, key=lambda item: item["name"])


def validate_signed_tag(
    tag: dict[str, Any],
    *,
    release_ref: str,
    source_git_sha: str,
    release_intent_id: str,
    release_kind: str,
    candidate: dict[str, Any],
    release_policy_sha256: str,
) -> dict[str, Any]:
    exact_keys(
        tag,
        {
            "schema_version",
            "status",
            "repository_id",
            "release_ref",
            "release_intent_id",
            "release_kind",
            "tag_object_sha",
            "target_git_sha",
            "candidate_run_id",
            "candidate_manifest_artifact_id",
            "candidate_manifest_artifact_digest",
            "candidate_manifest_sha256",
            "release_policy_root_sha256",
            "object_type",
            "annotated",
            "signature",
            "verified_at",
        },
        "signed tag proof",
    )
    if (
        tag["schema_version"] != 1
        or tag["status"] != "verified-signed-annotated-tag"
        or tag["repository_id"] != REPOSITORY_ID
        or tag["release_ref"] != release_ref
        or tag["release_intent_id"] != release_intent_id
        or tag["release_kind"] != release_kind
        or tag["target_git_sha"] != source_git_sha
        or tag["candidate_run_id"] != candidate["candidate_run_id"]
        or tag["candidate_manifest_artifact_id"] != candidate["manifest_artifact_id"]
        or tag["candidate_manifest_artifact_digest"]
        != candidate["manifest_artifact_digest"]
        or tag["candidate_manifest_sha256"] != candidate["manifest_sha256"]
        or tag["release_policy_root_sha256"] != release_policy_sha256
        or tag["object_type"] != "tag"
        or tag["annotated"] is not True
    ):
        raise ValueError("tag proof does not bind the signed annotated release tag")
    require_match(tag["tag_object_sha"], SHA40, "tag object SHA")
    require_match(tag["target_git_sha"], SHA40, "tag target SHA")
    positive_integer(tag["candidate_run_id"], "tag candidate run ID")
    positive_integer(tag["candidate_manifest_artifact_id"], "tag candidate artifact ID")
    require_match(
        tag["candidate_manifest_artifact_digest"],
        DIGEST,
        "tag candidate artifact digest",
    )
    require_match(
        tag["candidate_manifest_sha256"], SHA256, "tag candidate manifest digest"
    )
    require_match(tag["release_policy_root_sha256"], SHA256, "tag release policy root")
    signature = tag["signature"]
    if not isinstance(signature, dict):
        raise ValueError("tag signature proof must be an object")
    exact_keys(
        signature,
        {"verified", "format", "key_fingerprint", "signing_principal"},
        "tag signature proof",
    )
    if signature["verified"] is not True or signature["format"] != "ssh":
        raise ValueError("release tag requires one verified SSH signature")
    require_match(signature["key_fingerprint"], FINGERPRINT, "tag signing fingerprint")
    principal = signature["signing_principal"]
    if not isinstance(principal, str) or not (3 <= len(principal) <= 254):
        raise ValueError("tag signing principal is invalid")
    timestamp(tag["verified_at"], "tag verified_at")
    return tag


def validate_policy_audit(
    reference: dict[str, Any],
    manifest: dict[str, Any],
    *,
    workflow_id: int = PROMOTION_WORKFLOW_ID,
    workflow_path: str = PROMOTION_WORKFLOW_PATH,
) -> dict[str, Any]:
    keys = {
        "schema_version",
        "status",
        "repository_id",
        "source_git_sha",
        "candidate_run_id",
        "release_intent_id",
        "policy_audit_run_id",
        "policy_audit_run_attempt",
        "predicate_artifact_id",
        "predicate_artifact_digest",
        "predicate_sha256",
        "emitted_at",
        "expires_at",
        "app_id",
        "installation_id",
        "workflow_id",
        "workflow_path",
        "release_policy_sha256",
    }
    exact_keys(reference, keys, "policy audit reference")
    if (
        reference["schema_version"] != 1
        or reference["status"] != "shadow-non-authoritative"
        or reference["repository_id"] != REPOSITORY_ID
        or reference["source_git_sha"] != manifest["source_git_sha"]
        or reference["candidate_run_id"] != manifest["candidate_run_id"]
        or reference["release_intent_id"] != manifest["release_intent_id"]
        or reference["policy_audit_run_attempt"] != 1
        or reference["app_id"] != POLICY_AUDIT_APP_ID
        or reference["installation_id"] != POLICY_AUDIT_INSTALLATION_ID
        or reference["workflow_id"] != workflow_id
        or reference["workflow_path"] != workflow_path
        or reference["release_policy_sha256"] != manifest["release_policy"]["sha256"]
    ):
        raise ValueError("policy audit reference does not bind the exact candidate")
    for field in (
        "policy_audit_run_id",
        "predicate_artifact_id",
        "app_id",
        "installation_id",
        "workflow_id",
    ):
        positive_integer(reference[field], f"policy audit {field}")
    require_match(
        reference["predicate_artifact_digest"], DIGEST, "policy audit artifact digest"
    )
    require_match(
        reference["predicate_sha256"], SHA256, "policy audit predicate digest"
    )
    emitted = timestamp(reference["emitted_at"], "policy audit emitted_at")
    expires = timestamp(reference["expires_at"], "policy audit expires_at")
    if expires <= emitted or expires - emitted > timedelta(minutes=15):
        raise ValueError(
            "policy audit TTL must be positive and at most fifteen minutes"
        )
    return reference


def validate_artifact_reference(value: dict[str, Any], label: str) -> dict[str, Any]:
    exact_keys(value, {"artifact_id", "name", "archive_digest", "size_in_bytes"}, label)
    positive_integer(value["artifact_id"], f"{label} ID")
    require_match(value["name"], SAFE_NAME, f"{label} name")
    require_match(value["archive_digest"], DIGEST, f"{label} API digest")
    positive_integer(value["size_in_bytes"], f"{label} size")
    return value
