#!/usr/bin/env python3
"""Create or verify one exact downstream result reference."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from downstream_channels import file_hash, read_object
from downstream_result_reference import (
    create_reference,
    validate_reference,
    write_reference,
)


def arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--predicate", type=Path, required=True)
    parser.add_argument("--envelope", type=Path, required=True)
    parser.add_argument("--predicate-artifact", type=Path, required=True)
    parser.add_argument("--envelope-artifact", type=Path, required=True)
    parser.add_argument("--verified-at", required=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    arguments(create)
    create.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser("verify")
    arguments(verify)
    verify.add_argument("--document", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    expected = create_reference(
        request_path=args.request,
        predicate_path=args.predicate,
        envelope_path=args.envelope,
        predicate_artifact_path=args.predicate_artifact,
        envelope_artifact_path=args.envelope_artifact,
        verified_at=args.verified_at,
    )
    if args.command == "create":
        write_reference(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "channel result reference")
        validate_reference(actual)
        if actual != expected:
            raise ValueError("downstream result reference changed")
        print(file_hash(args.document))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"channel-result-reference: {error}", file=sys.stderr)
        raise SystemExit(1) from error
