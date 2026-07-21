#!/usr/bin/env python3
"""Create and verify exact, non-authoritative downstream payload evidence."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    DIGEST,
    REPOSITORY_ID,
    RUNNER_IMAGES,
    SAFE_NAME,
    SHA256,
    STATUS,
    canonical_hash,
    contract_channels,
    exact_keys,
    file_hash,
    match,
    positive,
    read_object,
    timestamp,
    validate_receipt_reference,
    validate_release,
    write_object,
)

PREDICATE_TYPE = "https://rmux.io/attestations/release-downstream-channel-payload/v1"
SUBJECT_NAME = "downstream-channel-payload-subject.json"
PRODUCER_WORKFLOW_ID = 316435347
PRODUCER_WORKFLOW_PATH = ".github/workflows/release-receipt.yml"
PRODUCER_JOB_WORKFLOW_PATH = ".github/workflows/release-downstream.yml"


def payload_contract() -> dict[str, Any]:
    from downstream_channels import load_contract

    value = load_contract().get("payload_evidence")
    if not isinstance(value, dict):
        raise ValueError("downstream payload evidence contract is missing")
    expected = {
        "schema": ".github/release/schemas/downstream-channel-payload.schema.json",
        "predicate_type": PREDICATE_TYPE,
        "subject_name": SUBJECT_NAME,
        "canonical_provenance_ready": True,
        "actions_expiry_bound": True,
        "producer_workflow_allowlist_ready": True,
        "producer": {
            "workflow_id": PRODUCER_WORKFLOW_ID,
            "workflow_path": PRODUCER_WORKFLOW_PATH,
            "job_workflow_path": PRODUCER_JOB_WORKFLOW_PATH,
            "required_run_attempt": 1,
        },
        "activation_blockers": [],
    }
    if value != expected:
        raise ValueError("downstream payload evidence contract differs")
    return value


def validate_producer(value: Any, channel: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("payload producer must be an object")
    exact_keys(
        value,
        {
            "run_id",
            "run_attempt",
            "workflow_id",
            "workflow_path",
            "job_workflow_path",
            "runner_group_id",
            "runner_group_name",
            "runner_image",
        },
        "payload producer",
    )
    positive(value["run_id"], "payload producer run ID")
    expected_image = "windows-latest" if channel == "chocolatey" else "ubuntu-22.04"
    if (
        type(value["run_attempt"]) is not int
        or value["run_attempt"] != 1
        or value["workflow_id"] != PRODUCER_WORKFLOW_ID
        or value["workflow_path"] != PRODUCER_WORKFLOW_PATH
        or value["job_workflow_path"] != PRODUCER_JOB_WORKFLOW_PATH
        or type(value["runner_group_id"]) is not int
        or value["runner_group_id"] != 0
        or value["runner_group_name"] != "GitHub Actions"
        or value["runner_image"] not in RUNNER_IMAGES
        or value["runner_image"] != expected_image
    ):
        raise ValueError("payload producer is not the allowlisted GitHub-hosted job")
    return value


def validate_artifact(
    value: Any, *, channel: str, source_sha: str, release_id: int
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("payload artifact must be an object")
    exact_keys(
        value,
        {
            "artifact_id",
            "name",
            "archive_digest",
            "size_in_bytes",
            "created_at",
            "updated_at",
            "expires_at",
        },
        "payload artifact",
    )
    positive(value["artifact_id"], "payload artifact ID")
    positive(value["size_in_bytes"], "payload artifact size")
    expected_name = f"rmux-downstream-{channel}-payload-{source_sha}-{release_id}"
    if value["name"] != expected_name:
        raise ValueError("payload artifact name does not bind its release")
    match(value["archive_digest"], DIGEST, "payload artifact digest")
    created = timestamp(value["created_at"], "payload artifact created_at")
    updated = timestamp(value["updated_at"], "payload artifact updated_at")
    expires = timestamp(value["expires_at"], "payload artifact expires_at")
    if updated < created or expires <= updated:
        raise ValueError("payload artifact timestamps are not ordered")
    return value


def validate_files(value: Any, channel: str) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value:
        raise ValueError("payload files must be a non-empty array")
    names: list[str] = []
    roles: list[str] = []
    for item in value:
        if not isinstance(item, dict):
            raise ValueError("payload file must be an object")
        exact_keys(item, {"name", "role", "size", "sha256"}, "payload file")
        names.append(match(item["name"], SAFE_NAME, "payload file name"))
        roles.append(item["role"])
        positive(item["size"], "payload file size")
        match(item["sha256"], SHA256, "payload file digest")
    if names != sorted(names) or len(names) != len(set(names)):
        raise ValueError("payload files must be sorted and unique")
    expected_roles = sorted(contract_channels()[channel]["payload_roles"])
    if sorted(set(roles)) != expected_roles:
        raise ValueError("payload file roles differ from the channel contract")
    return value


def validate_source_evidence(
    value: Any,
    *,
    receipt_predicate_sha256: str | None,
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("payload source evidence must be an object")
    exact_keys(
        value,
        {
            "receipt_predicate_sha256",
            "candidate_manifest_sha256",
            "candidate_manifest_artifact_digest",
            "candidate_manifest_expires_at",
            "release_asset_set_sha256",
            "sha256sums_sha256",
        },
        "payload source evidence",
    )
    for field in (
        "receipt_predicate_sha256",
        "candidate_manifest_sha256",
        "release_asset_set_sha256",
        "sha256sums_sha256",
    ):
        match(value[field], SHA256, f"payload source {field}")
    match(
        value["candidate_manifest_artifact_digest"],
        DIGEST,
        "payload candidate manifest artifact digest",
    )
    timestamp(
        value["candidate_manifest_expires_at"],
        "payload candidate manifest expires_at",
    )
    if (
        receipt_predicate_sha256 is not None
        and value["receipt_predicate_sha256"] != receipt_predicate_sha256
    ):
        raise ValueError("payload source differs from the exact publication receipt")
    return value


def validate_payload_document(
    value: dict[str, Any],
    *,
    channel: str,
    source_sha: str,
    receipt_predicate_sha256: str | None = None,
    release: dict[str, Any] | None = None,
) -> dict[str, Any]:
    payload_contract()
    exact_keys(
        value,
        {
            "schema_version",
            "predicate_type",
            "status",
            "downstream_authority",
            "execution_authority",
            "repository_id",
            "source_git_sha",
            "release",
            "channel",
            "producer",
            "source_evidence",
            "artifact",
            "created_at",
            "retention_expires_at",
            "file_count",
            "payload_set_sha256",
            "files",
        },
        "channel payload",
    )
    if (
        type(value["schema_version"]) is not int
        or value["schema_version"] != 1
        or value["predicate_type"] != PREDICATE_TYPE
        or value["status"] != STATUS
        or value["downstream_authority"] is not False
        or value["execution_authority"] is not False
        or value["repository_id"] != REPOSITORY_ID
        or value["source_git_sha"] != source_sha
        or value["channel"] != channel
    ):
        raise ValueError("channel payload identity changed")
    payload_release = validate_release(value["release"], source_sha)
    if release is not None and payload_release != release:
        raise ValueError("payload release differs from its downstream request")
    validate_producer(value["producer"], channel)
    source = validate_source_evidence(
        value["source_evidence"],
        receipt_predicate_sha256=receipt_predicate_sha256,
    )
    artifact = validate_artifact(
        value["artifact"],
        channel=channel,
        source_sha=source_sha,
        release_id=payload_release["id"],
    )
    files = validate_files(value["files"], channel)
    if type(value["file_count"]) is not int or value["file_count"] != len(files):
        raise ValueError("payload file cardinality changed")
    expected_set = canonical_hash(files)
    if value["payload_set_sha256"] != expected_set:
        raise ValueError("payload set digest differs from its exact files")
    created = timestamp(value["created_at"], "payload created_at")
    retention = timestamp(value["retention_expires_at"], "payload retention_expires_at")
    artifact_updated = timestamp(artifact["updated_at"], "payload artifact updated_at")
    candidate_expires = timestamp(
        source["candidate_manifest_expires_at"],
        "payload candidate manifest expires_at",
    )
    if (
        created < artifact_updated
        or created >= candidate_expires
        or retention != timestamp(artifact["expires_at"], "payload artifact expires_at")
        or created >= retention
    ):
        raise ValueError("payload creation or retention lies outside its evidence")
    return value


def validate_receipt_predicate(
    predicate: dict[str, Any], reference: dict[str, Any], predicate_path: Path
) -> None:
    if file_hash(predicate_path) != reference["predicate_sha256"]:
        raise ValueError("publication receipt predicate digest differs")
    release = predicate.get("release")
    receipt = predicate.get("receipt")
    if (
        predicate.get("schema_version") != 1
        or predicate.get("status") != STATUS
        or predicate.get("downstream_authority") is not False
        or predicate.get("repository_id") != REPOSITORY_ID
        or predicate.get("source_git_sha") != reference["source_git_sha"]
        or not isinstance(release, dict)
        or not isinstance(receipt, dict)
    ):
        raise ValueError("publication receipt predicate identity changed")
    expected_release = reference["release"]
    for field in ("id", "ref", "intent_id", "kind", "tag_object_sha", "immutable"):
        if release.get(field) != expected_release[field]:
            raise ValueError("publication receipt release differs")
    expected_receipt = reference["receipt"]
    for field in ("run_id", "run_attempt", "workflow_id", "workflow_path"):
        if receipt.get(field) != expected_receipt[field]:
            raise ValueError("publication receipt producer differs")
    assets = predicate.get("assets")
    candidate = predicate.get("candidate")
    if (
        not isinstance(assets, list)
        or not assets
        or predicate.get("asset_count") != len(assets)
        or not isinstance(candidate, dict)
    ):
        raise ValueError("publication receipt source evidence is incomplete")


def load_artifact_metadata(path: Path) -> dict[str, Any]:
    value = read_object(path, "payload artifact API metadata")
    exact_keys(
        value,
        {
            "artifact_id",
            "name",
            "digest",
            "size_in_bytes",
            "created_at",
            "updated_at",
            "expires_at",
            "run_id",
            "source_git_sha",
        },
        "payload artifact API metadata",
    )
    return value


def collect_files(root: Path, values: list[str] | None) -> list[dict[str, Any]]:
    if values is None:
        raise ValueError("at least one ROLE=NAME payload file is required")
    if root.is_symlink() or not root.is_dir():
        raise ValueError("payload root must be one real directory")
    mapped: list[tuple[str, str]] = []
    names: set[str] = set()
    for value in values:
        if "=" not in value:
            raise ValueError("payload file must use ROLE=NAME")
        role, name = value.split("=", 1)
        if not role or name in names or SAFE_NAME.fullmatch(name) is None:
            raise ValueError("payload file role or name is invalid")
        mapped.append((role, name))
        names.add(name)
    paths = list(root.iterdir())
    if any(path.is_symlink() or not path.is_file() for path in paths):
        raise ValueError("payload root can contain only regular files")
    if {path.name for path in paths} != names:
        raise ValueError("payload root file set differs from its role mapping")
    files = [
        {
            "name": name,
            "role": role,
            "size": (root / name).stat().st_size,
            "sha256": file_hash(root / name),
        }
        for role, name in mapped
    ]
    files.sort(key=lambda item: item["name"])
    return files


def expected_documents(
    args: argparse.Namespace,
) -> tuple[dict[str, Any], dict[str, Any]]:
    reference = read_object(args.receipt_reference, "publication receipt reference")
    validate_receipt_reference(reference)
    predicate_path = args.receipt_predicate.resolve(strict=True)
    predicate = read_object(predicate_path, "publication receipt predicate")
    validate_receipt_predicate(predicate, reference, predicate_path)
    producer = read_object(args.producer, "payload producer")
    validate_producer(producer, args.channel)
    receipt_identity = reference["receipt"]
    if (
        producer["run_id"] != receipt_identity["run_id"]
        or producer["run_attempt"] != receipt_identity["run_attempt"]
        or producer["workflow_id"] != receipt_identity["workflow_id"]
        or producer["workflow_path"] != receipt_identity["workflow_path"]
    ):
        raise ValueError("payload producer differs from the exact receipt run")
    metadata = load_artifact_metadata(args.artifact_metadata)
    if (
        metadata["run_id"] != producer["run_id"]
        or metadata["source_git_sha"] != reference["source_git_sha"]
    ):
        raise ValueError("payload artifact API identity differs from its producer")
    release = reference["release"]
    files = collect_files(args.payload_dir.resolve(strict=True), args.file)
    candidate = predicate["candidate"]
    source_evidence = {
        "receipt_predicate_sha256": reference["predicate_sha256"],
        "candidate_manifest_sha256": candidate["manifest_sha256"],
        "candidate_manifest_artifact_digest": candidate["manifest_artifact_digest"],
        "candidate_manifest_expires_at": candidate["manifest_expires_at"],
        "release_asset_set_sha256": canonical_hash(predicate["assets"]),
        "sha256sums_sha256": predicate["sha256sums_sha256"],
    }
    artifact = {
        "artifact_id": metadata["artifact_id"],
        "name": metadata["name"],
        "archive_digest": metadata["digest"],
        "size_in_bytes": metadata["size_in_bytes"],
        "created_at": metadata["created_at"],
        "updated_at": metadata["updated_at"],
        "expires_at": metadata["expires_at"],
    }
    payload = {
        "schema_version": 1,
        "predicate_type": PREDICATE_TYPE,
        "status": STATUS,
        "downstream_authority": False,
        "execution_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": reference["source_git_sha"],
        "release": release,
        "channel": args.channel,
        "producer": producer,
        "source_evidence": source_evidence,
        "artifact": artifact,
        "created_at": args.created_at,
        "retention_expires_at": metadata["expires_at"],
        "file_count": len(files),
        "payload_set_sha256": canonical_hash(files),
        "files": files,
    }
    validate_payload_document(
        payload,
        channel=args.channel,
        source_sha=reference["source_git_sha"],
        receipt_predicate_sha256=reference["predicate_sha256"],
        release=release,
    )
    subject = {
        "schema_version": 1,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": reference["source_git_sha"],
        "release_id": release["id"],
        "release_ref": release["ref"],
        "channel": args.channel,
        "payload_sha256": "",
        "payload_set_sha256": payload["payload_set_sha256"],
        "artifact": {
            "artifact_id": artifact["artifact_id"],
            "name": artifact["name"],
            "archive_digest": artifact["archive_digest"],
        },
    }
    return payload, subject


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("create", "verify"):
        command = commands.add_parser(name)
        command.add_argument("--receipt-reference", type=Path, required=True)
        command.add_argument("--receipt-predicate", type=Path, required=True)
        command.add_argument("--producer", type=Path, required=True)
        command.add_argument("--artifact-metadata", type=Path, required=True)
        command.add_argument(
            "--channel", choices=tuple(contract_channels()), required=True
        )
        command.add_argument("--payload-dir", type=Path, required=True)
        command.add_argument("--file", action="append")
        command.add_argument("--created-at", required=True)
        if name == "create":
            command.add_argument("--output", type=Path, required=True)
            command.add_argument("--subject-output", type=Path, required=True)
        else:
            command.add_argument("--document", type=Path, required=True)
            command.add_argument("--subject", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    payload, subject = expected_documents(args)
    if args.command == "create":
        write_object(args.output, payload)
        subject["payload_sha256"] = file_hash(args.output)
        write_object(args.subject_output, subject)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "channel payload")
        if actual != payload:
            raise ValueError("channel payload differs from its exact evidence")
        subject["payload_sha256"] = file_hash(args.document)
        actual_subject = read_object(args.subject, "channel payload subject")
        if actual_subject != subject:
            raise ValueError("channel payload subject differs from its exact evidence")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"downstream-payload: {error}", file=sys.stderr)
        raise SystemExit(1) from error
