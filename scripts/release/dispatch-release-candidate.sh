#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: dispatch-release-candidate.sh [--execute] --fast-run-id ID \
  --expected-source-sha SHA --release-intent-id ID --planned-release-ref TAG \
  --release-kind shadow|rc|stable

Dry-run is the default. Execution only dispatches the read-only/non-publishing
candidate route in CI; it never creates a tag, Release, or package publication.
EOF
}

execute=false
repository=Helvesec/rmux
fast_run_id=''
expected_source_sha=''
release_intent_id=''
planned_release_ref=''
release_kind=''

while (($#)); do
  case "$1" in
    --execute) execute=true; shift ;;
    --repository) repository=${2-}; shift 2 ;;
    --fast-run-id) fast_run_id=${2-}; shift 2 ;;
    --expected-source-sha) expected_source_sha=${2-}; shift 2 ;;
    --release-intent-id) release_intent_id=${2-}; shift 2 ;;
    --planned-release-ref) planned_release_ref=${2-}; shift 2 ;;
    --release-kind) release_kind=${2-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ $repository == Helvesec/rmux ]] || {
  echo "repository must be exactly Helvesec/rmux" >&2
  exit 2
}
[[ $fast_run_id =~ ^[1-9][0-9]*$ ]] || { echo "invalid fast run ID" >&2; exit 2; }
[[ $expected_source_sha =~ ^[0-9a-f]{40}$ ]] || {
  echo "invalid expected source SHA" >&2
  exit 2
}
[[ $release_intent_id =~ ^[A-Za-z0-9._:-]{8,128}$ ]] || {
  echo "invalid release intent ID" >&2
  exit 2
}
[[ $planned_release_ref =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || {
  echo "invalid planned release ref" >&2
  exit 2
}
case "$release_kind" in
  shadow|rc|stable) ;;
  *) echo "invalid release kind" >&2; exit 2 ;;
esac

if [[ $execute != true ]]; then
  # All interpolated fields have already passed strict character allowlists.
  # Keep the default dry-run usable on every test runner without requiring jq.
  cat <<EOF
{
  "mode": "dry-run",
  "method": "POST",
  "endpoint": "repos/$repository/actions/workflows/ci.yml/dispatches",
  "expected_source_sha": "$expected_source_sha",
  "payload": {
    "ref": "main",
    "return_run_details": true,
    "inputs": {
      "release_qualification": true,
      "fast_run_id": "$fast_run_id",
      "expected_source_sha": "$expected_source_sha",
      "release_intent_id": "$release_intent_id",
      "planned_release_ref": "$planned_release_ref",
      "release_kind": "$release_kind"
    }
  },
  "publication_authority": false
}
EOF
  exit 0
fi

command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required for execution" >&2; exit 2; }
payload=$(jq -cn \
  --arg ref main \
  --arg fast_run_id "$fast_run_id" \
  --arg expected_source_sha "$expected_source_sha" \
  --arg release_intent_id "$release_intent_id" \
  --arg planned_release_ref "$planned_release_ref" \
  --arg release_kind "$release_kind" \
  --argjson return_run_details true '
  {
    ref: $ref,
    return_run_details: $return_run_details,
    inputs: {
      release_qualification: true,
      fast_run_id: $fast_run_id,
      expected_source_sha: $expected_source_sha,
      release_intent_id: $release_intent_id,
      planned_release_ref: $planned_release_ref,
      release_kind: $release_kind
    }
  }')
api_headers=(
  -H 'Accept: application/vnd.github+json'
  -H 'X-GitHub-Api-Version: 2026-03-10'
)
remote_sha=$(gh api "${api_headers[@]}" \
  "repos/$repository/git/ref/heads/main" --jq '.object.sha')
[[ $remote_sha == "$expected_source_sha" ]] || {
  echo "main moved before dispatch: expected $expected_source_sha, got $remote_sha" >&2
  exit 1
}

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/../.." && pwd)
GH_TOKEN=${GH_TOKEN:-$(gh auth token)} \
  "$script_dir/verify-fast-run.py" \
  --repository "$repository" \
  --run-id "$fast_run_id" \
  --expected-source-sha "$expected_source_sha" \
  --kind fast \
  --repository-root "$repo_root" >/dev/null

response=$(gh api "${api_headers[@]}" --method POST \
  "repos/$repository/actions/workflows/ci.yml/dispatches" \
  --input - <<<"$payload")
candidate_run_id=$(jq -er '.workflow_run_id | select(type == "number" and . > 0)' \
  <<<"$response")

run=''
for _ in 1 2 3 4 5 6; do
  if run=$(gh api "${api_headers[@]}" \
    "repos/$repository/actions/runs/$candidate_run_id" 2>/dev/null); then
    break
  fi
  sleep 2
done
[[ -n $run ]] || {
  echo "dispatched candidate run did not become readable" >&2
  exit 1
}
jq -e \
  --arg repository "$repository" \
  --arg expected_source_sha "$expected_source_sha" \
  --argjson candidate_run_id "$candidate_run_id" '
  .id == $candidate_run_id and
  .workflow_id == 277622540 and
  .path == ".github/workflows/ci.yml" and
  .name == "CI" and
  .event == "workflow_dispatch" and
  .head_branch == "main" and
  .head_sha == $expected_source_sha and
  .run_attempt == 1 and
  .repository.id == 1239918790 and
  .repository.full_name == $repository and
  .head_repository.id == 1239918790 and
  .head_repository.full_name == $repository
  ' <<<"$run" >/dev/null || {
  echo "dispatched candidate identity is not exact" >&2
  exit 1
}

jq -n \
  --argjson candidate_run_id "$candidate_run_id" \
  --arg expected_source_sha "$expected_source_sha" \
  --arg html_url "$(jq -r '.html_url' <<<"$run")" '
  {
    candidate_run_id: $candidate_run_id,
    expected_source_sha: $expected_source_sha,
    html_url: $html_url,
    publication_authority: false
  }'
