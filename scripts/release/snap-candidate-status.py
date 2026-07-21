#!/usr/bin/env python3
"""Inspect and verify exact RMUX Snap candidate revisions."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from downstream_channels import target_for_channel, write_object
from downstream_result import validate_target_evidence

RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
ARCHITECTURES = ("amd64", "arm64")
INFO_URL = "https://api.snapcraft.io/v2/snaps/info/rmux"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("inspect", "verify"))
    parser.add_argument("--payload-dir", type=Path, required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    parser.add_argument("--target-evidence", type=Path)
    parser.add_argument("--mutation-started", action="store_true")
    return parser.parse_args()


def package_version(release_ref: str) -> str:
    if RELEASE_REF.fullmatch(release_ref) is None:
        raise ValueError("Snap candidate release ref is malformed")
    release_version = release_ref.removeprefix("v")
    if "-rc." not in release_version:
        return release_version
    version, rc_number = release_version.rsplit("-rc.", maxsplit=1)
    if not rc_number.isdigit() or rc_number.startswith("0"):
        raise ValueError("Snap candidate RC number is not canonical")
    return version


def payloads(root: Path, version: str) -> dict[str, Path]:
    if root.is_symlink() or not root.is_dir():
        raise ValueError("Snap candidate payload must be one real directory")
    result = {
        architecture: root / f"rmux-{version}-snap-{architecture}.snap"
        for architecture in ARCHITECTURES
    }
    entries = sorted(root.iterdir(), key=lambda path: path.name)
    actual = [path.name for path in entries]
    if actual != sorted(path.name for path in result.values()):
        raise ValueError("Snap candidate payload file set differs")
    if any(
        path.is_symlink() or not path.is_file() or path.stat().st_size <= 0
        for path in entries
    ):
        raise ValueError("Snap candidate payload contains an invalid package")
    return result


def sha3(path: Path) -> str:
    digest = hashlib.sha3_384()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def channel_map() -> list[dict[str, Any]]:
    request = urllib.request.Request(
        INFO_URL,
        headers={
            "Accept": "application/json",
            "Snap-Device-Series": "16",
            "User-Agent": "rmux-release-writer/1 (security@rmux.io)",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read(4 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return []
        raise ValueError(f"Snap Store lookup failed with HTTP {error.code}") from error
    if len(raw) > 4 * 1024 * 1024:
        raise ValueError("Snap Store response exceeds the release limit")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("Snap Store returned invalid JSON") from error
    entries = value.get("channel-map") if isinstance(value, dict) else None
    if not isinstance(entries, list):
        raise ValueError("Snap Store response has no channel map")
    return [entry for entry in entries if isinstance(entry, dict)]


def exact_status(files: dict[str, Path], version: str) -> dict[str, bool]:
    status = dict.fromkeys(ARCHITECTURES, False)
    for entry in channel_map():
        channel = entry.get("channel")
        download = entry.get("download")
        if not isinstance(channel, dict) or not isinstance(download, dict):
            continue
        architecture = channel.get("architecture")
        if (
            architecture in status
            and channel.get("name") == "candidate"
            and entry.get("version") == version
            and download.get("sha3-384") == sha3(files[architecture])
        ):
            status[architecture] = True
    return status


def append_outputs(path: Path, values: dict[str, str]) -> None:
    with path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            output.write(f"{key}={value}\n")


def execute(args: argparse.Namespace) -> None:
    version = package_version(args.release_ref)
    files = payloads(args.payload_dir, version)
    status = exact_status(files, version)
    if args.command == "inspect":
        append_outputs(
            args.github_output,
            {
                **{
                    architecture: str(files[architecture])
                    for architecture in ARCHITECTURES
                },
                **{
                    f"upload_{architecture}": str(not status[architecture]).lower()
                    for architecture in ARCHITECTURES
                },
                "all_exact": str(all(status.values())).lower(),
            },
        )
        return
    if args.target_evidence is None:
        raise ValueError("Snap verification requires a target evidence output")
    for attempt in range(19):
        status = exact_status(files, version)
        if all(status.values()):
            break
        if attempt < 18:
            time.sleep(5)
    if not all(status.values()):
        raise ValueError("exact Snap candidate revisions did not become visible")
    observed_at = (
        datetime.now(timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )
    state = "public-live" if args.mutation_started else "no-op-exact"
    external_id = f"snapcraft:rmux@{version}:candidate"
    target = target_for_channel("snap_candidate")
    evidence = {
        "schema_version": 1,
        "channel": "snap_candidate",
        "target_kind": target["target_kind"],
        "repository_id": target["repository_id"],
        "external_id": external_id,
        "url": "https://snapcraft.io/rmux",
        "version": version,
        "commit_sha": None,
        "public_live": True,
        "observed_at": observed_at,
    }
    validate_target_evidence(
        evidence,
        channel="snap_candidate",
        state=state,
        expected_target=target,
        expected_version=version,
    )
    write_object(args.target_evidence, evidence)
    append_outputs(
        args.github_output,
        {
            "state": state,
            "mutation_started": str(args.mutation_started).lower(),
            "remote_request_id": external_id,
            "observed_at": observed_at,
        },
    )


if __name__ == "__main__":
    try:
        execute(parse_args())
    except (OSError, ValueError) as error:
        print(f"snap-candidate-status: {error}", file=sys.stderr)
        raise SystemExit(1) from error
