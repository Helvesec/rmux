#!/usr/bin/env python3
"""Reject extra, missing, symbolic, or escaping files in one artifact tree."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path, PurePosixPath


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--file", action="append", required=True)
    return parser.parse_args()


def verify(root: Path, names: list[str]) -> None:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("artifact root must be one real directory")
    expected = set(names)
    if len(expected) != len(names) or not expected:
        raise ValueError("expected artifact files must be non-empty and unique")
    for name in expected:
        path = PurePosixPath(name)
        if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
            raise ValueError("expected artifact path is unsafe")
    resolved_root = root.resolve(strict=True)
    actual: set[str] = set()
    for entry in root.rglob("*"):
        if entry.is_symlink():
            raise ValueError("artifact tree contains a symlink")
        if entry.is_dir():
            continue
        if not entry.is_file() or not entry.resolve(strict=True).is_relative_to(
            resolved_root
        ):
            raise ValueError("artifact file escaped its root")
        actual.add(entry.relative_to(root).as_posix())
    if actual != expected:
        raise ValueError(
            f"artifact file set differs: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


if __name__ == "__main__":
    try:
        args = parse_args()
        verify(args.root, args.file)
    except (OSError, ValueError) as error:
        print(f"verify-exact-file-set: {error}", file=sys.stderr)
        raise SystemExit(1) from error
