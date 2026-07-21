#!/usr/bin/env python3
"""Resolve one exact channel request into an executable or typed no-op path."""

from __future__ import annotations

import argparse
import sys
from datetime import UTC, datetime
from pathlib import Path

from downstream_channels import (
    read_object,
    target_for_channel,
    validate_request,
    write_object,
)
from downstream_result import validate_target_evidence

TARGET_URLS = {
    "apt_rpm": "https://packages.rmux.io/",
    "chocolatey": "https://community.chocolatey.org/packages/rmux/",
    "crates_io": "https://crates.io/crates/rmux",
    "homebrew_core": "https://github.com/Homebrew/homebrew-core/pulls",
    "homebrew_tap": "https://github.com/Helvesec/homebrew-rmux/releases",
    "rmux_io": "https://rmux.io/",
    "scoop": "https://github.com/Helvesec/scoop-rmux/releases",
    "snap_candidate": "https://snapcraft.io/rmux",
    "snap_stable": "https://snapcraft.io/rmux",
    "web_share": "https://share.rmux.io/",
    "winget": "https://github.com/microsoft/winget-pkgs/pulls",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--channel", required=True)
    parser.add_argument("--target-evidence", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    return parser.parse_args()


def execute(args: argparse.Namespace) -> None:
    request = read_object(args.request, "downstream channel request")
    validate_request(request)
    if request["channel"] != args.channel:
        raise ValueError("channel execution request identity differs")
    decision = request["plan_entry"]["execution_decision"]
    execution_enabled = request["execution_authority"]
    if execution_enabled is not (decision == "enabled"):
        raise ValueError("channel execution authority differs from its plan entry")
    started_at = (
        datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    )
    target = target_for_channel(args.channel)
    repository = target["repository_full_name"]
    repository_name = repository.split("/", 1)[1] if isinstance(repository, str) else ""
    with args.github_output.open("a", encoding="utf-8") as output:
        output.write(f"execute={str(execution_enabled).lower()}\n")
        output.write(f"started_at={started_at}\n")
        output.write(f"repository_name={repository_name}\n")
    if execution_enabled:
        return
    if decision == "denied":
        state = "denied-by-policy"
    elif decision in {"blocked", "disarmed"}:
        state = "blocked"
    else:
        raise ValueError("non-executable channel decision is unknown")
    observed_at = (
        datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    )
    evidence = {
        "schema_version": 1,
        "channel": args.channel,
        "target_kind": target["target_kind"],
        "repository_id": target["repository_id"],
        "external_id": None,
        "url": TARGET_URLS[args.channel],
        "version": request["release"]["ref"].removeprefix("v"),
        "commit_sha": None,
        "public_live": False,
        "observed_at": observed_at,
    }
    validate_target_evidence(
        evidence,
        channel=args.channel,
        state=state,
        expected_target=target,
        expected_version=evidence["version"],
    )
    write_object(args.target_evidence, evidence)
    with args.github_output.open("a", encoding="utf-8") as output:
        output.write(f"state={state}\n")
        output.write("mutation_started=false\n")
        output.write("remote_request_id=\n")
        output.write(f"observed_at={observed_at}\n")


if __name__ == "__main__":
    try:
        execute(parse_args())
    except (OSError, ValueError) as error:
        print(f"channel-execution: {error}", file=sys.stderr)
        raise SystemExit(1) from error
