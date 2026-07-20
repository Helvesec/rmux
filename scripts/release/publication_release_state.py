#!/usr/bin/env python3
"""Validate immutable GitHub Release state against exact authorization bytes."""

from __future__ import annotations

from typing import Any

from release_evidence import (
    DIGEST,
    REPOSITORY_ID,
    RELEASE_REF,
    exact_keys,
    positive_integer,
    require_match,
    timestamp,
)


def _expected_release_assets(
    predicate: dict[str, Any], envelope: dict[str, Any]
) -> dict[str, dict[str, Any]]:
    expected: dict[str, dict[str, Any]] = {}
    for asset in predicate["assets"]:
        expected[asset["name"]] = {
            "role": asset["role"],
            "size": asset["size"],
            "digest": f"sha256:{asset['sha256']}",
        }
    for asset in envelope["public_metadata_assets"]:
        if asset["name"] in expected:
            raise ValueError("authorization metadata collides with a candidate asset")
        expected[asset["name"]] = {
            "role": asset["role"],
            "size": asset["size"],
            "digest": f"sha256:{asset['sha256']}",
        }
    return expected


def validate_release_state(
    state: dict[str, Any], predicate: dict[str, Any], envelope: dict[str, Any]
) -> list[dict[str, Any]]:
    exact_keys(
        state,
        {
            "schema_version",
            "status",
            "repository_id",
            "release_id",
            "release_ref",
            "source_git_sha",
            "tag_object_sha",
            "draft",
            "prerelease",
            "immutable",
            "created_at",
            "published_at",
            "assets",
        },
        "release state",
    )
    if (
        state["schema_version"] != 1
        or state["status"] != "verified-immutable-release"
        or state["repository_id"] != REPOSITORY_ID
        or state["release_ref"] != predicate["release"]["ref"]
        or state["source_git_sha"] != predicate["source_git_sha"]
        or state["tag_object_sha"] != predicate["signed_tag"]["tag_object_sha"]
        or state["draft"] is not False
        or state["prerelease"] is not predicate["release"]["is_prerelease"]
        or state["immutable"] is not True
    ):
        raise ValueError(
            "release state does not match the authorized immutable release"
        )
    positive_integer(state["release_id"], "GitHub Release ID")
    require_match(state["release_ref"], RELEASE_REF, "GitHub Release ref")
    created = timestamp(state["created_at"], "GitHub Release created_at")
    published = timestamp(state["published_at"], "GitHub Release published_at")
    issued = timestamp(predicate["issued_at"], "authorization issued_at")
    expires = timestamp(predicate["expires_at"], "authorization expires_at")
    audit_expires = timestamp(
        predicate["policy_audit"]["expires_at"], "policy audit expires_at"
    )
    if not issued <= created <= published <= expires or published > audit_expires:
        raise ValueError("GitHub Release changed state outside the authorization TTL")
    expected = _expected_release_assets(predicate, envelope)
    actual = state["assets"]
    if not isinstance(actual, list) or len(actual) != len(expected):
        raise ValueError("GitHub Release asset cardinality changed")
    rendered: list[dict[str, Any]] = []
    ids: set[int] = set()
    names: list[str] = []
    for asset in actual:
        if not isinstance(asset, dict):
            raise ValueError("GitHub Release asset must be an object")
        exact_keys(asset, {"id", "name", "size", "digest"}, "GitHub Release asset")
        asset_id = positive_integer(asset["id"], "GitHub Release asset ID")
        if asset_id in ids:
            raise ValueError("GitHub Release asset IDs are duplicated")
        ids.add(asset_id)
        name = asset["name"]
        names.append(name)
        contracted = expected.get(name)
        if contracted is None or any(
            asset[field] != contracted[field] for field in ("size", "digest")
        ):
            raise ValueError(f"GitHub Release asset differs from authorization: {name}")
        require_match(asset["digest"], DIGEST, f"GitHub Release asset {name} digest")
        rendered.append({**asset, "role": contracted["role"]})
    if names != sorted(names) or len(names) != len(set(names)):
        raise ValueError("GitHub Release assets must be sorted and unique")
    return rendered
