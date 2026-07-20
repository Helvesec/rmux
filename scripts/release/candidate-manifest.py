#!/usr/bin/env python3
"""Create or verify one deterministic, non-authoritative candidate manifest."""

from __future__ import annotations
import argparse
import hashlib
import json
import re
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from candidate_manifest_evidence import (
    REPOSITORY_ID,
    SAFE_PATH,
    SHA40,
    SHA64,
    exact_keys,
    positive_integer,
    timestamp,
    validate_intent,
    validate_policy,
    validate_proof,
)

DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
METADATA_KEYS = set(
    "schema_version status repository_id source_git_sha fast_run_id candidate_run_id candidate_run_attempt release_intent_id planned_release_ref release_kind resolution_sha256 fast_evidence source_artifacts canonical_platforms".split()
)
PLATFORM_METADATA_KEYS = set(
    "platform_key assets_artifact provenance_artifact build_record_sha256 attestation_id attestation_bundle_sha256 runner toolchain files".split()
)
RECORD_KEYS = set(
    "schema_version repository_id source_git_sha fast_run_id candidate_run_id candidate_run_attempt release_intent_id planned_release_ref release_kind platform runner toolchain build_policy created_at files".split()
)
TOOLCHAIN_KEYS = set("requested release host commit_hash rustc_verbose_sha256".split())
BINDING_KEYS = set(
    "schema_version repository_id source_git_sha candidate_run_id platform_key assets build_record_sha256 attestation".split()
)
PLATFORMS: dict[str, dict[str, Any]] = {
    "linux-x86_64": {
        "target": "x86_64-unknown-linux-gnu",
        "runner": {"image": "ubuntu-22.04", "os": "Linux", "arch": "X64"},
        "roles": {"archive", "checksums", "debian", "rpm"},
    },
    "linux-aarch64": {
        "target": "aarch64-unknown-linux-gnu",
        "runner": {
            "image": "ubuntu-22.04-arm",
            "os": "Linux",
            "arch": "ARM64",
        },
        "roles": {"archive", "checksums", "debian", "rpm"},
    },
    "macos-x86_64": {
        "target": "x86_64-apple-darwin",
        "runner": {"image": "macos-15-intel", "os": "macOS", "arch": "X64"},
        "roles": {"archive", "checksums"},
    },
    "macos-aarch64": {
        "target": "aarch64-apple-darwin",
        "runner": {"image": "macos-15", "os": "macOS", "arch": "ARM64"},
        "roles": {"archive", "checksums"},
    },
    "windows-x86_64": {
        "target": "x86_64-pc-windows-msvc",
        "runner": {"image": "windows-latest", "os": "Windows", "arch": "X64"},
        "roles": {"archive", "checksums"},
    },
}
BUILD_POLICY = {
    "cargo_incremental": False,
    "cargo_locked": True,
    "fresh_target": True,
    "object_cache_restored": False,
    "publication_authority": False,
}


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON: {path}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def render_timestamp(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def mapped_paths(values: list[str] | None, label: str) -> dict[str, Path]:
    if values is None:
        raise ValueError(f"five {label} arguments are required")
    result: dict[str, Path] = {}
    for value in values:
        if "=" not in value:
            raise ValueError(f"{label} must use PLATFORM=PATH")
        platform, raw_path = value.split("=", 1)
        if platform not in PLATFORMS or platform in result or not raw_path:
            raise ValueError(f"duplicate or invalid {label} platform: {platform}")
        result[platform] = Path(raw_path)
    if set(result) != set(PLATFORMS):
        raise ValueError(f"{label} platform set must contain exactly five platforms")
    return result


def validate_metadata(
    metadata: dict[str, Any], identity: dict[str, Any], fast_proof: dict[str, Any]
) -> dict[str, dict[str, Any]]:
    exact_keys(metadata, METADATA_KEYS, "canonical artifact metadata")
    if (
        metadata["schema_version"] != 1
        or metadata["status"] != "verified-for-shadow-sealing"
        or metadata["repository_id"] != REPOSITORY_ID
        or metadata["candidate_run_attempt"] != 1
    ):
        raise ValueError("canonical artifact metadata identity changed")
    for field, expected in identity.items():
        if metadata.get(field) != expected:
            raise ValueError(f"canonical artifact metadata {field} mismatch")
    if not isinstance(metadata["resolution_sha256"], str) or not SHA64.fullmatch(
        metadata["resolution_sha256"]
    ):
        raise ValueError("canonical artifact resolution digest is invalid")
    fast_evidence = metadata["fast_evidence"]
    if not isinstance(fast_evidence, dict):
        raise ValueError("canonical fast evidence is missing")
    exact_keys(
        fast_evidence, {"proof_sha256", "nextest_artifact"}, "canonical fast evidence"
    )
    if fast_evidence["proof_sha256"] != fast_proof["proof_sha256"]:
        raise ValueError("canonical metadata selected a different fast proof")
    values = metadata["canonical_platforms"]
    if not isinstance(values, list) or len(values) != len(PLATFORMS):
        raise ValueError("canonical metadata must contain exactly five platforms")
    by_platform: dict[str, dict[str, Any]] = {}
    for value in values:
        if not isinstance(value, dict):
            raise ValueError("canonical metadata platform must be an object")
        exact_keys(value, PLATFORM_METADATA_KEYS, "metadata platform")
        platform = value["platform_key"]
        if platform not in PLATFORMS or platform in by_platform:
            raise ValueError("canonical metadata platform set is invalid")
        by_platform[platform] = value
    if set(by_platform) != set(PLATFORMS):
        raise ValueError("canonical metadata platform set changed")
    source_artifacts = metadata["source_artifacts"]
    if not isinstance(source_artifacts, list) or len(source_artifacts) != 11:
        raise ValueError("canonical metadata must bind eleven source artifacts")
    expected_artifacts = [
        ("fast-proof", None, f"rmux-fast-proof-{identity['source_git_sha']}")
    ]
    expected_artifacts.extend(
        ("canonical-assets", key, f"rmux-canonical-{key}-{identity['source_git_sha']}")
        for key in PLATFORMS
    )
    expected_artifacts.extend(
        (
            "canonical-provenance",
            key,
            f"rmux-canonical-provenance-{key}-{identity['source_git_sha']}",
        )
        for key in PLATFORMS
    )
    ids: set[int] = set()
    for artifact, (role, platform, name) in zip(
        source_artifacts, expected_artifacts, strict=True
    ):
        if not isinstance(artifact, dict):
            raise ValueError("source artifact metadata must be an object")
        exact_keys(
            artifact,
            {
                "role",
                "platform_key",
                "artifact_id",
                "name",
                "archive_digest",
                "size_in_bytes",
            },
            "source artifact",
        )
        if (artifact["role"], artifact["platform_key"], artifact["name"]) != (
            role,
            platform,
            name,
        ):
            raise ValueError("source artifact identity or ordering changed")
        artifact_id = positive_integer(artifact["artifact_id"], "source artifact ID")
        if artifact_id in ids:
            raise ValueError("source artifact ID is duplicated")
        ids.add(artifact_id)
        if not isinstance(artifact["archive_digest"], str) or not DIGEST.fullmatch(
            artifact["archive_digest"]
        ):
            raise ValueError("source artifact digest is invalid")
        positive_integer(artifact["size_in_bytes"], "source artifact size")
    sources = {(item["role"], item["platform_key"]): item for item in source_artifacts}
    for platform, value in by_platform.items():
        if value["assets_artifact"] != sources[("canonical-assets", platform)]:
            raise ValueError(f"{platform} asset metadata differs from its source")
        if value["provenance_artifact"] != sources[("canonical-provenance", platform)]:
            raise ValueError(f"{platform} provenance metadata differs from its source")
    return by_platform


def validate_files(files: Any, platform_key: str) -> list[dict[str, Any]]:
    if not isinstance(files, list) or not files:
        raise ValueError("canonical build record files must be non-empty")
    roles: list[str] = []
    paths: list[str] = []
    for item in files:
        if not isinstance(item, dict):
            raise ValueError("canonical build record file must be an object")
        exact_keys(item, {"path", "role", "size", "sha256"}, "canonical file")
        path = item["path"]
        if not isinstance(path, str) or SAFE_PATH.fullmatch(path) is None:
            raise ValueError("canonical file path is invalid")
        positive_integer(item["size"], "canonical file size")
        if not isinstance(item["sha256"], str) or not SHA64.fullmatch(item["sha256"]):
            raise ValueError("canonical file digest is invalid")
        roles.append(item["role"])
        paths.append(path)
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise ValueError("canonical files must be sorted and unique")
    if set(roles) != PLATFORMS[platform_key]["roles"] or len(roles) != len(set(roles)):
        raise ValueError("canonical file role set changed")
    return files


def canonical_entry(
    platform_key: str,
    record_path: Path,
    binding_path: Path,
    metadata: dict[str, Any],
    identity: dict[str, Any],
    candidate_started: datetime,
    candidate_verified: datetime,
) -> dict[str, Any]:
    record = read_object(record_path, f"{platform_key} build record")
    binding = read_object(binding_path, f"{platform_key} artifact binding")
    exact_keys(record, RECORD_KEYS, f"{platform_key} build record")
    expected_fields = {
        "schema_version": 1,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": identity["source_git_sha"],
        "fast_run_id": identity["fast_run_id"],
        "candidate_run_id": identity["candidate_run_id"],
        "candidate_run_attempt": 1,
        "release_intent_id": identity["release_intent_id"],
        "planned_release_ref": identity["planned_release_ref"],
        "release_kind": identity["release_kind"],
        "build_policy": BUILD_POLICY,
    }
    for field, expected in expected_fields.items():
        if record[field] != expected:
            raise ValueError(f"{platform_key} build record {field} mismatch")
    platform = PLATFORMS[platform_key]
    platform_record = record["platform"]
    if (
        not isinstance(platform_record, dict)
        or platform_record.get("key") != platform_key
    ):
        raise ValueError(f"{platform_key} platform record mismatch")
    if platform_record.get("target_triple") != platform["target"]:
        raise ValueError(f"{platform_key} target triple mismatch")
    expected_runner = {**platform["runner"], "environment": "github-hosted"}
    if record["runner"] != expected_runner:
        raise ValueError(f"{platform_key} runner identity mismatch")
    toolchain = record["toolchain"]
    if not isinstance(toolchain, dict):
        raise ValueError(f"{platform_key} toolchain record is missing")
    exact_keys(toolchain, TOOLCHAIN_KEYS, f"{platform_key} toolchain")
    if (
        toolchain["requested"] != "1.96.1"
        or toolchain["release"] != "1.96.1"
        or toolchain["host"] != platform["target"]
    ):
        raise ValueError(f"{platform_key} toolchain identity mismatch")
    if not isinstance(toolchain["commit_hash"], str) or not SHA40.fullmatch(
        toolchain["commit_hash"]
    ):
        raise ValueError(f"{platform_key} rustc commit hash is invalid")
    if not isinstance(toolchain["rustc_verbose_sha256"], str) or not SHA64.fullmatch(
        toolchain["rustc_verbose_sha256"]
    ):
        raise ValueError(f"{platform_key} rustc evidence digest is invalid")
    created = timestamp(record["created_at"], f"{platform_key} created_at")
    if not candidate_started <= created <= candidate_verified:
        raise ValueError(
            f"{platform_key} build timestamp lies outside the candidate run"
        )
    files = validate_files(record["files"], platform_key)
    record_sha = sha256_file(record_path)
    for field, expected in (
        ("build_record_sha256", record_sha),
        ("runner", record["runner"]),
        ("toolchain", toolchain),
        ("files", files),
    ):
        if metadata[field] != expected:
            raise ValueError(f"{platform_key} verified metadata {field} mismatch")

    exact_keys(binding, BINDING_KEYS, f"{platform_key} binding")
    if binding["schema_version"] != 1 or binding["repository_id"] != REPOSITORY_ID:
        raise ValueError(f"{platform_key} binding identity changed")
    if (
        binding["source_git_sha"] != identity["source_git_sha"]
        or binding["candidate_run_id"] != identity["candidate_run_id"]
        or binding["platform_key"] != platform_key
    ):
        raise ValueError(f"{platform_key} binding run identity mismatch")
    if binding["build_record_sha256"] != record_sha:
        raise ValueError(f"{platform_key} binding does not cover the build record")
    assets = metadata["assets_artifact"]
    expected_assets = {
        "artifact_id": assets["artifact_id"],
        "artifact_name": assets["name"],
        "artifact_digest": assets["archive_digest"],
    }
    if binding["assets"] != expected_assets:
        raise ValueError(
            f"{platform_key} binding does not cover the exact asset artifact"
        )
    attestation = binding["attestation"]
    if not isinstance(attestation, dict):
        raise ValueError(f"{platform_key} attestation binding is missing")
    exact_keys(
        attestation,
        {"attestation_id", "bundle_file", "bundle_sha256"},
        f"{platform_key} attestation",
    )
    if (
        not isinstance(attestation["attestation_id"], str)
        or not 1 <= len(attestation["attestation_id"]) <= 256
    ):
        raise ValueError(f"{platform_key} attestation ID is invalid")
    if (
        attestation["bundle_file"] != "build-provenance.sigstore.json"
        or not isinstance(attestation["bundle_sha256"], str)
        or not SHA64.fullmatch(attestation["bundle_sha256"])
    ):
        raise ValueError(f"{platform_key} attestation bundle identity is invalid")
    if (
        metadata["attestation_id"] != attestation["attestation_id"]
        or metadata["attestation_bundle_sha256"] != attestation["bundle_sha256"]
    ):
        raise ValueError(f"{platform_key} verified attestation metadata mismatch")
    return {
        "platform_key": platform_key,
        "target_triple": platform["target"],
        "runner_image": platform["runner"]["image"],
        "rust_toolchain": toolchain["release"],
        "assets": assets,
        "provenance": metadata["provenance_artifact"],
        "build_record_sha256": record_sha,
        "binding_sha256": sha256_file(binding_path),
        "attestation": attestation,
        "files": files,
    }


def build_manifest(args: argparse.Namespace) -> dict[str, Any]:
    candidate_path = args.candidate_proof.resolve(strict=True)
    fast_path = args.fast_proof.resolve(strict=True)
    intent_path = args.candidate_intent.resolve(strict=True)
    policy_path = args.policy_root.resolve(strict=True)
    metadata_path = args.canonical_artifacts.resolve(strict=True)
    candidate = read_object(candidate_path, "candidate proof")
    fast = read_object(fast_path, "fast proof")
    intent = read_object(intent_path, "candidate intent")
    policy = read_object(policy_path, "policy root")
    metadata = read_object(metadata_path, "canonical artifact metadata")
    candidate_started, candidate_verified = validate_proof(
        candidate, "candidate", "candidate proof"
    )
    fast_started, fast_verified = validate_proof(fast, "fast", "fast proof")
    validate_intent(intent)
    source_sha = candidate["source_git_sha"]
    identity = {
        "source_git_sha": source_sha,
        "fast_run_id": fast["run_id"],
        "candidate_run_id": candidate["run_id"],
        "release_intent_id": intent["release_intent_id"],
        "planned_release_ref": intent["planned_release_ref"],
        "release_kind": intent["release_kind"],
    }
    if fast["source_git_sha"] != source_sha or intent["source_git_sha"] != source_sha:
        raise ValueError("candidate, fast proof, and intent source SHA differ")
    if intent["fast_run_id"] != fast["run_id"]:
        raise ValueError("candidate intent selected a different fast run")
    if not fast_started <= candidate_started <= fast_verified <= candidate_verified:
        raise ValueError("fast and candidate proof timestamps are not ordered")
    expires = fast_started + timedelta(hours=48)
    if candidate_verified > expires:
        raise ValueError("candidate proof was produced after fast evidence expiry")
    contract_record = validate_policy(policy, source_sha)
    for proof, label in ((fast, "fast proof"), (candidate, "candidate proof")):
        if (
            proof["contract_sha256"] != contract_record["sha256"]
            or proof["contract_blob_oid"] != contract_record["blob_oid"]
        ):
            raise ValueError(f"{label} is not bound to the release policy contract")
    records = mapped_paths(args.build_record, "build record")
    bindings = mapped_paths(args.artifact_binding, "artifact binding")
    metadata_by_platform = validate_metadata(metadata, identity, fast)
    artifacts = [
        canonical_entry(
            platform,
            records[platform].resolve(strict=True),
            bindings[platform].resolve(strict=True),
            metadata_by_platform[platform],
            identity,
            candidate_started,
            candidate_verified,
        )
        for platform in sorted(PLATFORMS)
    ]
    jobs = [
        {"id": job["id"], "name": job["name"], "conclusion": job["conclusion"]}
        for job in candidate["jobs"]
    ]
    return {
        "schema_version": 1,
        "repository_id": REPOSITORY_ID,
        **identity,
        "candidate_run_attempt": 1,
        "release_version": intent["release_version"],
        "package_version": intent["package_version"],
        "is_prerelease": intent["is_prerelease"],
        "producer": {
            "workflow_id": 277622540,
            "workflow_path": ".github/workflows/ci.yml",
            "workflow_name": "CI",
            "event": "workflow_dispatch",
            "branch": "main",
        },
        "proofs": {
            "fast_semantic_sha256": fast["proof_sha256"],
            "fast_document_sha256": sha256_file(fast_path),
            "candidate_semantic_sha256": candidate["proof_sha256"],
            "candidate_document_sha256": sha256_file(candidate_path),
            "intent_document_sha256": sha256_file(intent_path),
            "policy_document_sha256": sha256_file(policy_path),
            "canonical_metadata_document_sha256": sha256_file(metadata_path),
        },
        "canonical_build_policy": BUILD_POLICY,
        "release_policy": {
            "sha256": policy["release_policy_sha256"],
            "contract_blob_oid": policy["contract_blob_oid"],
            "record_count": len(policy["records"]),
        },
        "created_at": render_timestamp(candidate_verified),
        "expires_at": render_timestamp(expires),
        "jobs": jobs,
        "artifacts": artifacts,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("create", "verify"):
        command = commands.add_parser(name)
        command.add_argument("--candidate-proof", type=Path, required=True)
        command.add_argument("--fast-proof", type=Path, required=True)
        command.add_argument("--candidate-intent", type=Path, required=True)
        command.add_argument("--policy-root", type=Path, required=True)
        command.add_argument("--canonical-artifacts", type=Path, required=True)
        command.add_argument("--build-record", action="append")
        command.add_argument("--artifact-binding", action="append")
        if name == "create":
            command.add_argument("--output", type=Path, required=True)
        else:
            command.add_argument("--manifest", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    expected = build_manifest(args)
    if args.command == "create":
        args.output.write_text(
            json.dumps(expected, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(sha256_file(args.output))
    else:
        actual = read_object(args.manifest, "candidate manifest")
        if actual != expected:
            raise ValueError("candidate manifest differs from its exact evidence set")
        print(sha256_file(args.manifest))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
