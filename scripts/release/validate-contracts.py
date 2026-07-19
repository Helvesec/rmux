#!/usr/bin/env python3
"""Validate release contracts without network access or publication credentials."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CONTRACT_DIR = ROOT / ".github" / "release"
ALLOWED_WINDOWS_PROOFS = {
    "fast_exact",
    "release_delta",
    "canonical_build",
    "canonical_smoke",
}
IRREVERSIBLE_RC_CHANNELS = {
    "apt_rpm",
    "chocolatey",
    "crates_io",
    "homebrew_core",
    "homebrew_tap",
    "rmux_io",
    "scoop",
    "web_share",
    "winget",
    "snap_stable",
}


def load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {path.relative_to(ROOT)}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path.relative_to(ROOT)} must contain an object")
    return value


def require_unique_strings(values: Any, label: str) -> list[str]:
    if not isinstance(values, list) or not all(
        isinstance(value, str) for value in values
    ):
        raise ValueError(f"{label} must be an array of strings")
    if len(values) != len(set(values)):
        raise ValueError(f"{label} contains duplicates")
    return values


def validate_candidate() -> None:
    contract = load(CONTRACT_DIR / "candidate-contract.json")
    if contract.get("schema_version") != 1:
        raise ValueError("candidate contract schema_version must be 1")
    repository = contract.get("repository", {})
    if (
        repository.get("full_name") != "Helvesec/rmux"
        or repository.get("id") != 1239918790
    ):
        raise ValueError("candidate contract repository identity drifted")
    fast = contract.get("fast_run", {})
    expected = {
        "branch": "main",
        "event": "push",
        "required_run_attempt": 1,
        "workflow_id": 277622540,
        "workflow_name": "CI",
        "workflow_path": ".github/workflows/ci.yml",
        "freshness_hours": 48,
        "maximum_signed_extension_hours": 72,
    }
    for key, value in expected.items():
        if fast.get(key) != value:
            raise ValueError(f"fast_run.{key} must equal {value!r}")
    success = require_unique_strings(fast.get("success_jobs"), "fast_run.success_jobs")
    skipped = require_unique_strings(fast.get("skipped_jobs"), "fast_run.skipped_jobs")
    allowed = fast.get("allowed_jobs")
    if not isinstance(allowed, dict) or allowed != {
        "Cache reusable Windows test archive": ["success", "skipped"]
    }:
        raise ValueError(
            "fast_run must explicitly allow the archive cache to succeed or skip"
        )
    overlap = (
        (set(success) & set(skipped))
        | (set(success) & set(allowed))
        | (set(skipped) & set(allowed))
    )
    if overlap:
        raise ValueError(
            f"fast jobs cannot be both success and skipped: {sorted(overlap)}"
        )
    shards = {
        name
        for name in success
        if re.fullmatch(r"Windows workspace tests \([0-9]+/18\)", name)
    }
    expected_shards = {
        f"Windows workspace tests ({index}/18)" for index in range(1, 19)
    }
    if shards != expected_shards:
        raise ValueError(
            "candidate contract must cover every Windows slice 1..18 exactly once"
        )
    qualification = contract.get("qualification_run", {})
    for key, value in {
        "branch": "main",
        "event": "workflow_dispatch",
        "required_run_attempt": 1,
        "workflow_id": 277622540,
        "workflow_name": "CI",
        "workflow_path": ".github/workflows/ci.yml",
    }.items():
        if qualification.get(key) != value:
            raise ValueError(f"qualification_run.{key} must equal {value!r}")
    qualification_success = require_unique_strings(
        qualification.get("success_jobs"), "qualification_run.success_jobs"
    )
    qualification_skipped = require_unique_strings(
        qualification.get("skipped_jobs"), "qualification_run.skipped_jobs"
    )
    qualification_allowed = qualification.get("allowed_jobs")
    if qualification_allowed != {
        "Cache reusable Windows test archive": ["success", "skipped"]
    }:
        raise ValueError(
            "qualification cache job must explicitly allow success or skip"
        )
    qualification_sets = [
        set(qualification_success),
        set(qualification_skipped),
        set(qualification_allowed),
    ]
    if any(
        qualification_sets[left] & qualification_sets[right]
        for left in range(len(qualification_sets))
        for right in range(left + 1, len(qualification_sets))
    ):
        raise ValueError("qualification job conclusion sets overlap")
    if len(set.union(*qualification_sets)) != 53 or len(qualification_success) != 52:
        raise ValueError(
            "qualification contract must contain 52 successes and one variable cache job"
        )
    qualification_shards = {
        name
        for name in qualification_success
        if re.fullmatch(r"Windows workspace tests \([0-9]+/18\)", name)
    }
    if qualification_shards != expected_shards:
        raise ValueError("qualification contract must cover every Windows slice 1..18")
    paths = require_unique_strings(contract.get("policy_paths"), "policy_paths")
    if paths != sorted(paths):
        raise ValueError("policy_paths must be bytewise sorted")
    missing = [path for path in paths if not (ROOT / path).is_file()]
    if missing:
        raise ValueError(f"policy paths do not exist: {missing}")
    tracked_result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=ROOT,
        check=False,
        capture_output=True,
    )
    if tracked_result.returncode != 0:
        raise ValueError("cannot enumerate tracked files for the release policy")
    tracked = {
        value.decode("utf-8") for value in tracked_result.stdout.split(b"\x00") if value
    }
    untracked = sorted(set(paths) - tracked)
    if untracked:
        raise ValueError(f"policy paths are not tracked by Git: {untracked}")
    if ".github/release/candidate-contract.json" not in paths:
        raise ValueError("candidate contract must include itself in policy_paths")
    workflow_paths = {
        path.relative_to(ROOT).as_posix()
        for suffix in ("*.yml", "*.yaml")
        for path in (ROOT / ".github" / "workflows").glob(suffix)
    }
    referenced_scripts: set[str] = set()
    for workflow_path in workflow_paths:
        workflow = (ROOT / workflow_path).read_text(encoding="utf-8")
        referenced_scripts.update(
            re.findall(
                r"(?:\./)?(scripts/[A-Za-z0-9_./-]+\.(?:sh|ps1|py))",
                workflow,
            )
        )
    all_scripts = {path for path in tracked if path.startswith("scripts/")}
    local_actions = {path for path in tracked if path.startswith(".github/actions/")}
    release_gate_data = {
        "CHANGELOG.md",
        "benches/perf/baselines/release-0.7.0.json",
        "benches/perf/baselines/release-0.9.0-linux.json",
        "benches/perf/baselines/release-0.9.0.json",
        "deny.toml",
        "tests/reference/tmux_compat/divergences.toml",
        "tests/reference/tmux_compat/error_exit_matrix.yaml",
        "tests/reference/tmux_compat/frozen_reference.yaml",
    }
    uncovered = sorted(
        (
            workflow_paths
            | referenced_scripts
            | all_scripts
            | local_actions
            | release_gate_data
        )
        - set(paths)
    )
    if uncovered:
        raise ValueError(
            f"release policy does not cover workflow-reachable files: {uncovered}"
        )


def validate_windows_coverage() -> None:
    contract = load(CONTRACT_DIR / "windows-coverage-contract.json")
    source = ROOT / str(contract.get("source", ""))
    source_text = source.read_text(encoding="utf-8")
    source_steps = re.findall(r'^\s*Step "([^"]+)"', source_text, re.MULTILINE)
    entries = contract.get("steps")
    if not isinstance(entries, list) or not all(
        isinstance(entry, dict) for entry in entries
    ):
        raise ValueError("Windows coverage steps must be an array of objects")
    contract_steps = [entry.get("name") for entry in entries]
    if len(contract_steps) != len(set(contract_steps)):
        raise ValueError("Windows coverage contract contains duplicate step names")
    if set(contract_steps) != set(source_steps):
        missing = sorted(set(source_steps) - set(contract_steps))
        stale = sorted(set(contract_steps) - set(source_steps))
        raise ValueError(f"Windows coverage mismatch; missing={missing}, stale={stale}")
    for entry in entries:
        if entry.get("proof") not in ALLOWED_WINDOWS_PROOFS:
            raise ValueError(f"invalid proof for Windows step {entry.get('name')}")
        if not isinstance(entry.get("evidence"), str) or not entry["evidence"].strip():
            raise ValueError(f"Windows step {entry.get('name')} has no evidence")


def validate_channels() -> None:
    policy = load(CONTRACT_DIR / "channel-policy.json")
    if policy.get("schema_version") != 1 or policy.get("default_decision") != "deny":
        raise ValueError("channel policy must be schema v1 and default-deny")
    kinds = policy.get("release_kinds")
    if not isinstance(kinds, dict) or set(kinds) != {"shadow", "rc", "stable"}:
        raise ValueError("channel policy must define exactly shadow, rc, and stable")
    channel_sets = {
        kind: set(values) for kind, values in kinds.items() if isinstance(values, dict)
    }
    if (
        len(channel_sets) != 3
        or len({frozenset(channels) for channels in channel_sets.values()}) != 1
    ):
        raise ValueError("every release kind must define the same channel set")
    if any(value is not False for value in kinds["shadow"].values()):
        raise ValueError("shadow releases must deny every channel")
    for channel in IRREVERSIBLE_RC_CHANNELS:
        if kinds["rc"].get(channel) is not False:
            raise ValueError(f"RC must deny irreversible channel {channel}")
    if kinds["rc"].get("snap_candidate") != "explicit_opt_in":
        raise ValueError("RC Snap candidate must require explicit opt-in")
    if kinds["stable"].get("snap_candidate") is not True:
        raise ValueError("stable releases currently stage Snap in latest/candidate")
    if kinds["stable"].get("snap_stable") is not False:
        raise ValueError("Snap stable must remain denied pending a support decision")
    if policy.get("snap_stable_publication") != "denied_until_support_decision":
        raise ValueError("Snap stable denial must be explicit")
    if policy.get("downstream_gate") != "publication_receipt_success":
        raise ValueError(
            "every downstream channel must gate on the publication receipt"
        )


def validate_schemas() -> None:
    schema_dir = CONTRACT_DIR / "schemas"
    expected = {
        "candidate-manifest.schema.json": {
            "candidate_run_attempt",
            "fast_run_proof_sha256",
            "producer",
            "canonical_build_policy",
            "release_policy_sha256",
            "artifacts",
        },
        "promotion-authorization.schema.json": {
            "authorization_attestation_id",
            "candidate_manifest_artifact_digest",
            "authorization_bundle_artifact_id",
            "authorization_bundle_artifact_digest",
        },
        "publication-receipt.schema.json": {
            "release_id",
            "authorization_attestation_id",
            "candidate_manifest_artifact_id",
            "candidate_manifest_artifact_digest",
            "authorization_bundle_artifact_id",
            "authorization_bundle_artifact_digest",
            "receipt_run_attempt",
            "immutable",
        },
    }
    for filename, required_fields in expected.items():
        schema = load(schema_dir / filename)
        if schema.get("additionalProperties") is not False:
            raise ValueError(f"{filename} must fail closed on unknown fields")
        required = set(
            require_unique_strings(schema.get("required"), f"{filename}.required")
        )
        missing = required_fields - required
        if missing:
            raise ValueError(f"{filename} is missing required fields {sorted(missing)}")
    receipt = load(schema_dir / "publication-receipt.schema.json")
    if receipt["properties"]["receipt_run_attempt"].get("const") != 1:
        raise ValueError("receipt-only runs must be attempt 1")
    authorization = load(schema_dir / "promotion-authorization.schema.json")
    if authorization["properties"]["authorization_run_attempt"].get("const") != 1:
        raise ValueError("promotion authorization runs must be attempt 1")
    candidate = load(schema_dir / "candidate-manifest.schema.json")
    intent_pattern = candidate["properties"]["release_intent_id"].get("pattern")
    if intent_pattern != "^[A-Za-z0-9._:-]{8,128}$":
        raise ValueError("release intent IDs must use the canonical ASCII allowlist")
    if not isinstance(candidate.get("allOf"), list) or len(candidate["allOf"]) < 3:
        raise ValueError(
            "candidate schema must bind stable/RC kind and prerelease state"
        )


def validate_measurement_budget() -> None:
    budget = load(CONTRACT_DIR / "measurement-budget.json")
    policy = budget.get("percentile_policy")
    if (
        not isinstance(policy, dict)
        or policy.get("minimum_samples_for_percentiles") != 7
    ):
        raise ValueError(
            "measurement percentiles require at least seven comparable samples"
        )
    if policy.get("below_minimum_report") != ["raw_values", "median", "maximum"]:
        raise ValueError("small samples must report raw values, median, and maximum")
    ceilings = budget.get("initial_ceilings_seconds")
    if not isinstance(ceilings, dict) or ceilings.get("fast_required_checks") != 600:
        raise ValueError("fast required checks ceiling must remain ten minutes")
    if any(not isinstance(value, int) or value <= 0 for value in ceilings.values()):
        raise ValueError("measurement ceilings must be positive integer seconds")
    if budget.get("observed_anchor_aggregation") != (
        "individual-observations-not-percentiles"
    ):
        raise ValueError("observed anchors must not be presented as percentiles")
    pathological = [
        anchor
        for anchor in budget.get("observed_anchors", [])
        if anchor.get("run_id") == 29647419533
    ]
    if len(pathological) != 5 or any(
        anchor.get("run_conclusion") != "failure"
        or anchor.get("job_conclusion") != "success"
        or anchor.get("step_name") != "Build rmux"
        or anchor.get("measurement") != "step_wallclock"
        for anchor in pathological
    ):
        raise ValueError("failed release baseline anchors must disclose their scope")


def main() -> int:
    validate_candidate()
    validate_windows_coverage()
    validate_channels()
    validate_schemas()
    validate_measurement_budget()
    print("release-contracts-ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
