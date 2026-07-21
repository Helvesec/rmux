#!/usr/bin/env python3
"""Prepare and revalidate one exact, single-depth downstream channel retry."""

from __future__ import annotations

import argparse
import shutil
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from channel_request import expected_request
from downstream_channels import (
    canonical_hash,
    file_hash,
    read_object,
    validate_payload,
    validate_receipt_reference,
    validate_request,
    write_object,
)
from downstream_plan import validate_plan
from downstream_result import validate_retryable_previous
from downstream_result_document import validate_envelope, validate_predicate

CHANNEL_PRODUCERS = {
    "chocolatey": ".github/workflows/release-chocolatey-channel.yml",
    "snap_candidate": ".github/workflows/release-snap-channel.yml",
}

EVIDENCE_FILES = {
    "receipt": {
        "publication-receipt-predicate.json",
        "publication-receipt.sigstore.json",
        "release-state.json",
    },
    "receipt-envelope": {"publication-receipt-envelope.json"},
    "result": {
        "channel-payload.json",
        "downstream-channel-plan.json",
        "downstream-channel-request.json",
        "downstream-channel-result-predicate.json",
        "downstream-channel-result.sigstore.json",
        "downstream-channel-target-evidence.json",
        "receipt-reference.json",
    },
    "result-envelope": {"downstream-channel-result-envelope.json"},
}


def exact_file_set(root: Path, expected: set[str], label: str) -> None:
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"{label} must be one real directory")
    paths = list(root.rglob("*"))
    if any(path.is_symlink() for path in paths):
        raise ValueError(f"{label} contains a symlink")
    actual = {path.relative_to(root).as_posix() for path in paths if path.is_file()}
    if actual != expected:
        raise ValueError(
            f"{label} file set differs: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def artifact_metadata(path: Path, label: str) -> dict[str, Any]:
    value = read_object(path, label)
    expected = {
        "artifact_id",
        "name",
        "digest",
        "size_in_bytes",
        "created_at",
        "updated_at",
        "expires_at",
        "run_id",
        "source_git_sha",
    }
    if set(value) != expected:
        raise ValueError(f"{label} fields differ")
    return value


def validate_payload_files(payload: dict[str, Any], root: Path) -> None:
    expected = {item["name"] for item in payload["files"]}
    exact_file_set(root, expected, "retry payload")
    for item in payload["files"]:
        path = root / item["name"]
        if path.stat().st_size != item["size"] or file_hash(path) != item["sha256"]:
            raise ValueError(f"retry payload bytes differ for {item['name']}")
    if canonical_hash(payload["files"]) != payload["payload_set_sha256"]:
        raise ValueError("retry payload set digest differs")


def validate_identity(value: Any, expected: Any, label: str) -> None:
    if value != expected:
        raise ValueError(f"{label} differs")


def validate_evidence(
    args: argparse.Namespace,
) -> tuple[dict[str, Any], dict[str, Any]]:
    root = args.root.resolve(strict=True)
    for directory, names in EVIDENCE_FILES.items():
        exact_file_set(root / directory, names, f"retry {directory}")

    result_root = root / "result"
    predicate_path = root / "receipt/publication-receipt-predicate.json"
    receipt_envelope_path = root / "receipt-envelope/publication-receipt-envelope.json"
    request_path = result_root / "downstream-channel-request.json"
    result_path = result_root / "downstream-channel-result-predicate.json"
    result_envelope_path = (
        root / "result-envelope/downstream-channel-result-envelope.json"
    )

    reference = read_object(result_root / "receipt-reference.json", "receipt reference")
    validate_receipt_reference(reference)
    validate_identity(reference["source_git_sha"], args.source_sha, "receipt source")
    validate_identity(reference["release"]["id"], args.release_id, "receipt release ID")
    validate_identity(
        reference["release"]["ref"], args.release_ref, "receipt release ref"
    )
    validate_identity(
        reference["predicate_sha256"],
        file_hash(predicate_path),
        "receipt predicate digest",
    )
    validate_identity(
        reference["envelope_sha256"],
        file_hash(receipt_envelope_path),
        "receipt envelope digest",
    )
    validate_identity(
        reference["predicate_sha256"],
        args.receipt_predicate_sha256,
        "receipt predicate input",
    )
    validate_identity(
        reference["envelope_sha256"],
        args.receipt_envelope_sha256,
        "receipt envelope input",
    )
    validate_identity(
        reference["receipt"],
        {
            "run_id": args.receipt_run_id,
            "run_attempt": 1,
            "workflow_id": args.receipt_workflow_id,
            "workflow_path": ".github/workflows/release-receipt.yml",
        },
        "receipt producer",
    )
    for field, expected in (
        ("predicate_bundle", (args.receipt_artifact_id, args.receipt_artifact_digest)),
        (
            "envelope_bundle",
            (args.receipt_envelope_artifact_id, args.receipt_envelope_artifact_digest),
        ),
    ):
        artifact = reference[field]
        validate_identity(artifact["artifact_id"], expected[0], f"receipt {field} ID")
        validate_identity(
            artifact["archive_digest"], expected[1], f"receipt {field} digest"
        )

    plan = read_object(result_root / "downstream-channel-plan.json", "channel plan")
    validate_plan(plan)
    embedded_receipt = {
        **reference["receipt"],
        "predicate_bundle": reference["predicate_bundle"],
        "predicate_sha256": reference["predicate_sha256"],
        "envelope_bundle": reference["envelope_bundle"],
        "envelope_sha256": reference["envelope_sha256"],
        "attestation": reference["attestation"],
        "verified_at": reference["verified_at"],
    }
    validate_identity(plan["receipt"], embedded_receipt, "plan receipt")

    payload = read_object(result_root / "channel-payload.json", "channel payload")
    validate_payload(payload, args.channel, args.source_sha)
    validate_identity(payload["release"], reference["release"], "payload release")
    validate_identity(payload["producer"]["run_id"], args.receipt_run_id, "payload run")

    request = read_object(request_path, "original channel request")
    validate_request(request)
    validate_identity(request["channel"], args.channel, "original request channel")
    validate_identity(request["operation"], "initial", "original request operation")
    validate_identity(request["retry_depth"], 0, "original request retry depth")
    validate_identity(
        request["idempotency_key"], args.idempotency_key, "request idempotency key"
    )
    validate_identity(request["payload_artifact"], payload, "request payload")
    validate_identity(request["receipt"], plan["receipt"], "request receipt")
    validate_identity(
        request["plan_sha256"],
        file_hash(result_root / "downstream-channel-plan.json"),
        "request plan digest",
    )

    result = read_object(result_path, "prior result predicate")
    validate_predicate(result, request, request_path)
    expected_producer_path = CHANNEL_PRODUCERS[args.channel]
    validate_identity(
        result["producer"]["run_id"], args.prior_result_run_id, "result run"
    )
    validate_identity(
        result["producer"]["workflow_id"],
        args.prior_result_producer_workflow_id,
        "result producer workflow ID",
    )
    validate_identity(
        result["producer"]["workflow_path"],
        expected_producer_path,
        "result producer workflow path",
    )
    validate_identity(
        args.prior_result_producer_workflow_path,
        expected_producer_path,
        "result producer workflow input",
    )
    validate_retryable_previous(result)

    envelope = read_object(result_envelope_path, "prior result envelope")
    validate_envelope(envelope, predicate=result, predicate_path=result_path)
    validate_identity(
        file_hash(result_path),
        args.prior_result_predicate_sha256,
        "result predicate digest",
    )
    validate_identity(
        file_hash(result_envelope_path),
        args.prior_result_envelope_sha256,
        "result envelope digest",
    )

    result_meta = artifact_metadata(root / "result-artifact.json", "result artifact")
    envelope_meta = artifact_metadata(
        root / "result-envelope-artifact.json", "result envelope artifact"
    )
    for metadata, artifact_id, digest, label in (
        (
            result_meta,
            args.prior_result_artifact_id,
            args.prior_result_artifact_digest,
            "result artifact",
        ),
        (
            envelope_meta,
            args.prior_result_envelope_artifact_id,
            args.prior_result_envelope_artifact_digest,
            "result envelope artifact",
        ),
    ):
        validate_identity(metadata["artifact_id"], artifact_id, f"{label} ID")
        validate_identity(metadata["digest"], digest, f"{label} digest")
        validate_identity(metadata["run_id"], args.prior_result_run_id, f"{label} run")
        validate_identity(
            metadata["source_git_sha"], args.source_sha, f"{label} source"
        )
    expected_bundle = {
        "artifact_id": result_meta["artifact_id"],
        "name": result_meta["name"],
        "archive_digest": result_meta["digest"],
        "size_in_bytes": result_meta["size_in_bytes"],
    }
    validate_identity(envelope["result_bundle"], expected_bundle, "result bundle")

    previous = {
        "predicate_artifact_id": result_meta["artifact_id"],
        "predicate_artifact_digest": result_meta["digest"],
        "envelope_artifact_id": envelope_meta["artifact_id"],
        "envelope_artifact_digest": envelope_meta["digest"],
        "predicate_sha256": file_hash(result_path),
        "envelope_sha256": file_hash(result_envelope_path),
        "request_sha256": file_hash(request_path),
        "state": result["state"],
        "mutation_started": result["mutation_started"],
        "remote_request_id": result["remote_request_id"],
    }
    validate_retryable_previous(previous)
    return payload, previous


def request_namespace(
    *,
    prepared: Path,
    channel: str,
    requested_at: str,
    expires_at: str,
) -> SimpleNamespace:
    return SimpleNamespace(
        plan=prepared / "plan/downstream-channel-plan.json",
        channel=channel,
        operation="retry",
        payload_artifact=prepared / "channel-payload.json",
        pre_site_summary=None,
        retry_of=prepared / "original-request.json",
        previous_result=prepared / "previous-result.json",
        requested_at=requested_at,
        expires_at=expires_at,
    )


def verify_prepared(args: argparse.Namespace) -> None:
    prepared = args.prepared.resolve(strict=True)
    payload = read_object(prepared / "channel-payload.json", "retry payload")
    validate_payload(payload, args.channel, args.source_sha)
    payload_names = {item["name"] for item in payload["files"]}
    expected = {
        "channel-payload.json",
        "downstream-channel-request.json",
        "original-request.json",
        "previous-result.json",
        "plan/downstream-channel-plan.json",
        "plan/receipt-reference.json",
        *(f"payload/{name}" for name in payload_names),
    }
    exact_file_set(prepared, expected, "prepared retry bundle")
    validate_payload_files(payload, prepared / "payload")
    request = read_object(prepared / "downstream-channel-request.json", "retry request")
    validate_request(request)
    validate_identity(request["source_git_sha"], args.source_sha, "retry source")
    validate_identity(request["release"]["id"], args.release_id, "retry release ID")
    validate_identity(request["release"]["ref"], args.release_ref, "retry release ref")
    validate_identity(
        request["idempotency_key"], args.idempotency_key, "retry idempotency"
    )
    expected = expected_request(
        request_namespace(
            prepared=prepared,
            channel=args.channel,
            requested_at=request["requested_at"],
            expires_at=request["expires_at"],
        )
    )
    validate_identity(request, expected, "prepared retry request")


def prepare(args: argparse.Namespace) -> None:
    payload, previous = validate_evidence(args)
    payload_meta = artifact_metadata(
        args.root / "payload-artifact-live.json", "payload artifact"
    )
    validate_identity(
        payload_meta["artifact_id"],
        payload["artifact"]["artifact_id"],
        "payload artifact ID",
    )
    validate_identity(
        payload_meta["digest"],
        payload["artifact"]["archive_digest"],
        "payload artifact digest",
    )
    validate_identity(
        payload_meta["run_id"], payload["producer"]["run_id"], "payload run"
    )
    validate_identity(payload_meta["source_git_sha"], args.source_sha, "payload source")
    validate_payload_files(payload, args.root / "payload")

    prepared = args.output
    if prepared.exists() or prepared.is_symlink():
        raise ValueError("prepared retry output already exists")
    (prepared / "plan").mkdir(parents=True)
    (prepared / "payload").mkdir()
    result_root = args.root / "result"
    shutil.copyfile(
        result_root / "downstream-channel-plan.json",
        prepared / "plan/downstream-channel-plan.json",
    )
    shutil.copyfile(
        result_root / "receipt-reference.json",
        prepared / "plan/receipt-reference.json",
    )
    shutil.copyfile(
        result_root / "channel-payload.json", prepared / "channel-payload.json"
    )
    shutil.copyfile(
        result_root / "downstream-channel-request.json",
        prepared / "original-request.json",
    )
    write_object(prepared / "previous-result.json", previous)
    for item in payload["files"]:
        shutil.copyfile(
            args.root / "payload" / item["name"], prepared / "payload" / item["name"]
        )

    now = datetime.now(UTC).replace(microsecond=0)
    retention = datetime.fromisoformat(
        payload["retention_expires_at"].replace("Z", "+00:00")
    )
    expires = min(now + timedelta(hours=24), retention)
    if expires <= now:
        raise ValueError("exact retry payload has expired")

    def render(value: datetime) -> str:
        return value.isoformat().replace("+00:00", "Z")

    request = expected_request(
        request_namespace(
            prepared=prepared,
            channel=args.channel,
            requested_at=render(now),
            expires_at=render(expires),
        )
    )
    write_object(prepared / "downstream-channel-request.json", request)
    verify_prepared(
        SimpleNamespace(
            prepared=prepared,
            channel=args.channel,
            source_sha=args.source_sha,
            release_id=args.release_id,
            release_ref=args.release_ref,
            idempotency_key=args.idempotency_key,
        )
    )


def add_identity_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--channel", choices=tuple(CHANNEL_PRODUCERS), required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-id", type=int, required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--idempotency-key", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    add_identity_args(prepare_parser)
    prepare_parser.add_argument("--root", type=Path, required=True)
    prepare_parser.add_argument("--output", type=Path, required=True)
    for name in (
        "receipt_run_id",
        "receipt_workflow_id",
        "receipt_artifact_id",
        "receipt_envelope_artifact_id",
        "prior_result_run_id",
        "prior_result_producer_workflow_id",
        "prior_result_artifact_id",
        "prior_result_envelope_artifact_id",
    ):
        prepare_parser.add_argument(
            f"--{name.replace('_', '-')}", type=int, required=True
        )
    for name in (
        "receipt_artifact_digest",
        "receipt_envelope_artifact_digest",
        "receipt_predicate_sha256",
        "receipt_envelope_sha256",
        "prior_result_producer_workflow_path",
        "prior_result_artifact_digest",
        "prior_result_predicate_sha256",
        "prior_result_envelope_artifact_digest",
        "prior_result_envelope_sha256",
    ):
        prepare_parser.add_argument(f"--{name.replace('_', '-')}", required=True)
    verify_parser = commands.add_parser("verify-prepared")
    add_identity_args(verify_parser)
    verify_parser.add_argument("--prepared", type=Path, required=True)
    return parser.parse_args()


if __name__ == "__main__":
    try:
        arguments = parse_args()
        if arguments.command == "prepare":
            prepare(arguments)
        else:
            verify_prepared(arguments)
    except (OSError, ValueError) as error:
        print(f"prepare-channel-retry: {error}", file=sys.stderr)
        raise SystemExit(1) from error
