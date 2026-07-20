#!/usr/bin/env python3
"""Fail closed unless one reviewed release capability is explicitly active."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from policy_audit_contract import read_object, validate_activation


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("capability")
    parser.add_argument(
        "--ledger",
        type=Path,
        default=Path(".github/release/release-activation.json"),
    )
    args = parser.parse_args()
    ledger = read_object(args.ledger, "release activation ledger")
    validate_activation(ledger)
    if args.capability not in ledger["capabilities"]:
        raise ValueError("unknown release capability")
    if ledger["capabilities"][args.capability] is not True:
        raise ValueError(
            f"release capability {args.capability!r} is disabled until reviewed PR8 cut-over"
        )
    print(f"release-capability-enabled={args.capability}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(f"release-capability: {error}", file=sys.stderr)
        raise SystemExit(1) from error
