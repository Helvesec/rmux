#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <download-root> <source-sha> <gh-binary>" >&2
  exit 2
fi

download_root=$1
source_sha=$2
gh_bin=$3
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo "invalid source SHA" >&2; exit 2; }
[[ -d $download_root && ! -L $download_root ]] || {
  echo "download root must be a directory" >&2
  exit 2
}
[[ -x $gh_bin && ! -L $gh_bin ]] || { echo "gh verifier must be executable" >&2; exit 2; }

platforms=(
  linux-aarch64
  linux-x86_64
  macos-aarch64
  macos-x86_64
  windows-x86_64
)

for platform in "${platforms[@]}"; do
  assets_name="rmux-canonical-$platform-$source_sha"
  provenance_name="rmux-canonical-provenance-$platform-$source_sha"
  assets="$download_root/$assets_name"
  provenance="$download_root/$provenance_name"
  record="$assets/canonical-build-record.json"
  bundle="$provenance/build-provenance.sigstore.json"
  [[ -f $record && ! -L $record && -f $bundle && ! -L $bundle ]] || {
    echo "missing regular record or attestation bundle for $platform" >&2
    exit 1
  }
  mapfile -t subjects < <(python3 - "$record" "$assets" <<'PY'
import json
import pathlib
import sys

record = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
root = pathlib.Path(sys.argv[2]).resolve(strict=True)
files = record.get("files")
if not isinstance(files, list) or not files:
    raise SystemExit("canonical record has no subjects")
for item in files:
    relative = item.get("path") if isinstance(item, dict) else None
    if not isinstance(relative, str):
        raise SystemExit("canonical subject path is invalid")
    candidate = root / "assets" / relative
    if candidate.is_symlink():
        raise SystemExit("canonical subject escaped its artifact root")
    path = candidate.resolve(strict=True)
    if root not in path.parents or not path.is_file():
        raise SystemExit("canonical subject escaped its artifact root")
    print(path)
print(pathlib.Path(sys.argv[1]).resolve(strict=True))
PY
  )
  [[ ${#subjects[@]} -ge 2 ]] || { echo "no subjects for $platform" >&2; exit 1; }
  for subject in "${subjects[@]}"; do
    "$gh_bin" attestation verify "$subject" \
      --bundle "$bundle" \
      --repo Helvesec/rmux \
      --signer-workflow Helvesec/rmux/.github/workflows/canonical-native-build.yml \
      --source-digest "$source_sha" \
      --source-ref refs/heads/main \
      --deny-self-hosted-runners \
      --format json >/dev/null
  done
done

echo candidate-attestations-ok
