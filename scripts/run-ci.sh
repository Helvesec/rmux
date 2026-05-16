#!/usr/bin/env sh
set -eu

repo="${RMUX_GITHUB_REPO:-Helvesec/rmux}"
ref="${1:-main}"
windows_runtime_smoke="${RMUX_WINDOWS_RUNTIME_SMOKE:-false}"

if [ "${2:-}" = "--windows-runtime-smoke" ]; then
  windows_runtime_smoke="true"
fi

gh workflow run ci.yml \
  --repo "$repo" \
  --ref "$ref" \
  -f "windows_runtime_smoke=$windows_runtime_smoke"

printf 'Triggered CI for %s on %s.\n' "$repo" "$ref"
printf 'Watch it with: gh run list --repo %s --workflow CI --limit 1\n' "$repo"
