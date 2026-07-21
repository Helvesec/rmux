#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: dispatch-release-promotion.sh [--plan] [--snap-candidate] \
  CANDIDATE_RUN_ID MANIFEST_RUN_ID SOURCE_SHA RELEASE_INTENT_ID RELEASE_REF RELEASE_KIND
EOF
  exit 2
}

mode=dispatch
snap_candidate=false
while [[ ${1:-} == --* ]]; do
  case "$1" in
    --plan) mode=plan ;;
    --snap-candidate) snap_candidate=true ;;
    *) usage ;;
  esac
  shift
done
[[ $# -eq 6 ]] || usage

candidate_run_id=$1
manifest_run_id=$2
source_sha=$3
release_intent_id=$4
release_ref=$5
release_kind=$6
repository=Helvesec/rmux
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

[[ $candidate_run_id =~ ^[1-9][0-9]*$ ]]
[[ $manifest_run_id =~ ^[1-9][0-9]*$ ]]
[[ $source_sha =~ ^[0-9a-f]{40}$ ]]
[[ $release_intent_id =~ ^[A-Za-z0-9._:-]{8,128}$ ]]
[[ $release_ref =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]]
[[ $release_kind =~ ^(rc|stable)$ ]]
if [[ $release_kind == rc ]]; then
  [[ $release_ref =~ -rc\.[0-9]+$ ]]
else
  [[ $release_ref =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
fi
[[ $(git branch --show-current) == main ]]
[[ $(git rev-parse HEAD) == "$source_sha" ]]
[[ $(git status --porcelain) == '' ]]

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
artifact_name="rmux-candidate-manifest-$candidate_run_id"
GH_TOKEN=${GH_TOKEN:-$(gh auth token)}
export GH_TOKEN
python3 scripts/release/actions-artifact.py resolve \
  --repository "$repository" --run-id "$manifest_run_id" \
  --name "$artifact_name" --expected-source-sha "$source_sha" \
  --expected-workflow-id 316223904 \
  --expected-workflow-path .github/workflows/release-shadow.yml \
  --expected-event workflow_dispatch --expected-head-branch main \
  --include-retention > "$tmp/artifact.json"

gh run download "$manifest_run_id" --repo "$repository" \
  --name "$artifact_name" --dir "$tmp/manifest"
python3 - "$tmp/manifest" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
expected = {
    "candidate-manifest.json",
    "candidate-run-proof.json",
    "release-policy-root.json",
    "verified-candidate-artifacts.json",
}
paths = list(root.rglob("*"))
if any(path.is_symlink() for path in paths):
    raise SystemExit("candidate manifest artifact contains a symlink")
files = {path.relative_to(root).as_posix() for path in paths if path.is_file()}
if files != expected:
    raise SystemExit(f"candidate manifest artifact file set differs: {sorted(files)}")
PY

python3 - \
  "$tmp/artifact.json" "$tmp/manifest/candidate-manifest.json" \
  "$tmp/manifest/release-policy-root.json" "$tmp/dispatch.json" \
  "$candidate_run_id" "$manifest_run_id" "$source_sha" \
  "$release_intent_id" "$release_ref" "$release_kind" "$snap_candidate" <<'PY'
import hashlib
import json
import pathlib
import sys

(
    artifact_path,
    manifest_path,
    policy_path,
    output_path,
    candidate_run_id,
    manifest_run_id,
    source_sha,
    intent_id,
    release_ref,
    release_kind,
    snap_candidate,
) = sys.argv[1:]
artifact = json.loads(pathlib.Path(artifact_path).read_text(encoding="utf-8"))
manifest_file = pathlib.Path(manifest_path)
manifest = json.loads(manifest_file.read_text(encoding="utf-8"))
policy = json.loads(pathlib.Path(policy_path).read_text(encoding="utf-8"))
expected = {
    "candidate_run_id": int(candidate_run_id),
    "source_git_sha": source_sha,
    "release_intent_id": intent_id,
    "planned_release_ref": release_ref,
    "release_kind": release_kind,
    "candidate_run_attempt": 1,
}
for field, value in expected.items():
    if manifest.get(field) != value:
        raise SystemExit(f"candidate manifest {field} differs")
release_policy = manifest.get("release_policy", {})
if (
    policy.get("source_git_sha") != source_sha
    or policy.get("release_policy_sha256") != release_policy.get("sha256")
    or policy.get("contract_blob_oid") != release_policy.get("contract_blob_oid")
):
    raise SystemExit("candidate release policy evidence differs")
inputs = {
    "candidate_run_id": candidate_run_id,
    "candidate_manifest_run_id": manifest_run_id,
    "candidate_manifest_artifact_id": str(artifact["artifact_id"]),
    "candidate_manifest_artifact_digest": artifact["digest"],
    "candidate_manifest_sha256": hashlib.sha256(manifest_file.read_bytes()).hexdigest(),
    "candidate_manifest_created_at": manifest["created_at"],
    "candidate_manifest_expires_at": manifest["expires_at"],
    "expected_source_sha": source_sha,
    "release_intent_id": intent_id,
    "release_ref": release_ref,
    "release_kind": release_kind,
    "release_policy_sha256": release_policy["sha256"],
    "release_policy_contract_blob_oid": release_policy["contract_blob_oid"],
    "snap_candidate_opt_in": snap_candidate == "true",
}
payload = {"ref": "main", "return_run_details": True, "inputs": inputs}
pathlib.Path(output_path).write_text(
    json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
)
PY

if [[ $mode == plan ]]; then
  cat "$tmp/dispatch.json"
  exit 0
fi

for capability in \
  signed_tag_creation policy_audit promotion_authorization \
  github_release_publication publication_receipt downstream_channels; do
  scripts/release/assert-release-capability.py "$capability" >/dev/null
done

response=$(gh api --method POST \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/actions/workflows/release-tag-authoring.yml/dispatches" \
  --input "$tmp/dispatch.json")
run_id=$(python3 - "$response" <<'PY'
import json
import sys

value = json.loads(sys.argv[1])
run_id = value.get("workflow_run_id")
if type(run_id) is not int or run_id <= 0:
    raise SystemExit("tag-authoring dispatch did not return one workflow run ID")
print(run_id)
PY
)
run=$(gh api -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2026-03-10' \
  "repos/$repository/actions/runs/$run_id")
python3 - "$run" "$run_id" "$source_sha" <<'PY'
import json
import sys

value = json.loads(sys.argv[1])
expected = {
    "id": int(sys.argv[2]),
    "workflow_id": 316435348,
    "path": ".github/workflows/release-tag-authoring.yml",
    "event": "workflow_dispatch",
    "head_branch": "main",
    "head_sha": sys.argv[3],
    "run_attempt": 1,
}
if any(value.get(field) != wanted for field, wanted in expected.items()):
    raise SystemExit("dispatched tag-authoring run identity differs")
if value.get("repository", {}).get("id") != 1239918790:
    raise SystemExit("dispatched promotion repository identity differs")
print(json.dumps({"tag_authoring_run_id": value["id"], "url": value["html_url"]}))
PY
