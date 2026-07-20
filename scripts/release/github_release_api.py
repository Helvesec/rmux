#!/usr/bin/env python3
"""Bounded GitHub API client and live checks for one RMUX Release."""

from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode, urlparse
from urllib.request import HTTPRedirectHandler, Request, build_opener

REPOSITORY = "Helvesec/rmux"
REPOSITORY_ID = 1239918790
API_ROOT = "https://api.github.com"
UPLOAD_ROOT = "https://uploads.github.com"
API_VERSION = "2026-03-10"
MAX_RESPONSE_BYTES = 8 * 1024 * 1024


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(
        self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str
    ) -> None:
        return None


def positive(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def parse_time(value: Any, label: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be a canonical UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"{label} is not a valid timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{label} must use UTC")
    canonical = parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    if canonical != value:
        raise ValueError(f"{label} is not canonically encoded")
    return parsed


def validate_freshness(evidence: Any, now: datetime) -> None:
    predicate = evidence.predicate
    issued = parse_time(predicate.get("issued_at"), "authorization issued_at")
    expires = parse_time(predicate.get("expires_at"), "authorization expires_at")
    audit = predicate["policy_audit"]
    audit_emitted = parse_time(audit.get("emitted_at"), "policy audit emitted_at")
    audit_expires = parse_time(audit.get("expires_at"), "policy audit expires_at")
    candidate = predicate.get("candidate")
    if not isinstance(candidate, dict):
        raise ValueError("authorization candidate binding is missing")
    candidate_expires = parse_time(
        candidate.get("manifest_expires_at"), "candidate expiry"
    )
    if expires <= issued or expires - issued > timedelta(minutes=10):
        raise ValueError("authorization TTL exceeds ten minutes")
    if audit_expires <= audit_emitted or audit_expires - audit_emitted > timedelta(
        minutes=15
    ):
        raise ValueError("policy audit TTL exceeds fifteen minutes")
    if not (
        issued <= now < expires
        and audit_emitted <= now < audit_expires
        and now < candidate_expires
    ):
        raise ValueError(
            "authorization, audit, or candidate evidence is not currently fresh"
        )


class ApiClient:
    def __init__(self, base: str, token: str | None, *, test_api: bool) -> None:
        parsed = urlparse(base)
        if test_api:
            if parsed.scheme != "http" or parsed.hostname not in {
                "127.0.0.1",
                "localhost",
                "::1",
            }:
                raise ValueError("test API must be one loopback HTTP endpoint")
        elif base != API_ROOT:
            raise ValueError(
                "production API root must be exactly https://api.github.com"
            )
        self.base = base.rstrip("/")
        self.upload_base = self.base if test_api else UPLOAD_ROOT
        self.token = token
        self.opener = build_opener(NoRedirect())
        self.patch_count = 0

    def request(
        self,
        method: str,
        path: str,
        *,
        data: bytes | None = None,
        content_type: str = "application/json",
        statuses: set[int] | None = None,
    ) -> tuple[int, dict[str, str], Any]:
        if method not in {"GET", "POST", "PATCH"}:
            raise ValueError(f"forbidden GitHub API method: {method}")
        if method == "PATCH":
            self.patch_count += 1
            if self.patch_count != 1:
                raise ValueError("GitHub Release publication permits exactly one PATCH")
        headers = {
            "Accept": "application/vnd.github+json",
            "User-Agent": "rmux-release-publisher",
            "X-GitHub-Api-Version": API_VERSION,
        }
        if self.token is not None:
            headers["Authorization"] = f"Bearer {self.token}"
        if data is not None:
            headers["Content-Type"] = content_type
        request = Request(
            f"{self.base}{path}", data=data, method=method, headers=headers
        )
        try:
            response = self.opener.open(request, timeout=30)
        except HTTPError as error:
            if statuses is not None and error.code in statuses:
                response = error
            else:
                raise ValueError(
                    f"GitHub API {method} {path} failed closed with HTTP {error.code}"
                ) from error
        except (URLError, TimeoutError) as error:
            raise ValueError(f"GitHub API {method} {path} failed closed") from error
        with response:
            payload = response.read(MAX_RESPONSE_BYTES + 1)
            status = response.status
            response_headers = {
                key.lower(): value for key, value in response.headers.items()
            }
        allowed = statuses or {200}
        if status not in allowed:
            raise ValueError(f"GitHub API {method} {path} returned HTTP {status}")
        if len(payload) > MAX_RESPONSE_BYTES:
            raise ValueError("GitHub API response exceeded the size limit")
        if not payload:
            return status, response_headers, None
        try:
            return status, response_headers, json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError("GitHub API did not return UTF-8 JSON") from error

    def get_release(self, release_ref: str) -> dict[str, Any] | None:
        path = f"/repos/{REPOSITORY}/releases/tags/{quote(release_ref, safe='')}"
        status, _, value = self.request("GET", path, statuses={200, 404})
        if status == 404:
            return None
        if not isinstance(value, dict):
            raise ValueError("GitHub release response is not an object")
        return value

    def list_assets(self, release_id: int) -> list[dict[str, Any]]:
        _, headers, value = self.request(
            "GET", f"/repos/{REPOSITORY}/releases/{release_id}/assets?per_page=100"
        )
        if headers.get("link"):
            raise ValueError("GitHub release asset set exceeds one exact page")
        if not isinstance(value, list) or not all(
            isinstance(item, dict) for item in value
        ):
            raise ValueError("GitHub release assets response is invalid")
        return value

    def create_draft(self, evidence: Any) -> dict[str, Any]:
        payload = json.dumps(
            {
                "tag_name": evidence.release_ref,
                "target_commitish": evidence.source_sha,
                "name": evidence.title,
                "body": evidence.notes,
                "draft": True,
                "prerelease": evidence.prerelease,
                "generate_release_notes": False,
                "make_latest": "false" if evidence.prerelease else "true",
            },
            separators=(",", ":"),
        ).encode()
        _, _, value = self.request(
            "POST", f"/repos/{REPOSITORY}/releases", data=payload, statuses={201}
        )
        if not isinstance(value, dict):
            raise ValueError("created GitHub draft response is invalid")
        return value

    def upload(self, release_id: int, asset: Any) -> None:
        query = urlencode({"name": asset.name})
        path = f"/repos/{REPOSITORY}/releases/{release_id}/assets?{query}"
        original = self.base
        self.base = self.upload_base
        try:
            _, _, value = self.request(
                "POST",
                path,
                data=asset.path.read_bytes(),
                content_type="application/octet-stream",
                statuses={201},
            )
        finally:
            self.base = original
        if (
            not isinstance(value, dict)
            or value.get("name") != asset.name
            or value.get("size") != asset.size
            or value.get("digest") != f"sha256:{asset.sha256}"
        ):
            raise ValueError(f"GitHub uploaded asset identity changed: {asset.name}")

    def publish(self, release_id: int, *, prerelease: bool) -> dict[str, Any]:
        payload = json.dumps(
            {"draft": False, "make_latest": "false" if prerelease else "true"},
            separators=(",", ":"),
        ).encode()
        _, _, value = self.request(
            "PATCH",
            f"/repos/{REPOSITORY}/releases/{release_id}",
            data=payload,
        )
        if not isinstance(value, dict):
            raise ValueError("published GitHub release response is invalid")
        return value


def release_identity(release: dict[str, Any], evidence: Any, *, draft: bool) -> int:
    release_id = positive(release.get("id"), "GitHub release ID")
    expected = {
        "tag_name": evidence.release_ref,
        "target_commitish": evidence.source_sha,
        "name": evidence.title,
        "body": evidence.notes,
        "draft": draft,
        "prerelease": evidence.prerelease,
    }
    if any(release.get(key) != value for key, value in expected.items()):
        raise ValueError("GitHub release identity differs from the authorized draft")
    return release_id


def inspect_assets(
    actual: list[dict[str, Any]], evidence: Any, *, complete: bool
) -> list[Any]:
    expected = {asset.name: asset for asset in evidence.assets}
    seen: set[str] = set()
    for item in actual:
        name = item.get("name")
        if not isinstance(name, str) or name in seen or name not in expected:
            raise ValueError("GitHub draft contains an extra or duplicated asset")
        seen.add(name)
        asset = expected[name]
        if (
            item.get("state") != "uploaded"
            or item.get("size") != asset.size
            or item.get("digest") != f"sha256:{asset.sha256}"
        ):
            raise ValueError(
                f"GitHub draft asset differs from authorized bytes: {name}"
            )
    missing = [asset for asset in evidence.assets if asset.name not in seen]
    if complete and missing:
        raise ValueError("GitHub draft is missing authorized assets")
    return missing


def verify_live_evidence(client: ApiClient, evidence: Any, now: datetime) -> None:
    validate_freshness(evidence, now)
    tag = evidence.predicate["signed_tag"]
    _, _, ref = client.request(
        "GET",
        f"/repos/{REPOSITORY}/git/ref/tags/{quote(evidence.release_ref, safe='')}",
    )
    ref_object = ref.get("object") if isinstance(ref, dict) else None
    if (
        not isinstance(ref, dict)
        or ref.get("ref") != f"refs/tags/{evidence.release_ref}"
        or not isinstance(ref_object, dict)
        or ref_object.get("type") != "tag"
        or ref_object.get("sha") != tag["tag_object_sha"]
    ):
        raise ValueError("live release ref does not point to the authorized tag object")
    _, _, tag_object = client.request(
        "GET", f"/repos/{REPOSITORY}/git/tags/{tag['tag_object_sha']}"
    )
    verification = (
        tag_object.get("verification") if isinstance(tag_object, dict) else None
    )
    if (
        not isinstance(tag_object, dict)
        or tag_object.get("tag") != evidence.release_ref
        or tag_object.get("object", {}).get("type") != "commit"
        or tag_object.get("object", {}).get("sha") != evidence.source_sha
        or not isinstance(verification, dict)
        or verification.get("verified") is not True
        or verification.get("reason") != "valid"
    ):
        raise ValueError("live annotated tag signature or target changed")
    audit = evidence.predicate["policy_audit"]
    authorization = evidence.predicate["authorization"]
    _, _, run = client.request(
        "GET", f"/repos/{REPOSITORY}/actions/runs/{audit['policy_audit_run_id']}"
    )
    if (
        not isinstance(run, dict)
        or run.get("id") != audit["policy_audit_run_id"]
        or run.get("run_attempt") != 1
        or run.get("workflow_id") != authorization["workflow_id"]
        or run.get("path") != authorization["workflow_path"]
        or run.get("head_sha") != evidence.source_sha
        or run.get("status") != "in_progress"
        or run.get("conclusion") is not None
        or run.get("repository", {}).get("id") != REPOSITORY_ID
    ):
        raise ValueError("live policy audit run identity changed")
    _, _, artifact = client.request(
        "GET", f"/repos/{REPOSITORY}/actions/artifacts/{audit['predicate_artifact_id']}"
    )
    if (
        not isinstance(artifact, dict)
        or artifact.get("id") != audit["predicate_artifact_id"]
        or artifact.get("expired") is not False
        or artifact.get("digest") != audit["predicate_artifact_digest"]
        or artifact.get("workflow_run", {}).get("id") != audit["policy_audit_run_id"]
        or artifact.get("workflow_run", {}).get("head_sha") != evidence.source_sha
    ):
        raise ValueError("live policy audit artifact identity changed")


def verify_live_inputs(
    client: ApiClient, evidence: Any, now: datetime, release_id: int
) -> None:
    verify_live_evidence(client, evidence, now)
    current = client.get_release(evidence.release_ref)
    if current is None or release_identity(current, evidence, draft=True) != release_id:
        raise ValueError("GitHub draft changed immediately before publication")
    inspect_assets(client.list_assets(release_id), evidence, complete=True)
