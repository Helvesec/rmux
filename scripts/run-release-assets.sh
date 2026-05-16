#!/usr/bin/env sh
set -eu

repo="${RMUX_GITHUB_REPO:-Helvesec/rmux}"
tag="${1:-}"

if [ -z "$tag" ]; then
  printf 'usage: %s vX.Y.Z\n' "$0" >&2
  exit 2
fi

case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    printf 'release tag must look like vX.Y.Z, got: %s\n' "$tag" >&2
    exit 2
    ;;
esac

gh workflow run release.yml \
  --repo "$repo" \
  --ref main \
  -f "ref=$tag"

printf 'Triggered release asset build for %s on %s.\n' "$repo" "$tag"
printf 'Watch it with: gh run list --repo %s --workflow Release --limit 1\n' "$repo"
