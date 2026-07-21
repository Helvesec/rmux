#!/usr/bin/env python3
"""Validate the canonical native-build contract and its schemas."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CONTRACT_DIR = ROOT / ".github" / "release"

CANONICAL_RUNNER_LABELS = {
    "macos-15",
    "macos-15-intel",
    "ubuntu-22.04",
    "ubuntu-22.04-arm",
    "windows-latest",
}

PLATFORMS = {
    "linux-x86_64": (
        "ubuntu-22.04",
        "Linux",
        "X64",
        "x86_64-unknown-linux-gnu",
    ),
    "linux-aarch64": (
        "ubuntu-22.04-arm",
        "Linux",
        "ARM64",
        "aarch64-unknown-linux-gnu",
    ),
    "macos-x86_64": (
        "macos-15-intel",
        "macOS",
        "X64",
        "x86_64-apple-darwin",
    ),
    "macos-aarch64": (
        "macos-15",
        "macOS",
        "ARM64",
        "aarch64-apple-darwin",
    ),
    "windows-x86_64": (
        "windows-latest",
        "Windows",
        "X64",
        "x86_64-pc-windows-msvc",
    ),
}


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON object key: {key!r}")
        value[key] = item
    return value


def _load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {path.relative_to(ROOT)}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path.relative_to(ROOT)} must contain an object")
    return value


def validate_candidate_policy(value: Any) -> None:
    strict_contract = _load(CONTRACT_DIR / "candidate-workflow-contract.json")
    expected = {
        "contract": ".github/release/canonical-build-contract.json",
        "workflow": ".github/workflows/canonical-native-build.yml",
        "native_platforms": 5,
        "windows_smokes": ["mouse", "runtime", "sdk"],
        "artifact_retention_days": 7,
        "download_by_artifact_id": True,
        "object_cache_restored": False,
    }
    if value != expected or strict_contract.get("canonical_builds") != expected:
        raise ValueError("canonical candidate build policy drifted")


def validate_build() -> None:
    contract = _load(CONTRACT_DIR / "canonical-build-contract.json")
    if (
        contract.get("schema_version") != 1
        or contract.get("status") != "review-only-non-publishing"
        or contract.get("repository_id") != 1239918790
        or contract.get("rust_toolchain") != "1.96.1"
        or contract.get("artifact_retention_days") != 7
    ):
        raise ValueError("canonical build contract identity changed")
    platforms = contract.get("platforms")
    if not isinstance(platforms, list):
        raise ValueError("canonical platforms must be an array")
    actual = {
        entry.get("key"): (
            entry.get("runner_image"),
            entry.get("runner_os"),
            entry.get("runner_arch"),
            entry.get("target_triple"),
        )
        for entry in platforms
        if isinstance(entry, dict)
    }
    if actual != PLATFORMS or len(platforms) != len(PLATFORMS):
        raise ValueError("canonical target-to-runner mapping changed")
    expected_supplemental = {
        "linux-x86_64": [
            "crate-package-set",
            "snap-amd64",
            "wasm-byte-set",
            "wasm-provenance",
        ],
        "linux-aarch64": ["snap-arm64"],
        "macos-x86_64": [],
        "macos-aarch64": [],
        "windows-x86_64": ["chocolatey-package"],
    }
    if {
        entry.get("key"): entry.get("supplemental_roles")
        for entry in platforms
        if isinstance(entry, dict)
    } != expected_supplemental:
        raise ValueError("canonical supplemental role mapping changed")

    workflow = (ROOT / ".github/workflows/canonical-native-build.yml").read_text(
        encoding="utf-8"
    )
    actual_job_runners: dict[str, str] = {}
    current_job: str | None = None
    inside_jobs = False
    for line in workflow.splitlines():
        if line == "jobs:":
            inside_jobs = True
            continue
        if not inside_jobs:
            continue
        job_match = re.fullmatch(r"  ([a-z0-9-]+):", line)
        if job_match is not None:
            current_job = job_match.group(1)
            continue
        runner_match = re.fullmatch(r"    runs-on: ([A-Za-z0-9.-]+)", line)
        if current_job is not None and runner_match is not None:
            actual_job_runners[current_job] = runner_match.group(1)
    expected_job_runners = {
        **{
            f"build-{key.replace('_', '-')}": value[0]
            for key, value in PLATFORMS.items()
        },
        **{
            f"smoke-{key.replace('_', '-')}": value[0]
            for key, value in PLATFORMS.items()
        },
        "gate": "ubuntu-22.04",
    }
    if actual_job_runners != expected_job_runners:
        raise ValueError("canonical workflow job-to-runner mapping changed")
    for forbidden in (
        "runs-on: ${{",
        "actions/cache@",
        "sccache",
        "contents: write",
        "packages: write",
        "secrets: inherit",
        "environment:",
    ):
        if forbidden in workflow:
            raise ValueError(f"canonical workflow contains forbidden value {forbidden}")
    if workflow.count("uses: ./.github/actions/canonical-build") != 5:
        raise ValueError("canonical workflow must contain five explicit native builds")
    if workflow.count("expected-build-record-sha256: ${{ needs.build-") != 5:
        raise ValueError("every canonical smoke must bind the producer record digest")
    if "matrix:\n        smoke: [runtime, sdk, mouse]" not in workflow:
        raise ValueError("canonical Windows smokes must be exhaustive and explicit")


def validate_schemas(candidate: dict[str, Any], schema_dir: Path) -> None:
    strict_candidate = _load(schema_dir / "candidate-manifest.schema.json")
    if candidate != strict_candidate:
        raise ValueError(
            "candidate manifest schema changed during canonical validation"
        )
    candidate = strict_candidate
    runner_images = candidate["properties"]["artifacts"]["items"]["properties"][
        "runner_image"
    ].get("enum")
    if runner_images != sorted(CANONICAL_RUNNER_LABELS):
        raise ValueError(
            "candidate runner_image must use the canonical runner allowlist"
        )
    target_triples = candidate["properties"]["artifacts"]["items"]["properties"][
        "target_triple"
    ].get("enum")
    if target_triples != sorted(entry[3] for entry in PLATFORMS.values()):
        raise ValueError("candidate target_triple must use the canonical allowlist")
    artifact_pairing = candidate["properties"]["artifacts"]["items"].get("allOf")
    if not isinstance(artifact_pairing, list) or len(artifact_pairing) != 5:
        raise ValueError("candidate schema must bind each target to one runner image")

    canonical_record = _load(schema_dir / "canonical-build-record.schema.json")
    canonical_binding = _load(schema_dir / "canonical-artifact-binding.schema.json")
    for filename, schema in (
        ("canonical-build-record.schema.json", canonical_record),
        ("canonical-artifact-binding.schema.json", canonical_binding),
    ):
        if schema.get("additionalProperties") is not False:
            raise ValueError(f"{filename} must fail closed on unknown fields")
        if not isinstance(schema.get("required"), list):
            raise ValueError(f"{filename} must define exhaustive required fields")
    record_files = canonical_record["properties"]["files"]["items"]
    if set(record_files.get("required", [])) != {"path", "role", "size", "sha256"}:
        raise ValueError("canonical record must bind every asset role and byte")
    binding_required = set(canonical_binding.get("required", []))
    if not {"assets", "attestation", "build_record_sha256"} <= binding_required:
        raise ValueError("canonical artifact binding is incomplete")
