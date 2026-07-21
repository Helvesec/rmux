#!/usr/bin/env python3
"""Create or verify one exact two-phase downstream channel summary."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from downstream_channels import CHANNELS, file_hash, read_object, write_object
from downstream_summary import create_summary, validate_summary


def mappings(values: list[str]) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for raw in values:
        channel, separator, encoded = raw.partition("=")
        if separator != "=" or channel not in CHANNELS or not encoded:
            raise ValueError("result reference must use CHANNEL=PATH")
        if channel in result:
            raise ValueError(f"duplicate result reference for {channel}")
        result[channel] = Path(encoded)
    return result


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--phase", choices=("pre-site", "final"), required=True)
    parser.add_argument("--result-reference", action="append", default=[])
    parser.add_argument("--pre-site-summary", type=Path)
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
    expected = create_summary(
        plan_path=args.plan,
        phase=args.phase,
        result_paths=mappings(args.result_reference),
        pre_site_summary_path=args.pre_site_summary,
        created_at=args.created_at,
    )
    if args.command == "create":
        write_object(args.output, expected)
        print(file_hash(args.output))
    else:
        actual = read_object(args.document, "downstream channel summary")
        validate_summary(actual)
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
