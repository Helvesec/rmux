#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <owner/repository>" >&2
  exit 2
fi
repository=$1
if [[ ! $repository =~ ^[^/]+/[^/]+$ ]]; then
  echo "invalid GitHub repository: $repository" >&2
  exit 2
fi
command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

summaries=$(gh api --paginate \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' \
  "repos/$repository/rulesets?includes_parents=true&targets=tag&per_page=100")
ruleset_ids=$(jq -rs '
  [ .[] | if type == "array" then .[] else . end
    | select(.target == "tag" and .enforcement == "active")
    | .id ] | .[]
' <<<"$summaries")

for ruleset_id in $ruleset_ids; do
  detail=$(gh api \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    "repos/$repository/rulesets/$ruleset_id")
  if jq -e '
    .target == "tag" and
    .enforcement == "active" and
    ((.bypass_actors // []) | length == 0) and
    ((.conditions.ref_name.include // []) | any(. == "refs/tags/v*" or . == "refs/tags/v**" or . == "~ALL")) and
    ((.conditions.ref_name.exclude // []) | length == 0) and
    ([.rules[].type] | index("update") != null) and
    ([.rules[].type] | index("deletion") != null)
  ' <<<"$detail" >/dev/null; then
    name=$(jq -r '.name' <<<"$detail")
    echo "release-tag-protection-ok ruleset=$ruleset_id name=$name"
    exit 0
  fi
done

echo "no active non-bypassable tag ruleset blocks update and deletion for refs/tags/v*" >&2
exit 1
