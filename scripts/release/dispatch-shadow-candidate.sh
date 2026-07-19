#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: dispatch-shadow-candidate.sh [--execute] \
  --repository OWNER/REPO --expected-source-sha SHA \
  --fast-run-id ID --release-intent-id ID --planned-release-ref vX.Y.Z[-rc.N]

Without --execute, prints the exact dispatch request and performs no mutation.
This command never creates or pushes a tag and never publishes a Release.
EOF
}

execute=false
repository=''
expected_source_sha=''
fast_run_id=''
release_intent_id=''
planned_release_ref=''

while (($#)); do
  case "$1" in
    --execute) execute=true; shift ;;
    --repository) repository=${2-}; shift 2 ;;
    --expected-source-sha) expected_source_sha=${2-}; shift 2 ;;
    --fast-run-id) fast_run_id=${2-}; shift 2 ;;
    --release-intent-id) release_intent_id=${2-}; shift 2 ;;
    --planned-release-ref) planned_release_ref=${2-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ $repository =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]] || { echo "invalid repository" >&2; exit 2; }
[[ $expected_source_sha =~ ^[0-9a-f]{40}$ ]] || { echo "invalid expected source SHA" >&2; exit 2; }
[[ $fast_run_id =~ ^[1-9][0-9]*$ ]] || { echo "invalid fast run ID" >&2; exit 2; }
[[ $release_intent_id =~ ^[A-Za-z0-9._:-]{8,128}$ ]] || { echo "invalid release intent ID" >&2; exit 2; }
[[ $planned_release_ref =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || {
  echo "invalid planned release ref" >&2
  exit 2
}

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
    --arg fast_run_id "$fast_run_id" \
    --arg release_intent_id "$release_intent_id" \
    --arg planned_release_ref "$planned_release_ref" \
    --argjson payload "$payload" \
    '{mode:"dry-run",method:$method,api_version:$api_version,endpoint:$endpoint,
      expected_source_sha:$expected_source_sha,fast_run_id:($fast_run_id|tonumber),
      release_intent_id:$release_intent_id,planned_release_ref:$planned_release_ref,
      payload:$payload}'
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
candidate_run_id=$(jq -er '.workflow_run_id | select(type == "number" and . > 0)' <<<"$response")

run=$(gh api \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/actions/runs/$candidate_run_id")
[[ $(jq -r '.head_sha' <<<"$run") == "$expected_source_sha" ]] || {
  echo "dispatched run resolved a different SHA; candidate is rejected" >&2
  exit 1
}
[[ $(jq -r '.event' <<<"$run") == workflow_dispatch ]] || {
  echo "dispatched run has the wrong event" >&2
  exit 1
}
[[ $(jq -r '.run_attempt' <<<"$run") == 1 ]] || {
  echo "dispatched run is not attempt 1" >&2
  exit 1
}

jq -n \
  --argjson candidate_run_id "$candidate_run_id" \
  --arg run_url "$(jq -r '.url' <<<"$run")" \
  --arg html_url "$(jq -r '.html_url' <<<"$run")" \
  --arg expected_source_sha "$expected_source_sha" \
  --arg fast_run_id "$fast_run_id" \
  --arg release_intent_id "$release_intent_id" \
  --arg planned_release_ref "$planned_release_ref" \
  '{candidate_run_id:$candidate_run_id,run_url:$run_url,html_url:$html_url,
    expected_source_sha:$expected_source_sha,fast_run_id:($fast_run_id|tonumber),
    release_intent_id:$release_intent_id,planned_release_ref:$planned_release_ref}'
