"""Plan bounded GitHub-signed commits for one downstream repository update."""

from __future__ import annotations

import base64
import hashlib
import json
from typing import Any


CREATE_COMMIT_MUTATION = """
mutation CreateSignedCommit($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) {
    commit {
      oid
      signature {
        isValid
        state
        wasSignedByGitHub
      }
    }
    ref {
      target {
        oid
      }
    }
  }
}
"""

GRAPHQL_COMMIT_MAX_BYTES = 44_000_000
STAGING_BRANCH_PREFIX = "rmux-release-stage"


def commit_variables(
    *,
    full_name: str,
    branch: str,
    base_commit: str,
    additions: dict[str, bytes],
    deletions: set[str],
    message: str,
) -> dict[str, Any]:
    return {
        "input": {
            "branch": {
                "repositoryNameWithOwner": full_name,
                "branchName": branch,
            },
            "expectedHeadOid": base_commit,
            "message": {"headline": message},
            "fileChanges": {
                "additions": [
                    {
                        "path": path,
                        "contents": base64.b64encode(contents).decode("ascii"),
                    }
                    for path, contents in sorted(additions.items())
                ],
                "deletions": [{"path": path} for path in sorted(deletions)],
            },
        }
    }


def commit_request_size(
    *,
    full_name: str,
    branch: str,
    base_commit: str,
    additions: dict[str, bytes],
    deletions: set[str],
    message: str,
) -> int:
    variables = commit_variables(
        full_name=full_name,
        branch=branch,
        base_commit=base_commit,
        additions=additions,
        deletions=deletions,
        message=message,
    )
    return len(
        json.dumps(
            {"query": CREATE_COMMIT_MUTATION, "variables": variables},
            separators=(",", ":"),
        ).encode()
    )


def signed_commit_batches(
    *,
    full_name: str,
    branch: str,
    base_commit: str,
    additions: dict[str, bytes],
    deletions: set[str],
    message: str,
) -> list[tuple[dict[str, bytes], set[str]]]:
    batches: list[tuple[dict[str, bytes], set[str]]] = []
    current: dict[str, bytes] = {}
    pending_deletions = set(deletions)
    sizing_message = f"{message} (part 999/999)"
    for path, contents in sorted(additions.items()):
        candidate = {**current, path: contents}
        size = commit_request_size(
            full_name=full_name,
            branch=branch,
            base_commit=base_commit,
            additions=candidate,
            deletions=pending_deletions,
            message=sizing_message,
        )
        if size <= GRAPHQL_COMMIT_MAX_BYTES:
            current = candidate
            continue
        if not current:
            raise ValueError(
                f"downstream file exceeds GitHub's signed commit limit: {path}"
            )
        batches.append((current, pending_deletions))
        current = {path: contents}
        pending_deletions = set()
        if (
            commit_request_size(
                full_name=full_name,
                branch=branch,
                base_commit=base_commit,
                additions=current,
                deletions=pending_deletions,
                message=sizing_message,
            )
            > GRAPHQL_COMMIT_MAX_BYTES
        ):
            raise ValueError(
                f"downstream file exceeds GitHub's signed commit limit: {path}"
            )
    if current or pending_deletions:
        batches.append((current, pending_deletions))
    if not batches:
        raise ValueError("downstream repository update has no file changes")
    return batches


def staging_branch_name(
    base_commit: str, updates: dict[str, bytes], deletions: set[str]
) -> str:
    digest = hashlib.sha256()
    digest.update(base_commit.encode())
    for path, contents in sorted(updates.items()):
        digest.update(b"A\0")
        digest.update(path.encode())
        digest.update(b"\0")
        digest.update(hashlib.sha256(contents).digest())
    for path in sorted(deletions):
        digest.update(b"D\0")
        digest.update(path.encode())
        digest.update(b"\0")
    return f"{STAGING_BRANCH_PREFIX}-{base_commit[:12]}-{digest.hexdigest()[:16]}"
