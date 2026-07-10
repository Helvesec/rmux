#!/usr/bin/env python3
"""Validate the release-facing RMUX perf baseline JSON."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


REQUIRED_TARGETS = {
    "attach_render",
    "source_file_large_corpus",
    "status_format_heavy",
    "hook_storm",
    "daemon_churn",
}
GIT_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
FINGERPRINT_RE = re.compile(r"^[0-9A-Za-z._:-]{8,128}$")


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


def validate(path: Path, expected_platform: str | None) -> int:
    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)

    if payload.get("schema") != 2:
        return fail(f"{path}: expected schema 2")
    if payload.get("kind") != "rmux-perf-baseline":
        return fail(f"{path}: expected kind rmux-perf-baseline")
    if payload.get("release") != "release-0.9.0":
        return fail(f"{path}: expected release release-0.9.0")

    source = payload.get("source")
    if not isinstance(source, dict):
        return fail(f"{path}: missing source artifact")
    source_metrics = source.get("metrics")
    if not isinstance(source_metrics, list):
        return fail(f"{path}: missing source.metrics")
    metric_names = {
        str(metric.get("name"))
        for metric in source_metrics
        if isinstance(metric, dict) and metric.get("name")
    }

    targets = payload.get("required_targets")
    if not isinstance(targets, list):
        return fail(f"{path}: missing required_targets")
    targets_by_name = {
        str(target.get("name")): target
        for target in targets
        if isinstance(target, dict) and target.get("name")
    }

    missing_targets = sorted(REQUIRED_TARGETS.difference(targets_by_name))
    if missing_targets:
        return fail(f"{path}: missing Lot 9 target(s): {', '.join(missing_targets)}")

    for name in sorted(REQUIRED_TARGETS):
        target = targets_by_name[name]
        if target.get("status") != "collected":
            return fail(f"{path}: target {name} is not collected")
        source_metric = str(target.get("source_metric", ""))
        if source_metric not in metric_names:
            return fail(
                f"{path}: target {name} points to missing source metric {source_metric!r}"
            )

    versions = payload.get("versions")
    if not isinstance(versions, dict) or not versions.get("rmux") or not versions.get("tmux"):
        return fail(f"{path}: missing rmux/tmux version stamp")
    git = payload.get("git")
    if not isinstance(git, dict):
        return fail(f"{path}: missing git stamp")
    commit = str(git.get("commit", "")).lower()
    if GIT_SHA_RE.fullmatch(commit) is None:
        return fail(f"{path}: missing or invalid full git commit stamp")
    describe = str(git.get("describe", ""))
    if not describe:
        return fail(f"{path}: missing git describe stamp")
    if describe.endswith("-dirty") or git.get("dirty") is True:
        return fail(f"{path}: baseline was recorded from a dirty worktree")
    if expected_platform == "darwin" and git.get("dirty") is not False:
        return fail(f"{path}: Darwin baseline is missing an explicit clean-worktree stamp")
    environment = payload.get("environment")
    if not isinstance(environment, dict):
        return fail(f"{path}: missing environment stamp")
    platform = str(environment.get("platform", "")).lower()
    machine = str(environment.get("machine", "")).lower()
    fingerprint = str(environment.get("host_fingerprint", ""))
    if not platform or not machine:
        return fail(f"{path}: missing environment platform/machine stamp")
    if expected_platform is not None and platform != expected_platform:
        return fail(
            f"{path}: baseline platform {platform!r} does not match expected {expected_platform!r}"
        )
    if FINGERPRINT_RE.fullmatch(fingerprint) is None:
        return fail(f"{path}: missing or invalid environment.host_fingerprint")
    source_platform = str(source.get("platform", "")).lower()
    source_environment = source.get("environment")
    if isinstance(source_environment, dict) and source_environment.get("platform"):
        source_platform = str(source_environment["platform"]).lower()
    if source_platform != platform:
        return fail(
            f"{path}: source platform {source_platform or '<missing>'!r} does not match baseline {platform!r}"
        )

    print(f"perf-baseline={path} targets={len(REQUIRED_TARGETS)}")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("--expected-platform", choices=("darwin", "linux"))
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv[1:])
    return validate(args.baseline, args.expected_platform)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
