#!/usr/bin/env python3
"""Typed identity and provenance validation for RMUX perf artifacts."""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


CURRENT_SCHEMA = 2
CURRENT_KIND = "rmux-perf-current"
CURRENT_GENERATOR = "scripts/perf-bench.sh"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
GIT_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


@dataclass(frozen=True)
class ArtifactIdentity:
    """Release-relevant identity fields extracted from a perf artifact."""

    path: Path
    schema: object
    kind: str | None
    timestamp: str | None
    platform: str | None
    machine: str | None
    host_fingerprint: str | None
    git_commit: str | None
    git_describe: str | None
    git_dirty: bool | None
    binary_path: str | None
    binary_sha256: str | None
    binary_version: str | None
    binary_configuration: str | None
    provenance_generator: str | None
    provenance_invocation: str | None
    provenance_expected_commit: str | None
    provenance_expected_platform: str | None
    provenance_build_mode: str | None


def first_non_empty(*values: object, lower: bool = False) -> str | None:
    """Return the first non-empty string, optionally normalized to lowercase."""

    for value in values:
        if isinstance(value, str) and value.strip():
            normalized = value.strip()
            return normalized.lower() if lower else normalized
    return None


def load_payload(path: Path) -> tuple[dict[str, Any], dict[str, Any], ArtifactIdentity]:
    """Load a baseline wrapper or current artifact and extract its identity."""

    import json

    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)
    if not isinstance(payload, dict):
        raise ValueError(f"{path}: perf artifact is not an object")
    source = payload.get("source", payload)
    if not isinstance(source, dict):
        raise ValueError(f"{path}: source payload is not an object")

    wrapper_environment = object_dict(payload.get("environment"))
    source_environment = object_dict(source.get("environment"))
    wrapper_git = object_dict(payload.get("git"))
    source_git = object_dict(source.get("git"))
    binary = object_dict(source.get("binary"))
    provenance = object_dict(source.get("provenance"))

    legacy_binary_path = source.get("binary") if isinstance(source.get("binary"), str) else None
    identity = ArtifactIdentity(
        path=path,
        schema=payload.get("schema"),
        kind=first_non_empty(payload.get("kind")),
        timestamp=first_non_empty(payload.get("timestamp"), source.get("timestamp")),
        platform=first_non_empty(
            wrapper_environment.get("platform"),
            source_environment.get("platform"),
            source.get("platform"),
            payload.get("platform"),
            lower=True,
        ),
        machine=first_non_empty(
            wrapper_environment.get("machine"),
            source_environment.get("machine"),
            source.get("machine"),
            payload.get("machine"),
            lower=True,
        ),
        host_fingerprint=first_non_empty(
            wrapper_environment.get("host_fingerprint"),
            source_environment.get("host_fingerprint"),
            source.get("host_fingerprint"),
            payload.get("host_fingerprint"),
            lower=True,
        ),
        git_commit=first_non_empty(wrapper_git.get("commit"), source_git.get("commit"), lower=True),
        git_describe=first_non_empty(wrapper_git.get("describe"), source_git.get("describe")),
        git_dirty=first_bool(wrapper_git.get("dirty"), source_git.get("dirty")),
        binary_path=first_non_empty(binary.get("path"), legacy_binary_path),
        binary_sha256=first_non_empty(binary.get("sha256"), lower=True),
        binary_version=first_non_empty(binary.get("version")),
        binary_configuration=first_non_empty(binary.get("configuration"), lower=True),
        provenance_generator=first_non_empty(provenance.get("generator")),
        provenance_invocation=first_non_empty(provenance.get("invocation")),
        provenance_expected_commit=first_non_empty(
            provenance.get("expected_git_commit"), lower=True
        ),
        provenance_expected_platform=first_non_empty(
            provenance.get("expected_platform"), lower=True
        ),
        provenance_build_mode=first_non_empty(provenance.get("build_mode"), lower=True),
    )
    return payload, source, identity


def object_dict(value: object) -> dict[str, Any]:
    """Return an object value as a dictionary or an empty dictionary."""

    return value if isinstance(value, dict) else {}


def first_bool(*values: object) -> bool | None:
    """Return the first real boolean value."""

    return next((value for value in values if isinstance(value, bool)), None)


def validate_current_identity(
    current: ArtifactIdentity,
    *,
    expected_commit: str,
    expected_platform: str,
    expected_machine: str,
    expected_host_fingerprint: str,
    expected_provenance: str,
    expected_binary_version: str,
    max_age_seconds: int,
    now: datetime | None = None,
    verify_binary: bool = True,
) -> None:
    """Fail closed unless a current artifact is fresh and bound to the checkout/binary."""

    if current.schema != CURRENT_SCHEMA or current.kind != CURRENT_KIND:
        raise ValueError(
            f"{current.path}: current perf JSON must be schema {CURRENT_SCHEMA} kind {CURRENT_KIND}"
        )
    expected_commit = validate_git_sha(expected_commit, "expected current Git SHA")
    expected_platform = required_lower(expected_platform, "expected current platform")
    expected_machine = required_lower(expected_machine, "expected current machine")
    expected_host_fingerprint = required_lower(
        expected_host_fingerprint, "expected current host fingerprint"
    )
    if not expected_provenance.strip():
        raise ValueError("expected current provenance is empty")

    current_commit = validate_git_sha(current.git_commit, f"{current.path}: git.commit")
    if current_commit != expected_commit:
        raise ValueError(
            f"current perf Git SHA mismatch: expected={expected_commit} current={current_commit}"
        )
    if current.git_dirty is not False or (current.git_describe or "").endswith("-dirty"):
        raise ValueError(f"{current.path}: current perf artifact was recorded from a dirty worktree")
    require_equal("platform", expected_platform, current.platform)
    require_equal("machine", expected_machine, current.machine)
    require_equal("host fingerprint", expected_host_fingerprint, current.host_fingerprint)

    require_equal("provenance generator", CURRENT_GENERATOR, current.provenance_generator)
    require_equal("provenance invocation", expected_provenance, current.provenance_invocation)
    require_equal("provenance Git SHA", expected_commit, current.provenance_expected_commit)
    require_equal("provenance platform", expected_platform, current.provenance_expected_platform)
    require_equal("provenance build mode", "rebuilt", current.provenance_build_mode)
    require_equal("binary configuration", "release", current.binary_configuration)
    require_equal("binary version", expected_binary_version, current.binary_version)

    validate_fresh_timestamp(current.path, current.timestamp, max_age_seconds, now=now)
    digest = current.binary_sha256 or ""
    if not SHA256_RE.fullmatch(digest):
        raise ValueError(f"{current.path}: binary.sha256 is missing or invalid")
    binary_path = Path(current.binary_path or "")
    if not binary_path.is_absolute():
        raise ValueError(f"{current.path}: binary.path must be absolute")
    if verify_binary:
        if not binary_path.is_file():
            raise ValueError(f"{current.path}: measured binary is unavailable: {binary_path}")
        actual_digest = sha256_file(binary_path)
        if actual_digest != digest:
            raise ValueError(
                f"{current.path}: measured binary digest mismatch: recorded={digest} actual={actual_digest}"
            )


def validate_fresh_timestamp(
    path: Path, value: str | None, max_age_seconds: int, *, now: datetime | None = None
) -> None:
    """Validate an RFC3339 UTC timestamp against a bounded age."""

    if max_age_seconds <= 0:
        raise ValueError("max current artifact age must be positive")
    if value is None:
        raise ValueError(f"{path}: current perf timestamp is missing")
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"{path}: invalid current perf timestamp: {value}") from error
    if timestamp.tzinfo is None:
        raise ValueError(f"{path}: current perf timestamp must include a timezone")
    timestamp = timestamp.astimezone(timezone.utc)
    reference = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    age_seconds = (reference - timestamp).total_seconds()
    if age_seconds < -300:
        raise ValueError(f"{path}: current perf timestamp is in the future")
    if age_seconds > max_age_seconds:
        raise ValueError(
            f"{path}: current perf artifact is stale: age={int(age_seconds)}s max={max_age_seconds}s"
        )


def validate_git_sha(value: str | None, label: str) -> str:
    """Normalize and validate a full Git object SHA."""

    normalized = (value or "").strip().lower()
    if not GIT_SHA_RE.fullmatch(normalized):
        raise ValueError(f"{label} is missing or invalid")
    return normalized


def required_lower(value: str | None, label: str) -> str:
    """Normalize a required identity string."""

    normalized = (value or "").strip().lower()
    if not normalized:
        raise ValueError(f"{label} is empty")
    return normalized


def require_equal(label: str, expected: str, actual: str | None) -> None:
    """Require an exact identity match."""

    if actual != expected:
        raise ValueError(f"current perf {label} mismatch: expected={expected} current={actual or '<missing>'}")


def sha256_file(path: Path) -> str:
    """Hash a file without loading it all into memory."""

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()
