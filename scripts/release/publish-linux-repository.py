#!/usr/bin/env python3
"""Publish one exact signed APT/RPM tree to the RMUX package repository."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
from datetime import UTC, datetime
from pathlib import Path

from downstream_channels import target_for_channel, write_object
from downstream_result import validate_target_evidence
from github_repository_writer import GitHubApi, publish, repository_identity

SOURCE_SHA = re.compile(r"[0-9a-f]{40}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--target-evidence", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    return parser.parse_args()


def repository_files(root: Path) -> dict[str, bytes]:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("signed package repository must be one real directory")
    resolved = root.resolve(strict=True)
    updates: dict[str, bytes] = {}
    for entry in root.rglob("*"):
        if entry.is_symlink():
            raise ValueError("signed package repository cannot contain symlinks")
        if entry.is_dir():
            continue
        if not entry.is_file() or not entry.resolve(strict=True).is_relative_to(
            resolved
        ):
            raise ValueError("signed package repository file escaped its root")
        name = entry.relative_to(root).as_posix()
        if name in {"PACKAGE_REPOSITORY_BASE", "SHA256SUMS"}:
            continue
        if not (
            name in {"index.html", "_headers"} or name.startswith(("debian/", "rpm/"))
        ):
            raise ValueError("signed package repository contains an unexpected path")
        updates[name] = entry.read_bytes()
    required = {
        "index.html",
        "_headers",
        "debian/dists/stable/InRelease",
        "debian/dists/stable/Release",
        "debian/dists/stable/Release.gpg",
        "debian/rmux.asc",
        "rpm/repodata/repomd.xml",
        "rpm/repodata/repomd.xml.asc",
        "rpm/RPM-GPG-KEY-rmux",
        "rpm/RPM-GPG-KEY-rmux-repository",
        "rpm/rmux.repo",
    }
    if not required <= set(updates):
        raise ValueError("signed package repository is incomplete")
    checksum_path = root / "SHA256SUMS"
    expected_lines = []
    for name in sorted(
        path for path in updates if path.startswith(("debian/", "rpm/"))
    ):
        expected_lines.append(f"{hashlib.sha256(updates[name]).hexdigest()}  {name}\n")
    if checksum_path.read_text(encoding="utf-8") != "".join(expected_lines):
        raise ValueError("signed package repository checksum inventory differs")
    return updates


def execute(args: argparse.Namespace) -> None:
    if (
        SOURCE_SHA.fullmatch(args.source_sha) is None
        or RELEASE_REF.fullmatch(args.release_ref) is None
    ):
        raise ValueError("APT/RPM release identity is malformed")
    version = args.release_ref.removeprefix("v")
    base_path = args.repository_dir / "PACKAGE_REPOSITORY_BASE"
    expected_base = base_path.read_text(encoding="utf-8").strip()
    if SOURCE_SHA.fullmatch(expected_base) is None:
        raise ValueError("package repository base commit is malformed")
    updates = repository_files(args.repository_dir)
    target = target_for_channel("apt_rpm")
    full_name = target["repository_full_name"]
    repository_id = target["repository_id"]
    if not isinstance(full_name, str) or not isinstance(repository_id, int):
        raise ValueError("APT/RPM target identity is incomplete")
    api = GitHubApi(os.environ.get("RMUX_DOWNSTREAM_TOKEN", ""))
    repository_identity(api, full_name, repository_id, "main")
    outcome = publish(
        api,
        full_name=full_name,
        branch="main",
        updates=updates,
        message=f"packages: publish rmux {args.release_ref}",
        managed_prefixes=("debian", "rpm"),
        expected_base=expected_base,
    )
    observed_at = (
        datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    )
    evidence = {
        "schema_version": 1,
        "channel": "apt_rpm",
        "target_kind": target["target_kind"],
        "repository_id": repository_id,
        "external_id": outcome.commit_sha,
        "url": f"https://github.com/{full_name}/commit/{outcome.commit_sha}",
        "version": version,
        "commit_sha": outcome.commit_sha,
        "public_live": True,
        "observed_at": observed_at,
    }
    validate_target_evidence(
        evidence,
        channel="apt_rpm",
        state=outcome.state,
        expected_target=target,
        expected_version=version,
    )
    write_object(args.target_evidence, evidence)
    with args.github_output.open("a", encoding="utf-8") as output:
        output.write(f"state={outcome.state}\n")
        output.write(f"mutation_started={str(outcome.mutation_started).lower()}\n")
        output.write(f"remote_request_id={outcome.commit_sha}\n")
        output.write(f"observed_at={observed_at}\n")


if __name__ == "__main__":
    try:
        execute(parse_args())
    except (OSError, UnicodeDecodeError, ValueError) as error:
        print(f"publish-linux-repository: {error}", file=sys.stderr)
        raise SystemExit(1) from error
