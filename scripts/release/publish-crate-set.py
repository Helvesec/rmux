#!/usr/bin/env python3
"""Publish and verify the exact canonical RMUX crate set in dependency order."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

from crate_package_set import file_hash, unpack, validate
from downstream_channels import target_for_channel, write_object
from downstream_result import validate_target_evidence

SOURCE_SHA = re.compile(r"[0-9a-f]{40}")
RELEASE_REF = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--payload-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--target-evidence", type=Path, required=True)
    parser.add_argument("--github-output", type=Path, required=True)
    return parser.parse_args()


def package_set_file(root: Path, version: str) -> Path:
    expected = root / f"rmux-{version}-crate-package-set.tar"
    files = sorted(root.iterdir()) if root.is_dir() else []
    if files != [expected] or expected.is_symlink() or not expected.is_file():
        raise ValueError("crates.io payload file set differs")
    return expected


def prepare_target_dir(path: Path) -> None:
    if path.exists() or path.is_symlink():
        raise ValueError("crates.io target directory must start absent")
    path.mkdir(parents=True)


def registry_url(name: str, version: str, *, download: bool = False) -> str:
    name = urllib.parse.quote(name, safe="")
    version = urllib.parse.quote(version, safe="")
    suffix = "/download" if download else ""
    return f"https://crates.io/api/v1/crates/{name}/{version}{suffix}"


def registry_version_exists(name: str, version: str) -> bool:
    request = urllib.request.Request(
        registry_url(name, version),
        headers={"User-Agent": "rmux-release-writer/1 (security@rmux.io)"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read(2 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise ValueError(f"crates.io lookup failed with HTTP {error.code}") from error
    if len(data) > 2 * 1024 * 1024:
        raise ValueError("crates.io metadata exceeds the release size limit")
    try:
        metadata = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("crates.io metadata is invalid") from error
    observed = metadata.get("version") if isinstance(metadata, dict) else None
    if (
        not isinstance(observed, dict)
        or observed.get("crate") != name
        or observed.get("num") != version
        or observed.get("dl_path") != f"/api/v1/crates/{name}/{version}/download"
    ):
        raise ValueError("crates.io metadata identity differs")
    return True


def registry_bytes(name: str, version: str) -> bytes | None:
    if not registry_version_exists(name, version):
        return None
    request = urllib.request.Request(
        registry_url(name, version, download=True),
        headers={"User-Agent": "rmux-release-writer/1 (security@rmux.io)"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read(16 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        raise ValueError(f"crates.io download failed with HTTP {error.code}") from error
    if len(data) > 16 * 1024 * 1024:
        raise ValueError("crates.io package exceeds the release size limit")
    return data


def run_cargo(args: list[str], *, token: str, target_dir: Path) -> None:
    environment = {
        **os.environ,
        "CARGO_REGISTRY_TOKEN": token,
        "CARGO_TARGET_DIR": str(target_dir),
    }
    completed = subprocess.run(
        ["cargo", *args],
        check=False,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip()[-4000:]
        raise ValueError(f"cargo {' '.join(args[:2])} failed: {detail}")


def generated_package_paths(target_dir: Path, filename: str) -> tuple[Path, Path]:
    package_dir = target_dir / "package"
    return package_dir / filename, package_dir / "tmp-crate" / filename


def remove_generated_package(target_dir: Path, filename: str) -> None:
    for path in generated_package_paths(target_dir, filename):
        path.unlink(missing_ok=True)


def generated_package(target_dir: Path, filename: str) -> Path:
    files = [
        path
        for path in generated_package_paths(target_dir, filename)
        if path.is_file() and not path.is_symlink()
    ]
    if len(files) != 1:
        raise ValueError(f"Cargo generated an unexpected package set: {filename}")
    return files[0]


def wait_for_exact(name: str, version: str, expected: Path) -> None:
    expected_hash = file_hash(expected)
    for attempt in range(19):
        current = registry_bytes(name, version)
        if current is not None:
            if hashlib.sha256(current).hexdigest() != expected_hash:
                raise ValueError(f"published crate bytes differ: {name}")
            return
        if attempt < 18:
            time.sleep(5)
    raise ValueError(f"published crate did not become visible: {name}")


def execute(args: argparse.Namespace) -> None:
    if (
        SOURCE_SHA.fullmatch(args.source_sha) is None
        or RELEASE_REF.fullmatch(args.release_ref) is None
    ):
        raise ValueError("crates.io release identity is malformed")
    version = args.release_ref.removeprefix("v")
    token = os.environ.get("CARGO_REGISTRY_TOKEN", "")
    if not token or "\n" in token or "\r" in token:
        raise ValueError("short-lived crates.io token is missing or malformed")
    package_set = package_set_file(args.payload_dir, version)
    prepare_target_dir(args.target_dir)
    extracted = args.target_dir / "canonical"
    manifest = unpack(package_set, extracted)
    packages = validate(
        manifest, extracted, source_sha=args.source_sha, version=version
    )
    cargo_target = args.target_dir / "cargo"
    mutated = False
    for package in packages:
        name = package["name"]
        canonical = extracted / "crates" / package["file"]
        existing = registry_bytes(name, version)
        if existing is not None:
            if hashlib.sha256(existing).hexdigest() != package["sha256"]:
                raise ValueError(f"existing crates.io bytes differ: {name}")
            continue
        remove_generated_package(cargo_target, package["file"])
        run_cargo(
            ["publish", "--dry-run", "--locked", "--package", name],
            token=token,
            target_dir=cargo_target,
        )
        if file_hash(generated_package(cargo_target, package["file"])) != file_hash(
            canonical
        ):
            raise ValueError(f"Cargo package bytes differ from the candidate: {name}")
        remove_generated_package(cargo_target, package["file"])
        run_cargo(
            ["publish", "--locked", "--package", name],
            token=token,
            target_dir=cargo_target,
        )
        if file_hash(generated_package(cargo_target, package["file"])) != file_hash(
            canonical
        ):
            raise ValueError(f"published Cargo package bytes changed: {name}")
        mutated = True
        wait_for_exact(name, version, canonical)
    for package in packages:
        wait_for_exact(package["name"], version, extracted / "crates" / package["file"])
    observed_at = (
        datetime.now(timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )
    state = "public-live" if mutated else "no-op-exact"
    external_id = f"crates.io:rmux@{version}"
    target = target_for_channel("crates_io")
    evidence = {
        "schema_version": 1,
        "channel": "crates_io",
        "target_kind": target["target_kind"],
        "repository_id": target["repository_id"],
        "external_id": external_id,
        "url": f"https://crates.io/crates/rmux/{version}",
        "version": version,
        "commit_sha": None,
        "public_live": True,
        "observed_at": observed_at,
    }
    validate_target_evidence(
        evidence,
        channel="crates_io",
        state=state,
        expected_target=target,
        expected_version=version,
    )
    write_object(args.target_evidence, evidence)
    with args.github_output.open("a", encoding="utf-8") as output:
        output.write(f"state={state}\n")
        output.write(f"mutation_started={str(mutated).lower()}\n")
        output.write(f"remote_request_id={external_id}\n")
        output.write(f"observed_at={observed_at}\n")


if __name__ == "__main__":
    try:
        execute(parse_args())
    except (OSError, ValueError) as error:
        print(f"publish-crate-set: {error}", file=sys.stderr)
        raise SystemExit(1) from error
