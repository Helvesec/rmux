#!/usr/bin/env python3
"""Resolve and verify the exact artifact set for one canonical candidate."""

from __future__ import annotations
import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

from candidate_artifact_resolution import (
    DIGEST,
    PLATFORMS,
    REPOSITORY,
    REPOSITORY_ID,
    ResolutionRequest,
    exact_keys,
    load_candidate_resolution,
    require_equal,
    resolve_candidate_artifacts,
)

ROOT = Path(__file__).resolve().parents[2]
SHA = re.compile(r"[0-9a-f]{40}")
INTENT = re.compile(r"[A-Za-z0-9._:-]{8,128}")
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def write_output(path: Path, value: dict[str, Any]) -> None:
    if path.exists() or path.is_symlink():
        raise ValueError(f"refusing to overwrite output metadata: {path}")
    if not path.parent.is_dir():
        raise ValueError(f"output metadata parent does not exist: {path.parent}")
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def exact_entries(directory: Path, expected: set[str], label: str) -> None:
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError(f"{label} must be one real directory")
    actual = {entry.name for entry in directory.iterdir()}
    if actual != expected:
        raise ValueError(
            f"{label} entries differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )
    for entry in directory.iterdir():
        if entry.is_symlink():
            raise ValueError(f"{label} cannot contain symlinks: {entry.name}")


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not valid UTF-8 JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def run_verifier(script: str, arguments: list[str]) -> str:
    completed = subprocess.run(
        [sys.executable, str(ROOT / "scripts/release" / script), *arguments],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise ValueError(
            f"{script} rejected candidate evidence: {completed.stderr.strip() or completed.stdout.strip()}"
        )
    return completed.stdout.strip()


def verify_fast_directory(
    directory: Path,
    source_sha: str,
    fast_run_id: int,
    release_intent_id: str,
    planned_release_ref: str,
    release_kind: str,
) -> dict[str, Any]:
    exact_entries(
        directory,
        {"candidate-intent.json", "fast-nextest-artifact.json", "fast-proof.json"},
        "fast proof artifact",
    )
    intent = read_object(directory / "candidate-intent.json", "candidate intent")
    release_version = planned_release_ref.removeprefix("v")
    package_version = release_version.split("-rc.", maxsplit=1)[0]
    expected_intent = {
        "schema_version": 1,
        "repository_id": REPOSITORY_ID,
        "source_git_sha": source_sha,
        "fast_run_id": fast_run_id,
        "release_intent_id": release_intent_id,
        "planned_release_ref": planned_release_ref,
        "release_kind": release_kind,
        "release_version": release_version,
        "package_version": package_version,
        "is_prerelease": "-rc." in planned_release_ref,
        "candidate_run_attempt": 1,
    }
    require_equal(intent, expected_intent, "candidate intent")
    proof = read_object(directory / "fast-proof.json", "fast run proof")
    proof_hash = proof.get("proof_sha256")
    if (
        not isinstance(proof_hash, str)
        or re.fullmatch(r"[0-9a-f]{64}", proof_hash) is None
    ):
        raise ValueError("fast proof digest is not canonical")
    unhashed = {key: value for key, value in proof.items() if key != "proof_sha256"}
    require_equal(canonical_sha256(unhashed), proof_hash, "fast proof digest")
    for field, expected in {
        "schema_version": 1,
        "kind": "fast",
        "repository_id": REPOSITORY_ID,
        "run_id": fast_run_id,
        "run_attempt": 1,
        "source_git_sha": source_sha,
        "test_fixture": False,
    }.items():
        require_equal(proof.get(field), expected, f"fast proof {field}")
    nextest = read_object(
        directory / "fast-nextest-artifact.json", "fast Nextest artifact"
    )
    exact_keys(
        nextest,
        {"artifact_id", "name", "digest", "size_in_bytes", "run_id", "source_git_sha"},
        "fast Nextest artifact",
    )
    require_equal(nextest["run_id"], fast_run_id, "fast Nextest run ID")
    require_equal(nextest["source_git_sha"], source_sha, "fast Nextest source SHA")
    require_equal(
        nextest["name"], f"rmux-windows-nextest-{source_sha}", "fast Nextest name"
    )
    if type(nextest["artifact_id"]) is not int or nextest["artifact_id"] <= 0:
        raise ValueError("fast Nextest artifact ID is invalid")
    if (
        not isinstance(nextest["digest"], str)
        or DIGEST.fullmatch(nextest["digest"]) is None
    ):
        raise ValueError("fast Nextest artifact digest is invalid")
    if type(nextest["size_in_bytes"]) is not int or nextest["size_in_bytes"] <= 0:
        raise ValueError("fast Nextest artifact size is invalid")
    return {"proof_sha256": proof_hash, "nextest_artifact": nextest}


def verify_platform_download(
    root: Path,
    assets_meta: dict[str, Any],
    provenance_meta: dict[str, Any],
    source_sha: str,
    fast_run_id: int,
    candidate_run_id: int,
    release_intent_id: str,
    planned_release_ref: str,
    release_kind: str,
) -> dict[str, Any]:
    platform = assets_meta["platform_key"]
    assets_dir = root / assets_meta["name"]
    provenance_dir = root / provenance_meta["name"]
    exact_entries(
        assets_dir, {"assets", "canonical-build-record.json"}, f"{platform} assets"
    )
    exact_entries(
        provenance_dir,
        {
            "build-provenance.sigstore.json",
            "canonical-artifact-binding.json",
            "canonical-build-record.json",
            "rustc-vV.txt",
        },
        f"{platform} provenance",
    )
    record = assets_dir / "canonical-build-record.json"
    provenance_record = provenance_dir / "canonical-build-record.json"
    if record.read_bytes() != provenance_record.read_bytes():
        raise ValueError(f"{platform} provenance contains a different build record")
    binding_path = provenance_dir / "canonical-artifact-binding.json"
    binding = read_object(binding_path, f"{platform} artifact binding")
    expected_record_sha = binding.get("build_record_sha256")
    if (
        not isinstance(expected_record_sha, str)
        or DIGEST.fullmatch(f"sha256:{expected_record_sha}") is None
    ):
        raise ValueError(f"{platform} artifact binding has no canonical record digest")
    record_sha = run_verifier(
        "canonical-build-record.py",
        [
            "verify",
            "--source-sha",
            source_sha,
            "--fast-run-id",
            str(fast_run_id),
            "--candidate-run-id",
            str(candidate_run_id),
            "--release-intent-id",
            release_intent_id,
            "--planned-release-ref",
            planned_release_ref,
            "--release-kind",
            release_kind,
            "--platform-key",
            platform,
            "--assets-dir",
            str(assets_dir / "assets"),
            "--record",
            str(record),
            "--expected-record-sha256",
            expected_record_sha,
        ],
    )
    attestation = binding.get("attestation")
    if not isinstance(attestation, dict):
        raise ValueError(f"{platform} artifact binding has no attestation")
    attestation_id = attestation.get("attestation_id")
    if not isinstance(attestation_id, str) or not attestation_id:
        raise ValueError(f"{platform} artifact binding has no attestation ID")
    run_verifier(
        "canonical-artifact-binding.py",
        [
            "verify",
            "--source-sha",
            source_sha,
            "--candidate-run-id",
            str(candidate_run_id),
            "--platform-key",
            platform,
            "--assets-artifact-id",
            str(assets_meta["artifact_id"]),
            "--assets-artifact-name",
            assets_meta["name"],
            "--assets-artifact-digest",
            assets_meta["archive_digest"],
            "--build-record",
            str(provenance_record),
            "--build-record-sha256",
            record_sha,
            "--attestation-id",
            attestation_id,
            "--attestation-bundle",
            str(provenance_dir / "build-provenance.sigstore.json"),
            "--binding",
            str(binding_path),
        ],
    )
    record_value = read_object(record, f"{platform} build record")
    toolchain = record_value.get("toolchain")
    if not isinstance(toolchain, dict):
        raise ValueError(f"{platform} build record has no toolchain")
    require_equal(
        sha256_file(provenance_dir / "rustc-vV.txt"),
        toolchain.get("rustc_verbose_sha256"),
        f"{platform} rustc evidence digest",
    )
    return {
        "platform_key": platform,
        "assets_artifact": assets_meta,
        "provenance_artifact": provenance_meta,
        "build_record_sha256": record_sha,
        "attestation_id": attestation_id,
        "attestation_bundle_sha256": sha256_file(
            provenance_dir / "build-provenance.sigstore.json"
        ),
        "runner": record_value["runner"],
        "toolchain": record_value["toolchain"],
        "files": record_value["files"],
    }


def verify_downloaded(args: argparse.Namespace) -> dict[str, Any]:
    resolution = load_candidate_resolution(
        args.resolution, args.candidate_run_id, args.expected_source_sha
    )
    root = args.downloads_dir
    expected_names = {item["name"] for item in resolution["artifacts"]}
    exact_entries(root, expected_names, "candidate download root")
    by_role_platform = {
        (item["role"], item["platform_key"]): item for item in resolution["artifacts"]
    }
    fast = verify_fast_directory(
        root / by_role_platform[("fast-proof", None)]["name"],
        args.expected_source_sha,
        args.fast_run_id,
        args.release_intent_id,
        args.planned_release_ref,
        args.release_kind,
    )
    platforms = []
    for platform in PLATFORMS:
        platforms.append(
            verify_platform_download(
                root,
                by_role_platform[("canonical-assets", platform)],
                by_role_platform[("canonical-provenance", platform)],
                args.expected_source_sha,
                args.fast_run_id,
                args.candidate_run_id,
                args.release_intent_id,
                args.planned_release_ref,
                args.release_kind,
            )
        )
    return {
        "schema_version": 1,
        "status": "verified-for-shadow-sealing",
        "repository_id": REPOSITORY_ID,
        "source_git_sha": args.expected_source_sha,
        "fast_run_id": args.fast_run_id,
        "candidate_run_id": args.candidate_run_id,
        "candidate_run_attempt": 1,
        "release_intent_id": args.release_intent_id,
        "planned_release_ref": args.planned_release_ref,
        "release_kind": args.release_kind,
        "resolution_sha256": sha256_file(args.resolution),
        "fast_evidence": fast,
        "source_artifacts": resolution["artifacts"],
        "canonical_platforms": platforms,
    }


def validate_common(args: argparse.Namespace) -> None:
    if args.repository != REPOSITORY:
        raise ValueError(f"repository must be exactly {REPOSITORY}")
    if args.candidate_run_id <= 0:
        raise ValueError("candidate run ID must be positive")
    if SHA.fullmatch(args.expected_source_sha) is None:
        raise ValueError(
            "expected source SHA must be 40 lowercase hexadecimal characters"
        )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    resolve_parser = commands.add_parser("resolve")
    for command in (resolve_parser,):
        command.add_argument("--repository", default=REPOSITORY)
        command.add_argument("--candidate-run-id", type=int, required=True)
        command.add_argument("--expected-source-sha", required=True)
    resolve_parser.add_argument("--repository-json", type=Path)
    resolve_parser.add_argument("--run-json", type=Path)
    resolve_parser.add_argument("--artifacts-json", type=Path)
    resolve_parser.add_argument("--output", type=Path, required=True)
    verify_parser = commands.add_parser("verify-downloaded")
    verify_parser.add_argument("--repository", default=REPOSITORY)
    verify_parser.add_argument("--candidate-run-id", type=int, required=True)
    verify_parser.add_argument("--expected-source-sha", required=True)
    verify_parser.add_argument("--fast-run-id", type=int, required=True)
    verify_parser.add_argument("--release-intent-id", required=True)
    verify_parser.add_argument("--planned-release-ref", required=True)
    verify_parser.add_argument(
        "--release-kind", choices=("shadow", "rc", "stable"), required=True
    )
    verify_parser.add_argument("--resolution", type=Path, required=True)
    verify_parser.add_argument("--downloads-dir", type=Path, required=True)
    verify_parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    validate_common(args)
    if args.command == "resolve":
        value = resolve_candidate_artifacts(
            ResolutionRequest(
                repository=args.repository,
                candidate_run_id=args.candidate_run_id,
                source_sha=args.expected_source_sha,
                repository_json=args.repository_json,
                run_json=args.run_json,
                artifacts_json=args.artifacts_json,
            )
        )
    else:
        if args.fast_run_id <= 0:
            raise ValueError("fast run ID must be positive")
        if INTENT.fullmatch(args.release_intent_id) is None:
            raise ValueError("release intent ID uses characters outside the allowlist")
        if TAG.fullmatch(args.planned_release_ref) is None:
            raise ValueError("planned release ref is not canonical")
        value = verify_downloaded(args)
    write_output(args.output, value)
    print(canonical_sha256(value))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
