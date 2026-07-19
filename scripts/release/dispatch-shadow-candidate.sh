#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: dispatch-shadow-candidate.sh [--execute] \
  --repository OWNER/REPO --expected-source-sha SHA

Without --execute, prints the exact dispatch request and performs no mutation.
This command starts only the existing release qualification. It does not bind
a fast run, release intent, or planned tag, and never publishes a Release.
EOF
}

execute=false
repository=''
expected_source_sha=''

while (($#)); do
  case "$1" in
    --execute) execute=true; shift ;;
    --repository) repository=${2-}; shift 2 ;;
    --expected-source-sha) expected_source_sha=${2-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ $repository =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]] || { echo "invalid repository" >&2; exit 2; }
[[ $expected_source_sha =~ ^[0-9a-f]{40}$ ]] || { echo "invalid expected source SHA" >&2; exit 2; }
payload=$(jq -cn \
  --arg ref main \
  --argjson return_run_details true \
  '{ref:$ref,return_run_details:$return_run_details,inputs:{release_qualification:true}}')

if [[ $execute != true ]]; then
  jq -n \
    --arg method POST \
    --arg api_version 2026-03-10 \
    --arg endpoint "repos/$repository/actions/workflows/ci.yml/dispatches" \
    --arg expected_source_sha "$expected_source_sha" \
    --argjson payload "$payload" \
    '{mode:"dry-run",method:$method,api_version:$api_version,endpoint:$endpoint,
      expected_source_sha:$expected_source_sha,payload:$payload,
      binding:"none-baseline-qualification-only"}'
  exit 0
fi

command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }
remote_sha=$(gh api \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/git/ref/heads/main" --jq '.object.sha')
[[ $remote_sha == "$expected_source_sha" ]] || {
  echo "main moved before dispatch: expected $expected_source_sha, got $remote_sha" >&2
  exit 1
}

response=$(gh api --method POST \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/actions/workflows/ci.yml/dispatches" \
  --input - <<<"$payload")
qualification_run_id=$(jq -er '.workflow_run_id | select(type == "number" and . > 0)' <<<"$response")

run=''
for _ in 1 2 3 4 5 6; do
  if run=$(gh api \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2026-03-10' \
    "repos/$repository/actions/runs/$qualification_run_id" 2>/dev/null); then
    break
  fi
  sleep 2
done
[[ -n $run ]] || { echo "dispatched qualification run did not become readable" >&2; exit 1; }
jq -e \
  --arg repository "$repository" \
  --arg expected_source_sha "$expected_source_sha" '
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
  echo "dispatched qualification run identity is not exact" >&2
  exit 1
}

jq -n \
  --argjson qualification_run_id "$qualification_run_id" \
  --arg run_url "$(jq -r '.url' <<<"$run")" \
  --arg html_url "$(jq -r '.html_url' <<<"$run")" \
  --arg expected_source_sha "$expected_source_sha" \
  '{qualification_run_id:$qualification_run_id,run_url:$run_url,html_url:$html_url,
    expected_source_sha:$expected_source_sha,
    binding:"none-baseline-qualification-only"}'
