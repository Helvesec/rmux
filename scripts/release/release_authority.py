#!/usr/bin/env python3
"""Strict, tracked authority state for the RMUX release pipeline."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_LEDGER = ROOT / ".github" / "release" / "release-activation.json"
CAPABILITIES = (
    "downstream_channels",
    "github_release_publication",
    "policy_audit",
    "promotion_authorization",
    "publication_receipt",
    "signed_tag_creation",
)
DISARMED_STATUS = "disarmed"
ACTIVE_STATUS = "active"
DISARMED_EVIDENCE_STATUS = "disarmed-non-authoritative"
ACTIVE_EVIDENCE_STATUS = "authorized"


@dataclass(frozen=True)
class ReleaseAuthority:
    active: bool
    capabilities: dict[str, bool]

    @property
    def evidence_status(self) -> str:
        return ACTIVE_EVIDENCE_STATUS if self.active else DISARMED_EVIDENCE_STATUS

    def permits(self, required: Iterable[str]) -> bool:
        names = tuple(required)
        unknown = set(names) - set(CAPABILITIES)
        if unknown:
            raise ValueError(f"unknown release capabilities: {sorted(unknown)}")
        return all(self.capabilities[name] for name in names)


def validate_activation(value: dict[str, Any]) -> ReleaseAuthority:
    expected_keys = {
        "schema_version",
        "status",
        "description",
        "cutover_pr",
        "runtime_override_allowed",
        "capabilities",
    }
    if set(value) != expected_keys:
        raise ValueError("release activation ledger keys changed")
    if (
        value["schema_version"] != 1
        or value["cutover_pr"] != "PR8"
        or value["runtime_override_allowed"] is not False
        or not isinstance(value["description"], str)
        or not value["description"]
    ):
        raise ValueError("release activation ledger identity changed")
    capabilities = value["capabilities"]
    if not isinstance(capabilities, dict) or set(capabilities) != set(CAPABILITIES):
        raise ValueError("release activation capabilities changed")
    if any(type(capabilities[name]) is not bool for name in CAPABILITIES):
        raise ValueError("release activation capability must be boolean")
    status = value["status"]
    if status == DISARMED_STATUS and all(
        capabilities[name] is False for name in CAPABILITIES
    ):
        return ReleaseAuthority(False, dict(capabilities))
    if status == ACTIVE_STATUS and all(
        capabilities[name] is True for name in CAPABILITIES
    ):
        return ReleaseAuthority(True, dict(capabilities))
    raise ValueError("release activation must be atomically disarmed or active")


def load_authority(path: Path = DEFAULT_LEDGER) -> ReleaseAuthority:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"release activation ledger is invalid: {path}") from error
    if not isinstance(value, dict):
        raise ValueError("release activation ledger must be an object")
    return validate_activation(value)
