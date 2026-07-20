#!/usr/bin/env python3
"""Create or verify the immutable record for one canonical native bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CONTRACT = ROOT / ".github/release/canonical-build-contract.json"
SAFE_PATH = re.compile(r"[A-Za-z0-9._+@=-]+(?:/[A-Za-z0-9._+@=-]+)*")
SHA = re.compile(r"[0-9a-f]{40}")
INTENT = re.compile(r"[A-Za-z0-9._:-]{8,128}")
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")
RECORD_SHA = re.compile(r"[0-9a-f]{64}")


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON: {path}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValueError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def load_contract(path: Path) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    contract = read_object(path, "canonical build contract")
    exact_keys(
        contract,
        {
            "schema_version",
            "status",
            "repository_id",
            "workflow",
            "reusable_workflow",
            "rust_toolchain",
            "artifact_retention_days",
            "build_policy",
            "platforms",
        },
        "canonical build contract",
    )
    if contract["schema_version"] != 1:
        raise ValueError("unsupported canonical build contract schema")
    if contract["status"] != "review-only-non-publishing":
        raise ValueError("canonical build contract is not review-only")
    if contract["repository_id"] != 1239918790:
        raise ValueError("canonical build repository ID changed")
    if contract["rust_toolchain"] != "1.96.1":
        raise ValueError("canonical build toolchain changed")
    if contract["artifact_retention_days"] != 7:
        raise ValueError("canonical artifact retention must be seven days")
    expected_policy = {
        "cargo_incremental": False,
        "cargo_locked": True,
        "fresh_target": True,
        "object_cache_restored": False,
        "publication_authority": False,
    }
    if contract["build_policy"] != expected_policy:
        raise ValueError("canonical build policy changed")
    platforms = contract["platforms"]
    if not isinstance(platforms, list) or len(platforms) != 5:
        raise ValueError("canonical build contract must contain five platforms")
    by_key: dict[str, dict[str, Any]] = {}
    for platform in platforms:
        if not isinstance(platform, dict):
            raise ValueError("canonical platform must be an object")
        exact_keys(
            platform,
            {
                "key",
                "runner_image",
                "runner_os",
                "runner_arch",
                "target_triple",
                "archive_format",
                "linux_packages",
                "supplemental_roles",
            },
            "canonical platform",
        )
        key = platform["key"]
        if not isinstance(key, str) or key in by_key:
            raise ValueError("canonical platform keys must be unique strings")
        by_key[key] = platform
    return contract, by_key


def asset_role(relative: str, platform: dict[str, Any]) -> str:
    if relative == "SHA256SUMS.txt":
        return "checksums"
    archive_suffix = f".{platform['archive_format']}"
    if relative.endswith(archive_suffix):
        return "archive"
    if platform["linux_packages"] and relative.endswith(".deb"):
        return "debian"
    if platform["linux_packages"] and relative.endswith(".rpm"):
        return "rpm"
    if relative.endswith(".snap"):
        return {
            "linux-x86_64": "snap-amd64",
            "linux-aarch64": "snap-arm64",
        }.get(platform["key"], "invalid-snap-platform")
    if relative.startswith("rmux-web-crypto-wasm-") and relative.endswith(".tar"):
        return "wasm-byte-set"
    if relative.startswith("rmux-web-crypto-wasm-") and relative.endswith(
        ".provenance.json"
    ):
        return "wasm-provenance"
    if relative.startswith("rmux-") and relative.endswith("-crate-package-set.tar"):
        return "crate-package-set"
    if relative.startswith("rmux.") and relative.endswith(".nupkg"):
        return "chocolatey-package"
    raise ValueError(f"canonical asset has no contracted role: {relative}")


def verify_asset_roles(files: list[dict[str, Any]], platform: dict[str, Any]) -> None:
    expected = {"archive", "checksums"}
    if platform["linux_packages"]:
        expected.update({"debian", "rpm"})
    supplemental = platform.get("supplemental_roles")
    if not isinstance(supplemental, list) or any(
        not isinstance(role, str) for role in supplemental
    ):
        raise ValueError("canonical supplemental role contract is invalid")
    expected.update(supplemental)
    roles = [item["role"] for item in files]
    if set(roles) != expected or len(roles) != len(expected):
        raise ValueError(
            f"canonical asset roles differ: expected={sorted(expected)}, "
            f"actual={sorted(roles)}"
        )


def verify_checksums(directory: Path, files: list[dict[str, Any]]) -> None:
    checksums = directory / "SHA256SUMS.txt"
    try:
        lines = checksums.read_text(encoding="ascii").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        raise ValueError("canonical checksum manifest is not ASCII") from error
    expected = {
        item["path"]: item["sha256"] for item in files if item["role"] != "checksums"
    }
    actual: dict[str, str] = {}
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64})  (\S+)", line)
        if match is None or SAFE_PATH.fullmatch(match.group(2)) is None:
            raise ValueError("canonical checksum manifest line is invalid")
        path = match.group(2)
        if path in actual:
            raise ValueError(f"duplicate canonical checksum entry: {path}")
        actual[path] = match.group(1)
    if actual != expected:
        raise ValueError(
            "canonical checksum manifest does not cover the exact asset set"
        )


def asset_files(directory: Path, platform: dict[str, Any]) -> list[dict[str, Any]]:
    if directory.is_symlink():
        raise ValueError("assets path must be one real directory")
    root = directory.resolve(strict=True)
    if not root.is_dir():
        raise ValueError("assets path must be one real directory")
    files: list[dict[str, Any]] = []
    paths = sorted(
        root.rglob("*"),
        key=lambda path: path.relative_to(root).as_posix().encode("utf-8"),
    )
    for path in paths:
        if path.is_symlink():
            raise ValueError(f"canonical assets cannot contain symlinks: {path}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError(f"canonical asset is not a regular file: {path}")
        relative = path.relative_to(root).as_posix()
        if SAFE_PATH.fullmatch(relative) is None or ".." in relative.split("/"):
            raise ValueError(f"canonical asset path is not portable: {relative}")
        size = path.stat().st_size
        if size <= 0:
            raise ValueError(f"canonical asset is empty: {relative}")
        files.append(
            {
                "path": relative,
                "role": asset_role(relative, platform),
                "size": size,
                "sha256": sha256_file(path),
            }
        )
    if not files:
        raise ValueError("canonical bundle has no assets")
    verify_asset_roles(files, platform)
    verify_checksums(root, files)
    return files


def validate_identity(args: argparse.Namespace) -> None:
    if SHA.fullmatch(args.source_sha) is None:
        raise ValueError("source SHA must be 40 lowercase hexadecimal characters")
    if args.fast_run_id <= 0 or args.candidate_run_id <= 0:
        raise ValueError("run IDs must be positive integers")
    if args.candidate_run_attempt != 1:
        raise ValueError("canonical candidate must use Actions attempt 1")
    if INTENT.fullmatch(args.release_intent_id) is None:
        raise ValueError("release intent ID uses characters outside the allowlist")
    if TAG.fullmatch(args.planned_release_ref) is None:
        raise ValueError("planned release ref is not canonical")
    is_rc = "-rc." in args.planned_release_ref
    if args.release_kind == "stable" and is_rc:
        raise ValueError("stable candidate cannot use an RC ref")
    if args.release_kind == "rc" and not is_rc:
        raise ValueError("RC candidate requires an RC ref")


def rustc_identity(
    path: Path, expected_host: str, expected_release: str
) -> dict[str, str]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        raise ValueError("rustc verbose evidence is not UTF-8") from error
    fields: dict[str, str] = {}
    for line in lines:
        if ": " not in line:
            continue
        key, value = line.split(": ", 1)
        if key in fields:
            raise ValueError(f"duplicate rustc verbose field: {key}")
        fields[key] = value
    if fields.get("host") != expected_host:
        raise ValueError("rustc host does not match the canonical target")
    if fields.get("release") != expected_release:
        raise ValueError("rustc release does not match the canonical toolchain")
    commit = fields.get("commit-hash", "")
    if re.fullmatch(r"[0-9a-f]{40}", commit) is None:
        raise ValueError("rustc commit hash is missing or non-canonical")
    return {"host": expected_host, "release": expected_release, "commit_hash": commit}


def create(args: argparse.Namespace) -> str:
    contract, platforms = load_contract(args.contract)
    validate_identity(args)
    platform = platforms.get(args.platform_key)
    if platform is None:
        raise ValueError(f"unknown canonical platform: {args.platform_key}")
    actual_runner = {
        "image": args.runner_image,
        "os": args.runner_os,
        "arch": args.runner_arch,
        "environment": args.runner_environment,
    }
    expected_runner = {
        "image": platform["runner_image"],
        "os": platform["runner_os"],
        "arch": platform["runner_arch"],
        "environment": "github-hosted",
    }
    if actual_runner != expected_runner:
        raise ValueError(
            f"runner identity mismatch: expected {expected_runner}, got {actual_runner}"
        )
    if args.rustc_verbose.is_symlink():
        raise ValueError("rustc verbose evidence must be one regular file")
    rustc_path = args.rustc_verbose.resolve(strict=True)
    if not rustc_path.is_file():
        raise ValueError("rustc verbose evidence must be one regular file")
    rustc = rustc_identity(
        rustc_path, platform["target_triple"], contract["rust_toolchain"]
    )
    created_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    record = {
        "schema_version": 1,
        "repository_id": contract["repository_id"],
        "source_git_sha": args.source_sha,
        "fast_run_id": args.fast_run_id,
        "candidate_run_id": args.candidate_run_id,
        "candidate_run_attempt": args.candidate_run_attempt,
        "release_intent_id": args.release_intent_id,
        "planned_release_ref": args.planned_release_ref,
        "release_kind": args.release_kind,
        "platform": {
            "key": platform["key"],
            "target_triple": platform["target_triple"],
            "archive_format": platform["archive_format"],
        },
        "runner": actual_runner,
        "toolchain": {
            "requested": contract["rust_toolchain"],
            **rustc,
            "rustc_verbose_sha256": sha256_file(rustc_path),
        },
        "build_policy": contract["build_policy"],
        "created_at": created_at,
        "files": asset_files(args.assets_dir, platform),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    digest = sha256_file(args.output)
    print(digest)
    return digest


def verify_record_shape(
    record: dict[str, Any], contract: dict[str, Any], platform: dict[str, Any]
) -> None:
    exact_keys(
        record,
        {
            "schema_version",
            "repository_id",
            "source_git_sha",
            "fast_run_id",
            "candidate_run_id",
            "candidate_run_attempt",
            "release_intent_id",
            "planned_release_ref",
            "release_kind",
            "platform",
            "runner",
            "toolchain",
            "build_policy",
            "created_at",
            "files",
        },
        "canonical build record",
    )
    if record["schema_version"] != 1 or record["repository_id"] != 1239918790:
        raise ValueError("canonical build record identity changed")
    if record["candidate_run_attempt"] != 1:
        raise ValueError("canonical build record is not attempt 1")
    expected_platform = {
        "key": platform["key"],
        "target_triple": platform["target_triple"],
        "archive_format": platform["archive_format"],
    }
    if record["platform"] != expected_platform:
        raise ValueError("canonical platform record does not match its contract")
    expected_runner = {
        "image": platform["runner_image"],
        "os": platform["runner_os"],
        "arch": platform["runner_arch"],
        "environment": "github-hosted",
    }
    if record["runner"] != expected_runner:
        raise ValueError("canonical runner record does not match its contract")
    if record["build_policy"] != contract["build_policy"]:
        raise ValueError("canonical build policy record changed")
    toolchain = record.get("toolchain")
    if not isinstance(toolchain, dict):
        raise ValueError("canonical toolchain record is missing")
    exact_keys(
        toolchain,
        {"requested", "release", "host", "commit_hash", "rustc_verbose_sha256"},
        "toolchain",
    )
    if toolchain["requested"] != contract["rust_toolchain"]:
        raise ValueError("canonical toolchain request changed")
    if toolchain["release"] != contract["rust_toolchain"]:
        raise ValueError("canonical toolchain release changed")
    if toolchain["host"] != platform["target_triple"]:
        raise ValueError("canonical toolchain host changed")
    if not isinstance(toolchain["commit_hash"], str) or not re.fullmatch(
        r"[0-9a-f]{40}", toolchain["commit_hash"]
    ):
        raise ValueError("canonical toolchain commit hash is invalid")
    if not isinstance(toolchain["rustc_verbose_sha256"], str) or not re.fullmatch(
        r"[0-9a-f]{64}", toolchain["rustc_verbose_sha256"]
    ):
        raise ValueError("rustc verbose digest is invalid")
    try:
        parsed = datetime.fromisoformat(record["created_at"].replace("Z", "+00:00"))
    except (AttributeError, ValueError) as error:
        raise ValueError("canonical build timestamp is invalid") from error
    if parsed.tzinfo is None:
        raise ValueError("canonical build timestamp must include a timezone")


def verify(args: argparse.Namespace) -> str:
    if RECORD_SHA.fullmatch(args.expected_record_sha256) is None:
        raise ValueError("expected canonical build record digest is invalid")
    initial_digest = sha256_file(args.record)
    if initial_digest != args.expected_record_sha256:
        raise ValueError("canonical build record digest differs from build output")
    contract, platforms = load_contract(args.contract)
    platform = platforms.get(args.platform_key)
    if platform is None:
        raise ValueError(f"unknown canonical platform: {args.platform_key}")
    record = read_object(args.record, "canonical build record")
    verify_record_shape(record, contract, platform)
    expected = {
        "source_git_sha": args.source_sha,
        "fast_run_id": args.fast_run_id,
        "candidate_run_id": args.candidate_run_id,
        "release_intent_id": args.release_intent_id,
        "planned_release_ref": args.planned_release_ref,
        "release_kind": args.release_kind,
    }
    for field, value in expected.items():
        if record.get(field) != value:
            raise ValueError(f"canonical build record {field} mismatch")
    recorded_files = record.get("files")
    if not isinstance(recorded_files, list):
        raise ValueError("canonical build record files must be an array")
    actual_files = asset_files(args.assets_dir, platform)
    if recorded_files != actual_files:
        raise ValueError("downloaded canonical asset set or digest changed")
    digest = sha256_file(args.record)
    if digest != args.expected_record_sha256:
        raise ValueError("canonical build record changed during smoke verification")
    print(digest)
    return digest


def write_subjects(args: argparse.Namespace) -> None:
    record = read_object(args.record, "canonical build record")
    recorded_files = record.get("files")
    if not isinstance(recorded_files, list):
        raise ValueError("canonical build record files must be an array")
    _, platforms = load_contract(args.contract)
    platform_record = record.get("platform")
    if not isinstance(platform_record, dict):
        raise ValueError("canonical build record platform is missing")
    platform = platforms.get(platform_record.get("key"))
    if platform is None:
        raise ValueError("canonical build record platform is unknown")
    actual_files = asset_files(args.assets_dir, platform)
    if recorded_files != actual_files:
        raise ValueError("canonical build subjects differ from the build record")
    lines = [f"{item['sha256']}  assets/{item['path']}" for item in actual_files]
    lines.append(f"{sha256_file(args.record)}  canonical-build-record.json")
    args.output.write_text("\n".join(sorted(lines)) + "\n", encoding="ascii")


def write_checksums(args: argparse.Namespace) -> None:
    root = args.assets_dir.resolve(strict=True)
    if args.assets_dir.is_symlink() or not root.is_dir():
        raise ValueError("assets path must be one real directory")
    output = root / "SHA256SUMS.txt"
    if output.is_symlink() or (output.exists() and not output.is_file()):
        raise ValueError("canonical checksum manifest must be a regular file")
    lines: list[str] = []
    for path in sorted(
        root.rglob("*"), key=lambda item: item.relative_to(root).as_posix()
    ):
        if path.is_symlink():
            raise ValueError("canonical assets cannot contain symlinks")
        if path.is_dir():
            continue
        relative = path.relative_to(root).as_posix()
        if relative == "SHA256SUMS.txt":
            continue
        if not path.is_file() or SAFE_PATH.fullmatch(relative) is None:
            raise ValueError(f"canonical asset path is invalid: {relative}")
        if path.stat().st_size <= 0:
            raise ValueError(f"canonical asset is empty: {relative}")
        lines.append(f"{sha256_file(path)}  {relative}")
    if not lines:
        raise ValueError("canonical bundle has no checksum subjects")
    output.write_text("\n".join(lines) + "\n", encoding="ascii")


def common_identity(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--fast-run-id", type=int, required=True)
    parser.add_argument("--candidate-run-id", type=int, required=True)
    parser.add_argument("--release-intent-id", required=True)
    parser.add_argument("--planned-release-ref", required=True)
    parser.add_argument(
        "--release-kind", choices=("shadow", "rc", "stable"), required=True
    )
    parser.add_argument("--platform-key", required=True)
    parser.add_argument("--assets-dir", type=Path, required=True)
    parser.add_argument("--contract", type=Path, default=CONTRACT)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    create_parser = commands.add_parser("create")
    common_identity(create_parser)
    create_parser.add_argument("--candidate-run-attempt", type=int, required=True)
    create_parser.add_argument("--runner-image", required=True)
    create_parser.add_argument("--runner-os", required=True)
    create_parser.add_argument("--runner-arch", required=True)
    create_parser.add_argument("--runner-environment", required=True)
    create_parser.add_argument("--rustc-verbose", type=Path, required=True)
    create_parser.add_argument("--output", type=Path, required=True)
    verify_parser = commands.add_parser("verify")
    common_identity(verify_parser)
    verify_parser.add_argument("--record", type=Path, required=True)
    verify_parser.add_argument("--expected-record-sha256", required=True)
    subjects_parser = commands.add_parser("subjects")
    subjects_parser.add_argument("--record", type=Path, required=True)
    subjects_parser.add_argument("--assets-dir", type=Path, required=True)
    subjects_parser.add_argument("--output", type=Path, required=True)
    subjects_parser.add_argument("--contract", type=Path, default=CONTRACT)
    checksums_parser = commands.add_parser("checksums")
    checksums_parser.add_argument("--assets-dir", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command == "create":
        create(args)
    elif args.command == "verify":
        verify(args)
    elif args.command == "subjects":
        write_subjects(args)
    else:
        write_checksums(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
