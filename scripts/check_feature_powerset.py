#!/usr/bin/env python3
"""Compile workspace features and the release-only tiny binary entrypoint."""
import argparse
from itertools import combinations
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def commands(metadata):
    members = metadata["workspace_members"]
    if not members or len(set(members)) != len(members):
        raise ValueError("empty or duplicate workspace members")
    packages = [p for p in metadata["packages"] if p["id"] in members]
    if (len(packages) != len(members) or {p["id"] for p in packages} != set(members)
            or len({p["name"] for p in packages}) != len(packages)):
        raise ValueError("incomplete or duplicate workspace packages")

    # Workspace checks alone may enable a member's defaults through a sibling.
    # Keep the acceptance command, then compile each package independently too.
    workspace = ["cargo", "check", "--workspace", "--all-targets", "--locked"]
    result = [workspace + ["--no-default-features"], workspace,
              workspace + ["--all-features"]]
    for package in sorted(packages, key=lambda p: p["name"]):
        features = sorted(set(package["features"]) - {"default"})
        prefix = ["cargo", "check", "-p", package["name"], "--all-targets", "--locked"]
        result.extend([prefix, prefix + ["--all-features"]])
        for size in range(len(features) + 1):
            for subset in combinations(features, size):
                result.append(prefix + ["--no-default-features"] +
                              (["--features", ",".join(subset)] if subset else []))
    # Debug/test targets do not select the tiny main() guarded by
    # not(debug_assertions). Check the actual release binary, both with the
    # packaged defaults and without them, in every caller of this shared gate.
    tiny = ["cargo", "check", "-p", "rmux", "--release", "--bin", "rmux",
            "--locked", "--features", "tiny-cli"]
    result.extend([tiny, tiny + ["--no-default-features"]])
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", help="Cargo target triple (default: native host)")
    args = parser.parse_args(argv)
    metadata = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    plan = commands(json.loads(metadata.stdout))
    for command in plan:
        if args.target:
            command = command + ["--target", args.target]
        print("[features] " + " ".join(command), flush=True)
        subprocess.run(command, cwd=ROOT, check=True)
    print(f"[features] PASS {len(plan)} compilation checks", flush=True)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode if error.returncode > 0 else 1)
