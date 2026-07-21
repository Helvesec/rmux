#!/usr/bin/env python3
"""Create or verify one canonical downstream target observation."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Any

from downstream_channels import read_object, target_for_channel, write_object
from downstream_result import result_state, validate_target_evidence


def expected(args: argparse.Namespace) -> dict[str, Any]:
    state = result_state(args.state)
    target = target_for_channel(args.channel)
    value = {
        "schema_version": 1,
        "channel": args.channel,
        "target_kind": target["target_kind"],
        "repository_id": target["repository_id"],
        "external_id": args.external_id,
        "url": args.url,
        "version": args.version,
        "commit_sha": args.commit_sha,
        "public_live": state in {"no-op-exact", "public-live"},
        "observed_at": args.observed_at,
    }
    return validate_target_evidence(
        value,
        channel=args.channel,
        state=state,
        expected_target=target,
        expected_version=args.version,
    )


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--channel", required=True)
    parser.add_argument("--state", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--external-id")
    parser.add_argument("--url", required=True)
    parser.add_argument("--commit-sha")
    parser.add_argument("--observed-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    add_arguments(create)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    add_arguments(verify)
    verify.add_argument("--document", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    value = expected(args)
    if args.command == "create":
        write_object(args.output, value)
    elif read_object(args.document, "channel target evidence") != value:
        raise ValueError("channel target evidence changed")
    print("channel-target-evidence-ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"channel-target-evidence: {error}", file=sys.stderr)
        raise SystemExit(1) from error
