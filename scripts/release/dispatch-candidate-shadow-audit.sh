#!/usr/bin/env bash
set -euo pipefail

repository=Helvesec/rmux
candidate_run_id=
expected_source_sha=
release_intent_id=
planned_release_ref=
release_kind=
execute=0

usage() {
  cat <<'USAGE'
Usage: dispatch-candidate-shadow-audit.sh [options]

Seal an explicitly selected, already-completed candidate run. The default is
a local dry-run; --execute performs one workflow_dispatch and nothing public.

  --candidate-run-id <id>
  --expected-source-sha <40-hex>
  --release-intent-id <id>
  --planned-release-ref <vX.Y.Z[-rc.N]>
  --release-kind shadow|rc|stable
  --repository Helvesec/rmux
  --execute
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate-run-id) candidate_run_id=${2-}; shift 2 ;;
    --expected-source-sha) expected_source_sha=${2-}; shift 2 ;;
    --release-intent-id) release_intent_id=${2-}; shift 2 ;;
    --planned-release-ref) planned_release_ref=${2-}; shift 2 ;;
    --release-kind) release_kind=${2-}; shift 2 ;;
    --repository) repository=${2-}; shift 2 ;;
    --execute) execute=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Reject repository redirection before any API request, including the POST.
[[ $repository == Helvesec/rmux ]] || {
  echo "repository must be exactly Helvesec/rmux" >&2
  exit 2
}
[[ $candidate_run_id =~ ^[1-9][0-9]*$ ]] || { echo "invalid candidate run ID" >&2; exit 2; }
[[ $expected_source_sha =~ ^[0-9a-f]{40}$ ]] || { echo "invalid source SHA" >&2; exit 2; }
[[ $release_intent_id =~ ^[A-Za-z0-9._:-]{8,128}$ ]] || { echo "invalid release intent ID" >&2; exit 2; }
[[ $planned_release_ref =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || {
  echo "invalid planned release ref" >&2
  exit 2
}
[[ $release_kind == shadow || $release_kind == rc || $release_kind == stable ]] || {
  echo "invalid release kind" >&2
  exit 2
}

payload=$(python3 - "$candidate_run_id" "$expected_source_sha" "$release_intent_id" "$planned_release_ref" "$release_kind" <<'PY'
import json
import sys

print(json.dumps({
    "ref": "main",
    "return_run_details": True,
    "inputs": {
        "candidate_run_id": sys.argv[1],
        "expected_source_sha": sys.argv[2],
        "release_intent_id": sys.argv[3],
        "planned_release_ref": sys.argv[4],
        "release_kind": sys.argv[5],
    },
}, separators=(",", ":")))
PY
)

if [[ $execute -eq 0 ]]; then
  python3 - "$candidate_run_id" "$expected_source_sha" "$payload" <<'PY'
import json
import sys

print(json.dumps({
    "mode": "dry-run",
    "repository": "Helvesec/rmux",
    "candidate_run_id": int(sys.argv[1]),
    "source_git_sha": sys.argv[2],
    "endpoint": "repos/Helvesec/rmux/actions/workflows/release-shadow.yml/dispatches",
    "payload": json.loads(sys.argv[3]),
    "publication_authority": False,
}, indent=2, sort_keys=True))
PY
  exit 0
fi

command -v gh >/dev/null || { echo "gh is required for --execute" >&2; exit 2; }
candidate=$(gh api \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/actions/runs/$candidate_run_id")
python3 - "$candidate_run_id" "$expected_source_sha" "$candidate" <<'PY'
import json
import sys

run = json.loads(sys.argv[3])
expected = {
    "id": int(sys.argv[1]),
    "workflow_id": 277622540,
    "path": ".github/workflows/ci.yml",
    "event": "workflow_dispatch",
    "head_branch": "main",
    "head_sha": sys.argv[2],
    "run_attempt": 1,
    "status": "completed",
    "conclusion": "success",
}
for field, value in expected.items():
    if run.get(field) != value:
        raise SystemExit(f"candidate {field} differs: {run.get(field)!r} != {value!r}")
for field in ("repository", "head_repository"):
    identity = run.get(field) or {}
    if identity.get("id") != 1239918790 or identity.get("full_name") != "Helvesec/rmux":
        raise SystemExit(f"candidate {field} identity differs")
PY

response=$(gh api --method POST \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/actions/workflows/release-shadow.yml/dispatches" \
  --input - <<<"$payload")
shadow_run_id=$(python3 - "$response" <<'PY'
import json
import sys

response = json.loads(sys.argv[1])
run_id = response.get("workflow_run_id")
if type(run_id) is not int or run_id <= 0:
    raise SystemExit("dispatch response has no positive workflow_run_id")
print(run_id)
PY
)

for _ in 1 2 3 4 5 6; do
  if shadow=$(gh api \
      -H 'Accept: application/vnd.github+json' \
      -H 'X-GitHub-Api-Version: 2026-03-10' \
      "repos/$repository/actions/runs/$shadow_run_id" 2>/dev/null); then
    python3 - "$shadow_run_id" "$expected_source_sha" "$shadow" <<'PY'
import json
import sys

run = json.loads(sys.argv[3])
if run.get("id") != int(sys.argv[1]):
    raise SystemExit("shadow run ID differs")
if run.get("head_sha") != sys.argv[2] or run.get("head_branch") != "main":
    raise SystemExit("shadow run source identity differs")
if run.get("workflow_id") != 316223904 or run.get("event") != "workflow_dispatch":
    raise SystemExit("shadow workflow identity differs")
for field in ("repository", "head_repository"):
    if (run.get(field) or {}).get("id") != 1239918790:
        raise SystemExit(f"shadow {field} identity differs")
PY
    printf '{"mode":"dispatched","candidate_run_id":%s,"shadow_run_id":%s}\n' \
      "$candidate_run_id" "$shadow_run_id"
    exit 0
  fi
  sleep 2
done

echo "dispatched shadow run $shadow_run_id was not visible after bounded polling" >&2
exit 1
