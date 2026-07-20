#!/usr/bin/env python3
"""Create or verify byte-bound, disarmed downstream channel requests."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    DIGEST,
    REPOSITORY_ID,
    SHA256,
    STATUS,
    canonical_hash,
    exact_keys,
    file_hash,
    match,
    positive,
    read_object,
    timestamp,
    target_for_channel,
    validate_payload,
    validate_request,
    write_object,
)
from downstream_plan import validate_plan
from downstream_result import validate_retryable_previous


def request_idempotency_key(
    receipt_predicate_sha256: str,
    channel: str,
    release_ref: str,
    payload_set_sha256: str,
    target: dict[str, Any],
) -> str:
    for value, label in (
        (receipt_predicate_sha256, "receipt predicate digest"),
        (payload_set_sha256, "payload set digest"),
    ):
        match(value, SHA256, label)
    material = {
        "receipt_predicate_sha256": receipt_predicate_sha256,
        "channel": channel,
        "release_ref": release_ref,
        "payload_set_sha256": payload_set_sha256,
        "target": target,
    }
    return f"rmux-downstream-v1:{canonical_hash(material)}"


def previous_result(path: Path | None) -> dict[str, Any] | None:
    if path is None:
        return None
    value = read_object(path, "previous channel result reference")
    exact_keys(
        value,
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
    positive(value["predicate_artifact_id"], "previous predicate artifact ID")
    positive(value["envelope_artifact_id"], "previous envelope artifact ID")
    match(
        value["predicate_artifact_digest"], DIGEST, "previous predicate artifact digest"
    )
    match(
        value["envelope_artifact_digest"], DIGEST, "previous envelope artifact digest"
    )
    match(value["predicate_sha256"], SHA256, "previous predicate digest")
    match(value["envelope_sha256"], SHA256, "previous envelope digest")
    match(value["request_sha256"], SHA256, "previous request digest")
    validate_retryable_previous(value)
    return value


def expected_request(args: argparse.Namespace) -> dict[str, Any]:
    plan_path = args.plan.resolve(strict=True)
    plan = read_object(plan_path, "downstream channel plan")
    validate_plan(plan)
    entry = next(
        (item for item in plan["channels"] if item["name"] == args.channel), None
    )
    if entry is None:
        raise ValueError("requested channel is absent from the exact plan")
    if args.channel == "rmux_io":
        raise ValueError("rmux_io requires the unimplemented two-phase summary contract")
    if (
        entry["execution_decision"] != "disarmed"
        or entry["execution_enabled"] is not False
        or entry["blockers"] != []
        or entry["payload_ready"] is not True
    ):
        raise ValueError("channel is not an exact blocker-free disarmed request")
    payload = read_object(args.payload_artifact, "channel payload artifact")
    validate_payload(payload, args.channel, plan["source_git_sha"])
    requested = timestamp(args.requested_at, "request requested_at")
    if requested < timestamp(plan["created_at"], "plan created_at"):
        raise ValueError("channel request predates its exact plan")
    target = target_for_channel(args.channel)
    payload_set_sha256 = canonical_hash(payload["files"])
    idempotency_key = request_idempotency_key(
        plan["receipt"]["predicate_sha256"],
        args.channel,
        plan["release"]["ref"],
        payload_set_sha256,
        target,
    )
    retry_digest: str | None = None
    retry_depth = 0
    bound_previous = previous_result(args.previous_result)
    if args.operation == "initial":
        if args.retry_of is not None or bound_previous is not None:
            raise ValueError("initial channel request cannot bind retry evidence")
    else:
        if args.retry_of is None or bound_previous is None:
            raise ValueError(
                "retry requires exact original request and result evidence"
            )
        original = read_object(args.retry_of, "original channel request")
        validate_request(original)
        if original["operation"] != "initial" or original["retry_depth"] != 0:
            raise ValueError("downstream retries are limited to one exact attempt")
        retry_digest = file_hash(args.retry_of)
        retry_depth = 1
        if bound_previous["request_sha256"] != retry_digest:
            raise ValueError("previous result does not bind the original request")
        for field, expected in (
            ("repository_id", REPOSITORY_ID),
            ("source_git_sha", plan["source_git_sha"]),
            ("release", plan["release"]),
            ("receipt", plan["receipt"]),
            ("plan_sha256", file_hash(plan_path)),
            ("channel", args.channel),
            ("idempotency_key", idempotency_key),
            ("payload_artifact", payload),
            ("payload_set_sha256", payload_set_sha256),
            ("target", target),
        ):
            if original[field] != expected:
                raise ValueError(f"retry changed exact original field {field}")
    value = {
        "schema_version": 1,
        "status": STATUS,
        "downstream_authority": False,
        "execution_authority": False,
        "execution_enabled": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": plan["source_git_sha"],
        "release": plan["release"],
        "receipt": plan["receipt"],
        "plan_sha256": file_hash(plan_path),
        "channel": args.channel,
        "operation": args.operation,
        "retry_depth": retry_depth,
        "idempotency_key": idempotency_key,
        "retry_of_request_sha256": retry_digest,
        "payload_artifact": payload,
        "payload_set_sha256": payload_set_sha256,
        "target": target,
        "previous_result": bound_previous,
        "rebuild_native": False,
        "requested_at": args.requested_at,
        "expires_at": args.expires_at,
    }
    validate_request(value)
    return value


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--channel", required=True)
    parser.add_argument("--operation", choices=("initial", "retry"), required=True)
    parser.add_argument("--payload-artifact", type=Path, required=True)
    parser.add_argument("--retry-of", type=Path)
    parser.add_argument("--previous-result", type=Path)
    parser.add_argument("--requested-at", required=True)
    parser.add_argument("--expires-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    add_common(create)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    add_common(verify)
    verify.add_argument("--document", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    expected = expected_request(args)
    if args.command == "create":
        write_object(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "downstream channel request")
        if actual != expected:
            raise ValueError("downstream channel request changed")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"channel-request: {error}", file=sys.stderr)
        raise SystemExit(1) from error
