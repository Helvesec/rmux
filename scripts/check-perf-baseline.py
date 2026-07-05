#!/usr/bin/env python3
"""Validate the release-facing RMUX perf baseline JSON."""

from __future__ import annotations

import json
import sys
from pathlib import Path


REQUIRED_TARGETS = {
    "attach_render",
    "source_file_large_corpus",
    "status_format_heavy",
    "hook_storm",
    "daemon_churn",
}


def fail(message: str) -> int:
    print(f"error: {message}", file=sys.stderr)
    return 1


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: scripts/check-perf-baseline.py BASELINE_JSON", file=sys.stderr)
        return 2

    path = Path(argv[1])
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
    if not isinstance(git, dict) or not git.get("commit"):
        return fail(f"{path}: missing git commit stamp")
    environment = payload.get("environment")
    if not isinstance(environment, dict) or not environment.get("platform"):
        return fail(f"{path}: missing environment stamp")

    print(f"perf-baseline={path} targets={len(REQUIRED_TARGETS)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
