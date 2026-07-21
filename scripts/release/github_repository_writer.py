"""Minimal GitHub Git Data API client for one atomic repository update."""

from __future__ import annotations

import base64
import json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class PublishOutcome:
    state: str
    mutation_started: bool
    commit_sha: str


class GitHubApi:
    def __init__(self, token: str) -> None:
        if not token or "\n" in token or "\r" in token:
            raise ValueError("GitHub App token is missing or malformed")
        self._token = token

    def request(
        self, method: str, path: str, payload: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if not path.startswith("/") or "//" in path:
            raise ValueError("GitHub API path is invalid")
        data = None
        if payload is not None:
            data = json.dumps(payload, separators=(",", ":")).encode()
        request = urllib.request.Request(
            f"https://api.github.com{path}",
            data=data,
            method=method,
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "rmux-release-writer/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as error:
            detail = error.read(4096).decode("utf-8", errors="replace")
            raise ValueError(
                f"GitHub API {method} {path} failed: {error.code} {detail}"
            ) from error
        if len(raw) > 8 * 1024 * 1024:
            raise ValueError("GitHub API response exceeds the release limit")
        try:
            value = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError("GitHub API returned invalid JSON") from error
        if not isinstance(value, dict):
            raise ValueError("GitHub API returned a non-object")
        return value

    def get(self, path: str) -> dict[str, Any]:
        return self.request("GET", path)

    def get_bytes(self, path: str, *, limit: int) -> bytes:
        if not path.startswith("/") or "//" in path or limit <= 0:
            raise ValueError("GitHub raw API request is invalid")
        request = urllib.request.Request(
            f"https://api.github.com{path}",
            method="GET",
            headers={
                "Accept": "application/vnd.github.raw+json",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "rmux-release-writer/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read(limit + 1)
        except urllib.error.HTTPError as error:
            detail = error.read(4096).decode("utf-8", errors="replace")
            raise ValueError(
                f"GitHub API GET {path} failed: {error.code} {detail}"
            ) from error
        if len(raw) > limit:
            raise ValueError("GitHub raw file exceeds the release limit")
        return raw

    def post(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        return self.request("POST", path, payload)

    def patch(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        return self.request("PATCH", path, payload)


def object_sha(value: dict[str, Any], label: str) -> str:
    sha = value.get("sha")
    if not isinstance(sha, str) or len(sha) != 40:
        raise ValueError(f"GitHub {label} has no canonical SHA")
    return sha


def repository_identity(
    api: GitHubApi, full_name: str, repository_id: int, default_branch: str
) -> None:
    value = api.get(f"/repos/{full_name}")
    if (
        value.get("id") != repository_id
        or value.get("full_name") != full_name
        or value.get("visibility") != "public"
        or value.get("default_branch") != default_branch
        or value.get("archived") is not False
    ):
        raise ValueError("downstream repository identity changed")


def branch_head(api: GitHubApi, full_name: str, branch: str) -> str:
    value = api.get(f"/repos/{full_name}/git/ref/heads/{branch}")
    target = value.get("object")
    if not isinstance(target, dict) or target.get("type") != "commit":
        raise ValueError("downstream branch does not resolve to one commit")
    return object_sha(target, "branch")


def file_at(api: GitHubApi, full_name: str, path: str, ref: str) -> bytes | None:
    encoded_path = urllib.parse.quote(path, safe="/")
    encoded_ref = urllib.parse.quote(ref, safe="")
    try:
        return api.get_bytes(
            f"/repos/{full_name}/contents/{encoded_path}?ref={encoded_ref}",
            limit=64 * 1024 * 1024,
        )
    except ValueError as error:
        if "failed: 404 " in str(error):
            return None
        raise


def tree_paths(api: GitHubApi, full_name: str, commit_sha: str) -> set[str]:
    value = api.get(f"/repos/{full_name}/git/trees/{commit_sha}?recursive=1")
    if value.get("truncated") is not False or not isinstance(value.get("tree"), list):
        raise ValueError("downstream repository tree is missing or truncated")
    paths: set[str] = set()
    for entry in value["tree"]:
        if not isinstance(entry, dict) or entry.get("type") != "blob":
            continue
        path = entry.get("path")
        if not isinstance(path, str) or not path or path in paths:
            raise ValueError("downstream repository tree path is invalid")
        paths.add(path)
    return paths


def publish(
    api: GitHubApi,
    *,
    full_name: str,
    branch: str,
    updates: dict[str, bytes],
    message: str,
    managed_prefixes: tuple[str, ...] = (),
    expected_base: str | None = None,
) -> PublishOutcome:
    base_commit = branch_head(api, full_name, branch)
    if expected_base is not None and base_commit != expected_base:
        raise ValueError("downstream repository changed after payload preparation")
    existing_paths = (
        tree_paths(api, full_name, base_commit) if managed_prefixes else set()
    )
    managed_existing = {
        path
        for path in existing_paths
        if any(
            path == prefix or path.startswith(f"{prefix}/")
            for prefix in managed_prefixes
        )
    }
    managed_expected = {
        path
        for path in updates
        if any(
            path == prefix or path.startswith(f"{prefix}/")
            for prefix in managed_prefixes
        )
    }
    if managed_prefixes and not managed_expected:
        raise ValueError("managed repository update has no files under its prefixes")
    if managed_existing == managed_expected and all(
        file_at(api, full_name, path, base_commit) == data
        for path, data in updates.items()
    ):
        return PublishOutcome("no-op-exact", False, base_commit)
    commit = api.get(f"/repos/{full_name}/git/commits/{base_commit}")
    tree = commit.get("tree")
    if not isinstance(tree, dict):
        raise ValueError("downstream base commit has no tree")
    base_tree = object_sha(tree, "base tree")
    entries: list[dict[str, Any]] = [
        {"path": path, "mode": "100644", "type": "blob", "sha": None}
        for path in sorted(managed_existing - managed_expected)
    ]
    for path, data in sorted(updates.items()):
        blob = api.post(
            f"/repos/{full_name}/git/blobs",
            {"content": base64.b64encode(data).decode("ascii"), "encoding": "base64"},
        )
        entries.append(
            {
                "path": path,
                "mode": "100644",
                "type": "blob",
                "sha": object_sha(blob, "blob"),
            }
        )
    created_tree = api.post(
        f"/repos/{full_name}/git/trees", {"base_tree": base_tree, "tree": entries}
    )
    created_commit = api.post(
        f"/repos/{full_name}/git/commits",
        {
            "message": message,
            "tree": object_sha(created_tree, "created tree"),
            "parents": [base_commit],
        },
    )
    commit_sha = object_sha(created_commit, "created commit")
    api.patch(
        f"/repos/{full_name}/git/refs/heads/{branch}",
        {"sha": commit_sha, "force": False},
    )
    if branch_head(api, full_name, branch) != commit_sha:
        raise ValueError("downstream branch did not advance to the exact commit")
    for path, expected in updates.items():
        if file_at(api, full_name, path, commit_sha) != expected:
            raise ValueError("downstream repository bytes differ after publication")
    if managed_prefixes:
        final_paths = tree_paths(api, full_name, commit_sha)
        managed_final = {
            path
            for path in final_paths
            if any(
                path == prefix or path.startswith(f"{prefix}/")
                for prefix in managed_prefixes
            )
        }
        if managed_final != managed_expected:
            raise ValueError("managed repository paths differ after publication")
    return PublishOutcome("public-live", True, commit_sha)
