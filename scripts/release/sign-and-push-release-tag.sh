#!/usr/bin/env bash
set -euo pipefail

repository=''
repository_root=''
release_ref=''
release_intent_id=''
release_kind=''
source_sha=''
candidate_run_id=''
candidate_manifest_artifact_id=''
candidate_manifest_artifact_digest=''
candidate_manifest_sha256=''
release_policy_root_sha256=''
signing_key=''
dry_run=false

usage() {
  cat >&2 <<'EOF'
usage: sign-and-push-release-tag.sh \
  --repository Helvesec/rmux --repository-root PATH \
  --release-ref vX.Y.Z[-rc.N] --release-intent-id ID --release-kind stable|rc \
  --source-sha SHA --candidate-run-id ID --candidate-manifest-artifact-id ID \
  --candidate-manifest-artifact-digest sha256:HEX \
  --candidate-manifest-sha256 HEX --release-policy-root-sha256 HEX \
  --signing-key PATH [--dry-run]

Non-dry execution also requires RMUX_RELEASE_APP_ID=4339867 and an explicit
RMUX_RELEASE_APP_TOKEN installation token. Git receives one exact tag refspec;
this script never updates, force-pushes, or deletes a tag.
EOF
  exit 2
}

while (( $# > 0 )); do
  case "$1" in
    --repository) repository=${2-}; shift 2 ;;
    --repository-root) repository_root=${2-}; shift 2 ;;
    --release-ref) release_ref=${2-}; shift 2 ;;
    --release-intent-id) release_intent_id=${2-}; shift 2 ;;
    --release-kind) release_kind=${2-}; shift 2 ;;
    --source-sha) source_sha=${2-}; shift 2 ;;
    --candidate-run-id) candidate_run_id=${2-}; shift 2 ;;
    --candidate-manifest-artifact-id) candidate_manifest_artifact_id=${2-}; shift 2 ;;
    --candidate-manifest-artifact-digest) candidate_manifest_artifact_digest=${2-}; shift 2 ;;
    --candidate-manifest-sha256) candidate_manifest_sha256=${2-}; shift 2 ;;
    --release-policy-root-sha256) release_policy_root_sha256=${2-}; shift 2 ;;
    --signing-key) signing_key=${2-}; shift 2 ;;
    --dry-run) dry_run=true; shift ;;
    *) usage ;;
  esac
done

[[ $repository == Helvesec/rmux ]] || { echo 'repository must be Helvesec/rmux' >&2; exit 2; }
[[ -n $repository_root && -e $repository_root/.git ]] || { echo 'repository root is invalid' >&2; exit 2; }
[[ -n $signing_key && -f $signing_key && ! -L $signing_key ]] || {
  echo 'a regular, non-symlink dedicated signing key is required' >&2
  exit 2
}
for command in git python3 realpath ssh-keygen; do
  command -v "$command" >/dev/null || { echo "$command is required" >&2; exit 2; }
done

key_mode=$(python3 - "$signing_key" <<'PY'
import os
import stat
import sys

try:
    metadata = os.stat(sys.argv[1], follow_symlinks=False)
except OSError as error:
    raise SystemExit(f"cannot inspect dedicated signing key: {error}") from error
if not stat.S_ISREG(metadata.st_mode):
    raise SystemExit("dedicated signing key is no longer a regular file")
print(f"{stat.S_IMODE(metadata.st_mode):o}")
PY
)
if (( (8#$key_mode & 8#077) != 0 )); then
  echo 'dedicated signing key must not be accessible by group or others' >&2
  exit 2
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
"$script_dir/verify-release-tag.py" policy >/dev/null

identity_args=(
  --release-ref "$release_ref"
  --release-intent-id "$release_intent_id"
  --release-kind "$release_kind"
  --source-sha "$source_sha"
  --candidate-run-id "$candidate_run_id"
  --candidate-manifest-artifact-id "$candidate_manifest_artifact_id"
  --candidate-manifest-artifact-digest "$candidate_manifest_artifact_digest"
  --candidate-manifest-sha256 "$candidate_manifest_sha256"
  --release-policy-root-sha256 "$release_policy_root_sha256"
)

temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
message_file=$temporary/tag-message
"$script_dir/release-tag-message.py" render "${identity_args[@]}" --output "$message_file"

git -C "$repository_root" cat-file -e "$source_sha^{commit}" 2>/dev/null || {
  echo "source SHA is not an available commit: $source_sha" >&2
  exit 2
}

extract_http_response() {
  local raw=$1
  local destination=$2
  python3 - "$raw" "$destination" <<'PY'
from pathlib import Path
import re
import sys

raw = Path(sys.argv[1]).read_bytes()
first = raw.splitlines()[0].decode("ascii", "replace") if raw else ""
match = re.fullmatch(r"HTTP/[^ ]+ ([0-9]{3})(?: .*)?", first)
if match is None:
    raise SystemExit("GitHub API response has no HTTP status line")
separator = b"\r\n\r\n" if b"\r\n\r\n" in raw else b"\n\n"
parts = raw.split(separator, 1)
if len(parts) != 2:
    raise SystemExit("GitHub API response has no header/body separator")
Path(sys.argv[2]).write_bytes(parts[1])
print(match.group(1))
PY
}

api_get() {
  local endpoint=$1
  local destination=$2
  local raw=$temporary/api-response
  local errors=$temporary/api-errors
  local exit_code
  set +e
  GH_TOKEN=$RMUX_RELEASE_APP_TOKEN gh api --include \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    "$endpoint" >"$raw" 2>"$errors"
  exit_code=$?
  set -e
  local status
  status=$(extract_http_response "$raw" "$destination") || {
    cat "$errors" >&2
    return 1
  }
  case "$status" in
    200) [[ $exit_code -eq 0 ]] || { cat "$errors" >&2; return 1; }; return 0 ;;
    404) return 4 ;;
    *) cat "$errors" >&2; echo "unexpected GitHub API status $status" >&2; return 1 ;;
  esac
}

verify_existing_ref() {
  local ref_json=$1
  local tag_sha
  tag_sha=$(python3 - "$ref_json" <<'PY'
import json
import sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
obj = value.get("object")
if not isinstance(obj, dict) or obj.get("type") != "tag":
    raise SystemExit("existing release ref is not an annotated tag")
sha = obj.get("sha")
if not isinstance(sha, str) or len(sha) != 40:
    raise SystemExit("existing release ref has no full tag object SHA")
print(sha.lower())
PY
  )
  local tag_json=$temporary/existing-tag.json
  api_get "repos/$repository/git/tags/$tag_sha" "$tag_json" || {
    echo 'cannot read the existing tag object' >&2
    return 1
  }
  "$script_dir/verify-release-tag.py" github-json \
    --ref-json "$ref_json" --tag-json "$tag_json" "${identity_args[@]}"
}

if [[ $dry_run == false ]]; then
  command -v gh >/dev/null || { echo 'gh is required for tag creation' >&2; exit 2; }
  [[ ${RMUX_RELEASE_APP_ID:-} == 4339867 ]] || {
    echo 'RMUX_RELEASE_APP_ID must identify Helvesec RMUX Release App 4339867' >&2
    exit 2
  }
  [[ -n ${RMUX_RELEASE_APP_TOKEN:-} ]] || {
    echo 'RMUX_RELEASE_APP_TOKEN is required; no GitHub token fallback is allowed' >&2
    exit 2
  }
  ref_json=$temporary/existing-ref.json
  if api_get "repos/$repository/git/ref/tags/$release_ref" "$ref_json"; then
    verification=$(verify_existing_ref "$ref_json")
    printf '{"mode":"idempotent-existing","verification":%s}\n' "$verification"
    exit 0
  else
    status=$?
    [[ $status -eq 4 ]] || exit "$status"
  fi
fi

clone=$temporary/repository
git clone --quiet --no-checkout --no-hardlinks --local "$repository_root" "$clone"
git -C "$clone" config user.name 'RMUX Release'
git -C "$clone" config user.email 'release@rmux.io'
git -C "$clone" config gpg.format ssh
git -C "$clone" config user.signingkey "$(realpath "$signing_key")"
git -C "$clone" tag --annotate --sign --file "$message_file" "$release_ref" "$source_sha"

verification=$(
  "$script_dir/verify-release-tag.py" local \
    --repository "$clone" "${identity_args[@]}"
)
tag_object_sha=$(git -C "$clone" rev-parse --verify "refs/tags/$release_ref^{tag}")

if [[ $dry_run == true ]]; then
  printf '{"mode":"dry-run","tag_object_sha":"%s","verification":%s}\n' \
    "$tag_object_sha" "$verification"
  exit 0
fi

askpass=$temporary/git-askpass.sh
printf '%s\n' \
  '#!/usr/bin/env sh' \
  'case "$1" in' \
  '  *Username*) printf "%s\\n" x-access-token ;;' \
  '  *Password*) printf "%s\\n" "$RMUX_RELEASE_APP_TOKEN" ;;' \
  '  *) exit 2 ;;' \
  'esac' >"$askpass"
chmod 700 "$askpass"
git -C "$clone" remote add release-origin 'https://github.com/Helvesec/rmux.git'
set +e
GIT_ASKPASS=$askpass \
  GIT_TERMINAL_PROMPT=0 \
  RMUX_RELEASE_APP_TOKEN=$RMUX_RELEASE_APP_TOKEN \
  git -C "$clone" push --porcelain release-origin \
    "refs/tags/$release_ref:refs/tags/$release_ref"
push_status=$?
set -e

created_ref=$temporary/created-ref.json
if ! api_get "repos/$repository/git/ref/tags/$release_ref" "$created_ref"; then
  echo 'release tag creation was not observable after the create-only request' >&2
  exit 1
fi
created_sha=$(python3 - "$created_ref" "$release_ref" <<'PY'
import json
import sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
obj = value.get("object")
if value.get("ref") != f"refs/tags/{sys.argv[2]}" or not isinstance(obj, dict) or obj.get("type") != "tag":
    raise SystemExit("created ref identity or object type differs")
print(obj.get("sha", "").lower())
PY
)
if [[ $created_sha != "$tag_object_sha" ]]; then
  echo "release ref points to $created_sha, expected exact tag object $tag_object_sha" >&2
  exit 1
fi
github_verification=$(verify_existing_ref "$created_ref")
if [[ $push_status -ne 0 ]]; then
  echo 'tag push raced, but the exact signed tag object now exists' >&2
fi
printf '{"mode":"created","tag_object_sha":"%s","local_verification":%s,"github_verification":%s}\n' \
  "$tag_object_sha" "$verification" "$github_verification"
