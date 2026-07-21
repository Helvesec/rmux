#!/usr/bin/env python3
"""Validate immutable inputs before any release-candidate work starts."""

from __future__ import annotations

import argparse
import json
import re
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SHA_RE = re.compile(r"[0-9a-f]{40}")
INTENT_RE = re.compile(r"[A-Za-z0-9._:-]{8,128}")
VERSION_RE = re.compile(r"(?P<version>[0-9]+\.[0-9]+\.[0-9]+)")


def workspace_version(repository_root: Path) -> str:
    manifest_path = repository_root / "Cargo.toml"
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    try:
        version = manifest["workspace"]["package"]["version"]
    except (KeyError, TypeError) as error:
        raise ValueError("Cargo.toml has no workspace.package.version") from error
    if not isinstance(version, str) or VERSION_RE.fullmatch(version) is None:
        raise ValueError(f"unsupported workspace package version: {version!r}")
    return version


def validate(args: argparse.Namespace) -> dict[str, object]:
    if SHA_RE.fullmatch(args.expected_source_sha) is None:
        raise ValueError("expected source SHA must be 40 lowercase hex characters")
    if args.actual_source_sha != args.expected_source_sha:
        raise ValueError("checked-out source SHA does not match the requested SHA")
    if args.github_ref != "refs/heads/main":
        raise ValueError("canonical candidates must run from refs/heads/main")
    if args.github_run_attempt != 1:
        raise ValueError("canonical candidate runs must use attempt 1")
    if args.fast_run_id <= 0:
        raise ValueError("fast run ID must be a positive integer")
    if INTENT_RE.fullmatch(args.release_intent_id) is None:
        raise ValueError("release intent ID uses characters outside the allowlist")

    package_version = workspace_version(args.repository_root.resolve())
    stable_ref = f"v{package_version}"
    rc_ref = re.fullmatch(
        rf"{re.escape(stable_ref)}-rc\.([1-9][0-9]*)", args.planned_release_ref
    )
    is_rc = rc_ref is not None
    if args.release_kind == "rc":
        if not is_rc:
            raise ValueError(f"an RC release requires {stable_ref}-rc.N")
    elif args.planned_release_ref != stable_ref:
        raise ValueError(
            f"{args.release_kind} release ref must be {stable_ref!r} for this source tree"
        )
    release_version = args.planned_release_ref.removeprefix("v")

    return {
        "schema_version": 1,
        "repository_id": 1239918790,
        "source_git_sha": args.expected_source_sha,
        "fast_run_id": args.fast_run_id,
        "release_intent_id": args.release_intent_id,
        "planned_release_ref": args.planned_release_ref,
        "release_kind": args.release_kind,
        "release_version": release_version,
        "package_version": package_version,
        "is_prerelease": is_rc,
        "candidate_run_attempt": args.github_run_attempt,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", type=Path, default=ROOT)
    parser.add_argument("--expected-source-sha", required=True)
    parser.add_argument("--actual-source-sha", required=True)
    parser.add_argument("--fast-run-id", required=True, type=int)
    parser.add_argument("--release-intent-id", required=True)
    parser.add_argument("--planned-release-ref", required=True)
    parser.add_argument(
        "--release-kind", choices=("shadow", "rc", "stable"), required=True
    )
    parser.add_argument("--github-ref", required=True)
    parser.add_argument("--github-run-attempt", required=True, type=int)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    result = validate(args)
    args.output.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
