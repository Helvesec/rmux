#!/usr/bin/env python3
"""Create or verify a deterministic, disarmed downstream channel plan."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    CHANNELS,
    CHANNEL_POLICY,
    REPOSITORY_ID,
    downstream_status,
    file_hash,
    read_object,
    timestamp,
    validate_receipt_reference,
    write_object,
)
from downstream_plan import expected_channel_entries
from release_authority import load_authority


def expected_plan(args: argparse.Namespace) -> dict[str, Any]:
    reference = read_object(args.receipt_reference, "publication receipt reference")
    validate_receipt_reference(reference)
    authority = load_authority()
    if reference["downstream_authority"] is not authority.active:
        raise ValueError("receipt authority differs from the tracked activation ledger")
    release = reference["release"]
    if release["kind"] != args.release_kind:
        raise ValueError("requested release kind differs from the immutable receipt")
    created = timestamp(args.created_at, "channel plan created_at")
    if created < timestamp(reference["verified_at"], "receipt verified_at"):
        raise ValueError("channel plan predates its publication receipt")
    entries = expected_channel_entries(
        args.release_kind,
        args.snap_candidate_opt_in,
        authority_active=authority.active,
    )
    receipt = {
        **reference["receipt"],
        "predicate_bundle": reference["predicate_bundle"],
        "predicate_sha256": reference["predicate_sha256"],
        "envelope_bundle": reference["envelope_bundle"],
        "envelope_sha256": reference["envelope_sha256"],
        "attestation": reference["attestation"],
        "verified_at": reference["verified_at"],
    }
    return {
        "schema_version": 1,
        "status": downstream_status(authority.active),
        "downstream_authority": authority.active,
        "execution_authority": authority.active,
        "execution_enabled": authority.active,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": reference["source_git_sha"],
        "release": release,
        "receipt": receipt,
        "channel_policy": {
            "path": ".github/release/channel-policy.json",
            "schema_version": 1,
            "sha256": file_hash(CHANNEL_POLICY),
        },
        "snap_candidate_opt_in": args.snap_candidate_opt_in,
        "created_at": args.created_at,
        "channel_count": len(CHANNELS),
        "channels": entries,
    }


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--receipt-reference", type=Path, required=True)
    parser.add_argument("--release-kind", choices=("rc", "stable"), required=True)
    parser.add_argument("--snap-candidate-opt-in", action="store_true")
    parser.add_argument("--created-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    create = subparsers.add_parser("create-plan")
    add_common(create)
    create.add_argument("--output", type=Path, required=True)
    verify = subparsers.add_parser("verify-plan")
    add_common(verify)
    verify.add_argument("--document", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    expected = expected_plan(args)
    if args.command == "create-plan":
        write_object(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "downstream channel plan")
        if actual != expected:
            raise ValueError("downstream channel plan changed")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"channel-policy: {error}", file=sys.stderr)
        raise SystemExit(1) from error
