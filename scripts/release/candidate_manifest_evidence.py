"""Validate the exact proof, intent, and policy inputs of a candidate manifest."""

from __future__ import annotations

import hashlib
import json
import re
from datetime import datetime, timedelta
from typing import Any

REPOSITORY_ID = 1239918790
CONTRACT_PATH = ".github/release/candidate-contract.json"
POLICY_DOMAIN = b"RMUX-RELEASE-POLICY\x00\x02"
SHA40 = re.compile(r"[0-9a-f]{40}")
SHA64 = re.compile(r"[0-9a-f]{64}")
SAFE_PATH = re.compile(r"[A-Za-z0-9._+@=-]+(?:/[A-Za-z0-9._+@=-]+)*")
PROOF_KEYS = set(
    "schema_version kind repository_id run_id run_attempt source_git_sha "
    "run_started_at verified_at contract_sha256 contract_blob_oid test_fixture "
    "jobs proof_sha256".split()
)
JOB_KEYS = set(
    "id name conclusion labels runner_id runner_name runner_group_id "
    "runner_group_name".split()
)
INTENT_KEYS = set(
    "schema_version repository_id source_git_sha fast_run_id release_intent_id "
    "planned_release_ref release_kind release_version package_version "
    "is_prerelease candidate_run_attempt".split()
)
POLICY_KEYS = set(
    "schema_version algorithm source_git_sha contract_path contract_mode "
    "contract_type contract_blob_oid release_policy_sha256 records".split()
)


def canonical_hash(value: dict[str, Any]) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValueError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"extra={sorted(actual - expected)}"
        )


def positive_integer(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be an ISO-8601 timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"{label} is not a valid ISO-8601 timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{label} must use UTC")
    if value != parsed.isoformat().replace("+00:00", "Z"):
        raise ValueError(f"{label} is not canonically encoded")
    return parsed


def validate_proof(
    proof: dict[str, Any], expected_kind: str, label: str
) -> tuple[datetime, datetime]:
    exact_keys(proof, PROOF_KEYS, label)
    if proof["schema_version"] != 1 or proof["kind"] != expected_kind:
        raise ValueError(f"{label} identity changed")
    if proof["repository_id"] != REPOSITORY_ID or proof["run_attempt"] != 1:
        raise ValueError(f"{label} repository or attempt changed")
    positive_integer(proof["run_id"], f"{label} run ID")
    if not isinstance(proof["source_git_sha"], str) or not SHA40.fullmatch(
        proof["source_git_sha"]
    ):
        raise ValueError(f"{label} source SHA is not canonical")
    for field in ("contract_sha256", "proof_sha256"):
        if not isinstance(proof[field], str) or not SHA64.fullmatch(proof[field]):
            raise ValueError(f"{label} {field} is not a SHA-256 digest")
    if not isinstance(proof["contract_blob_oid"], str) or not SHA40.fullmatch(
        proof["contract_blob_oid"]
    ):
        raise ValueError(f"{label} contract blob OID is not canonical")
    if proof["test_fixture"] is not False:
        raise ValueError(f"{label} must come from live GitHub evidence")
    unsigned = dict(proof)
    claimed_hash = unsigned.pop("proof_sha256")
    if canonical_hash(unsigned) != claimed_hash:
        raise ValueError(f"{label} semantic digest changed")
    jobs = proof["jobs"]
    if not isinstance(jobs, list) or not jobs:
        raise ValueError(f"{label} jobs must be a non-empty array")
    names: set[str] = set()
    identifiers: set[int] = set()
    for job in jobs:
        if not isinstance(job, dict):
            raise ValueError(f"{label} job must be an object")
        exact_keys(job, JOB_KEYS, f"{label} job")
        job_id = positive_integer(job["id"], f"{label} job ID")
        name = job["name"]
        if not isinstance(name, str) or not name:
            raise ValueError(f"{label} job name must be non-empty")
        if job_id in identifiers or name in names:
            raise ValueError(f"{label} contains duplicate jobs")
        identifiers.add(job_id)
        names.add(name)
        if job["conclusion"] not in {"success", "skipped"}:
            raise ValueError(f"{label} job conclusion is not contracted")
        if not isinstance(job["labels"], list) or not all(
            isinstance(item, str) for item in job["labels"]
        ):
            raise ValueError(f"{label} job labels are invalid")
    if [job["name"] for job in jobs] != sorted(names):
        raise ValueError(f"{label} jobs are not sorted by name")
    if expected_kind == "candidate":
        gates = [job for job in jobs if job["name"] == "Release candidate gate"]
        if len(gates) != 1 or gates[0]["conclusion"] != "success":
            raise ValueError(
                "candidate proof does not contain one successful final gate"
            )
    started = timestamp(proof["run_started_at"], f"{label} run_started_at")
    verified = timestamp(proof["verified_at"], f"{label} verified_at")
    if verified < started:
        raise ValueError(f"{label} was verified before it started")
    return started, verified


def validate_intent(intent: dict[str, Any]) -> None:
    exact_keys(intent, INTENT_KEYS, "candidate intent")
    if intent["schema_version"] != 1 or intent["repository_id"] != REPOSITORY_ID:
        raise ValueError("candidate intent identity changed")
    if intent["candidate_run_attempt"] != 1:
        raise ValueError("candidate intent requires attempt 1")
    if intent["release_kind"] not in {"shadow", "rc", "stable"}:
        raise ValueError("candidate intent release kind is invalid")
    version = intent["release_version"]
    if intent["package_version"] != version or intent["planned_release_ref"] != (
        f"v{version}"
    ):
        raise ValueError("candidate intent version fields disagree")
    is_rc = isinstance(version, str) and "-rc." in version
    if intent["is_prerelease"] is not is_rc:
        raise ValueError("candidate intent prerelease flag disagrees with its version")
    if intent["release_kind"] == "stable" and is_rc:
        raise ValueError("stable intent cannot carry an RC version")
    if intent["release_kind"] == "rc" and not is_rc:
        raise ValueError("RC intent requires an RC version")


def encode_field(value: bytes) -> bytes:
    return len(value).to_bytes(8, "big") + value


def validate_policy(report: dict[str, Any], source_sha: str) -> dict[str, Any]:
    exact_keys(report, POLICY_KEYS, "policy root")
    if (
        report["schema_version"] != 1
        or report["algorithm"] != "sha256-length-delimited-v2"
    ):
        raise ValueError("policy root algorithm changed")
    if report["source_git_sha"] != source_sha:
        raise ValueError("policy root source SHA mismatch")
    if (
        report["contract_path"] != CONTRACT_PATH
        or report["contract_mode"] != "100644"
        or report["contract_type"] != "blob"
    ):
        raise ValueError("policy root contract identity changed")
    if not isinstance(report["contract_blob_oid"], str) or not SHA40.fullmatch(
        report["contract_blob_oid"]
    ):
        raise ValueError("policy root contract blob OID is invalid")
    records = report["records"]
    if not isinstance(records, list) or not records:
        raise ValueError("policy root records must be non-empty")
    digest = hashlib.sha256()
    digest.update(POLICY_DOMAIN)
    paths: list[str] = []
    contract_record: dict[str, Any] | None = None
    for record in records:
        if not isinstance(record, dict):
            raise ValueError("policy record must be an object")
        exact_keys(
            record,
            {"path", "mode", "type", "size", "blob_oid", "sha256"},
            "policy record",
        )
        path = record["path"]
        if not isinstance(path, str) or SAFE_PATH.fullmatch(path) is None:
            raise ValueError("policy record path is not canonical")
        if record["mode"] not in {"100644", "100755"} or record["type"] != "blob":
            raise ValueError("policy record is not a regular Git blob")
        size = record["size"]
        if type(size) is not int or size < 0:
            raise ValueError("policy record size is invalid")
        if not isinstance(record["blob_oid"], str) or not SHA40.fullmatch(
            record["blob_oid"]
        ):
            raise ValueError("policy record blob OID is invalid")
        if not isinstance(record["sha256"], str) or not SHA64.fullmatch(
            record["sha256"]
        ):
            raise ValueError("policy record digest is invalid")
        paths.append(path)
        for field in (path, record["mode"], record["type"], record["blob_oid"]):
            digest.update(encode_field(field.encode("utf-8")))
        digest.update(encode_field(size.to_bytes(8, "big")))
        digest.update(encode_field(record["sha256"].encode("ascii")))
        if path == CONTRACT_PATH:
            contract_record = record
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise ValueError("policy root paths must be sorted and unique")
    if contract_record is None:
        raise ValueError("policy root does not contain its candidate contract")
    if digest.hexdigest() != report["release_policy_sha256"]:
        raise ValueError("policy root digest changed")
    if contract_record["blob_oid"] != report["contract_blob_oid"]:
        raise ValueError("policy root contract blob identity disagrees")
    return contract_record
