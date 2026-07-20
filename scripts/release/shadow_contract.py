#!/usr/bin/env python3
"""Validate the non-authoritative shadow-sealer contract."""

from __future__ import annotations

from typing import Any


EXPECTED_SHADOW_SEALER = {
    "workflow_id": 316223904,
    "workflow": ".github/workflows/release-shadow.yml",
    "selected_by_candidate_run_id": True,
    "candidate_artifact_count": 11,
    "canonical_platforms": [
        "linux-aarch64",
        "linux-x86_64",
        "macos-aarch64",
        "macos-x86_64",
        "windows-x86_64",
    ],
    "manifest_status": "shadow-non-authoritative",
    "manifest_retention_days": 7,
    "manifest_self_reference": False,
}


def validate_candidate_policy(value: Any) -> None:
    """Reject drift in the exact shadow-sealer policy."""
    if value != EXPECTED_SHADOW_SEALER:
        raise ValueError("candidate shadow sealer contract drifted")
