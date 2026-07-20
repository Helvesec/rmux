#!/usr/bin/env python3
"""Create an exhaustive summary while result aggregation remains disarmed."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import (
    CHANNELS,
    REPOSITORY_ID,
    STATUS,
    file_hash,
    load_contract,
    read_object,
    timestamp,
    write_object,
)
from downstream_plan import validate_plan


def _mappings(values: list[str], label: str) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for raw in values:
        channel, separator, encoded = raw.partition("=")
        if separator != "=" or channel not in CHANNELS or not encoded:
            raise ValueError(f"{label} must use CHANNEL=PATH for a known channel")
        if channel in result:
            raise ValueError(f"duplicate {label} for {channel}")
        result[channel] = Path(encoded)
    return result


def expected_summary(args: argparse.Namespace) -> dict[str, Any]:
    plan_path = args.plan.resolve(strict=True)
    plan = read_object(plan_path, "downstream channel plan")
    validate_plan(plan)
    predicates = _mappings(args.result_predicate, "result predicate")
    envelopes = _mappings(args.result_envelope, "result envelope")
    if predicates or envelopes:
        raise ValueError(
            "result aggregation is blocked until exact references and attestations exist"
        )
    result_contract = load_contract()["result_evidence"]
    blockers = result_contract["aggregation_blockers"]
    if result_contract["result_aggregation_ready"] is not False or not blockers:
        raise ValueError("result aggregation contract unexpectedly became authoritative")
    entries: list[dict[str, Any]] = []
    for planned in plan["channels"]:
        decision = planned["execution_decision"]
        if decision not in {"denied", "blocked"}:
            raise ValueError(
                f"channel {planned['name']} requires the missing result-reference contract"
            )
        entries.append(
            {
                "channel": planned["name"],
                "state": "denied-by-policy" if decision == "denied" else "blocked",
                "request_sha256": None,
                "predicate_sha256": None,
                "envelope_sha256": None,
                "public_live": False,
            }
        )
    created = timestamp(args.created_at, "channel summary created_at")
    if created < timestamp(plan["created_at"], "plan created_at"):
        raise ValueError("channel summary predates its exact plan")
    unresolved = [
        item["channel"] for item in entries if item["state"] != "denied-by-policy"
    ]
    return {
        "schema_version": 1,
        "status": STATUS,
        "downstream_authority": False,
        "execution_authority": False,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": plan["source_git_sha"],
        "release": plan["release"],
        "receipt": plan["receipt"],
        "plan_sha256": file_hash(plan_path),
        "channel_policy_sha256": plan["channel_policy"]["sha256"],
        "result_aggregation_ready": False,
        "aggregation_blockers": blockers,
        "result_count": len(CHANNELS),
        "results": entries,
        "advertised_channels": [],
        "unresolved_channels": unresolved,
        "rmux_io_last": True,
        "rmux_io_two_phase_ready": False,
        "rmux_io_authority": False,
        "created_at": args.created_at,
    }


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--result-predicate", action="append", default=[])
    parser.add_argument("--result-envelope", action="append", default=[])
    parser.add_argument("--created-at", required=True)


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
    expected = expected_summary(args)
    if args.command == "create":
        write_object(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "downstream channel summary")
        if actual != expected:
            raise ValueError("downstream channel summary changed")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"channel-summary: {error}", file=sys.stderr)
        raise SystemExit(1) from error
