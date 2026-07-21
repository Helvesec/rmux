#!/usr/bin/env python3
"""Build the exact, non-mutating rmux.io handoff payload."""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

from downstream_channels import file_hash, read_object, write_object
from downstream_payload import infer_file_mappings
from downstream_plan import validate_plan
from downstream_summary import validate_summary_for_plan


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--pre-site-summary", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def stage(args: argparse.Namespace) -> None:
    if args.plan.is_symlink() or args.pre_site_summary.is_symlink():
        raise ValueError("rmux.io payload inputs cannot be symlinks")
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise ValueError("rmux.io payload output must start absent")
    plan_path = args.plan.resolve(strict=True)
    summary_path = args.pre_site_summary.resolve(strict=True)
    plan = read_object(plan_path, "downstream channel plan")
    validate_plan(plan)
    summary = read_object(summary_path, "pre-site channel summary")
    validate_summary_for_plan(
        summary,
        plan=plan,
        plan_sha256=file_hash(plan_path),
        expected_phase="pre-site",
    )
    args.output_dir.mkdir()
    copied_summary = args.output_dir / "pre-site-channel-summary.json"
    shutil.copyfile(summary_path, copied_summary)
    update = {
        "schema_version": 1,
        "status": "manual-site-update-required",
        "automation_enabled": False,
        "source_git_sha": plan["source_git_sha"],
        "release_ref": plan["release"]["ref"],
        "pre_site_summary_sha256": file_hash(copied_summary),
        "advertised_channels": summary["advertised_channels"],
        "unresolved_channels": summary["unresolved_channels"],
    }
    write_object(args.output_dir / "rmux-io-update.json", update)
    infer_file_mappings(args.output_dir, "rmux_io")


if __name__ == "__main__":
    try:
        stage(parse_args())
    except (OSError, ValueError) as error:
        print(f"stage-rmux-io-payload: {error}", file=sys.stderr)
        raise SystemExit(1) from error
