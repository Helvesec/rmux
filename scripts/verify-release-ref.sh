#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <owner/repository> <release-tag> <expected-commit-sha>" >&2
  exit 2
fi

repository=$1
release_tag=$2
expected_sha=$(printf '%s' "$3" | tr '[:upper:]' '[:lower:]')

if [[ ! $repository =~ ^[^/]+/[^/]+$ ]]; then
  echo "invalid GitHub repository: $repository" >&2
  exit 2
fi
if [[ ! $release_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid release tag: $release_tag" >&2
  exit 2
fi
if [[ ! $expected_sha =~ ^[0-9a-f]{40}$ ]]; then
  echo "expected commit must be a full 40-character SHA-1" >&2
  exit 2
fi
command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

encoded_tag=$(jq -rn --arg value "$release_tag" '$value | @uri')
ref_json=$(gh api \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' \
  "repos/$repository/git/ref/tags/$encoded_tag")
object_type=$(jq -er '.object.type' <<<"$ref_json")
object_sha=$(jq -er '.object.sha | ascii_downcase' <<<"$ref_json")

# Release provenance is a verified annotated tag, never a lightweight ref.
# Peel every signed layer and reject cycles or unexpected object types instead
# of trusting the mutable ref name.
if [[ $object_type != tag ]]; then
  echo "release ref $release_tag must be a verified signed annotated tag" >&2
  exit 1
fi
visited=''
for _ in $(seq 1 16); do
  if [[ $object_type == commit ]]; then
    break
  fi
  if [[ $object_type != tag ]]; then
    echo "release ref $release_tag resolves to unsupported object type $object_type" >&2
    exit 1
  fi
  if [[ " $visited " == *" $object_sha "* ]]; then
    echo "cycle while peeling annotated release tag $release_tag" >&2
    exit 1
  fi
  visited="$visited $object_sha"
  tag_json=$(gh api \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    "repos/$repository/git/tags/$object_sha")
  if ! jq -e '.verification.verified == true' <<<"$tag_json" >/dev/null; then
    verification_reason=$(jq -r '.verification.reason // "missing"' <<<"$tag_json")
    echo "release tag $release_tag has no verified signature (reason: $verification_reason)" >&2
    exit 1
  fi
  object_type=$(jq -er '.object.type' <<<"$tag_json")
  object_sha=$(jq -er '.object.sha | ascii_downcase' <<<"$tag_json")
done

if [[ $object_type != commit ]]; then
  echo "annotated release tag $release_tag exceeded the peel depth" >&2
  exit 1
fi
if [[ $object_sha != "$expected_sha" ]]; then
  echo "release tag $release_tag resolves to $object_sha, expected $expected_sha" >&2
  exit 1
fi

release_json_file=$(mktemp)
trap 'rm -f "$release_json_file"' EXIT
if gh api \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' \
  "repos/$repository/releases/tags/$encoded_tag" >"$release_json_file" 2>/dev/null; then
  release_tag_name=$(jq -er '.tag_name' <"$release_json_file")
  release_target=$(jq -er '.target_commitish | ascii_downcase' <"$release_json_file")
  if [[ $release_tag_name != "$release_tag" ]]; then
    echo "GitHub Release tag_name $release_tag_name does not match $release_tag" >&2
    exit 1
  fi
  if [[ $release_target != "$expected_sha" ]]; then
    echo "existing GitHub Release target_commitish $release_target does not match $expected_sha" >&2
    exit 1
  fi
fi

echo "release-ref-ok tag=$release_tag commit=$expected_sha"
