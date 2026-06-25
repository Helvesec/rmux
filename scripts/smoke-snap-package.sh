#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-snap-package.sh <rmux.snap>

Install an RMUX snap locally with classic confinement and run the release smoke.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

workspace_version() {
  awk '
    /^\[workspace\.package\]$/ { in_workspace = 1; next }
    /^\[/ { in_workspace = 0 }
    in_workspace && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' Cargo.toml
}

snap_path="${1:-}"
if [ "$snap_path" = "-h" ] || [ "$snap_path" = "--help" ]; then
  usage
  exit 0
fi

[ -n "$snap_path" ] || die "snap path is required"
[ -f "$snap_path" ] || die "snap package not found: $snap_path"
case "$snap_path" in *.snap) ;; *) die "expected a .snap package: $snap_path" ;; esac
command -v snap >/dev/null 2>&1 || die "snap command is required"
command -v sudo >/dev/null 2>&1 || die "sudo is required"

version="$(workspace_version)"
[ -n "$version" ] || die "unable to read workspace package version"

sudo snap remove rmux >/dev/null 2>&1 || true
cleanup() {
  /snap/bin/rmux kill-server >/dev/null 2>&1 || true
  sudo snap remove rmux >/dev/null 2>&1 || true
}
trap cleanup EXIT

sudo snap install --dangerous --classic "$snap_path"

test -f /snap/rmux/current/share/man/man1/rmux.1 ||
  die "snap package did not install rmux.1 manpage"
test -f /snap/rmux/current/share/bash-completion/completions/rmux ||
  die "snap package did not install bash completion"

version_output="$(/snap/bin/rmux -V)"
[ "$version_output" = "rmux $version" ] || die "unexpected rmux version: $version_output"

/snap/bin/rmux list-commands >/dev/null

label="snap-smoke-$$"
/snap/bin/rmux -L "$label" kill-server >/dev/null 2>&1 || true
/snap/bin/rmux -L "$label" new-session -d -s snap_smoke >/dev/null
sessions="$(/snap/bin/rmux -L "$label" list-sessions -F '#{session_name}')"
/snap/bin/rmux -L "$label" kill-server >/dev/null 2>&1 || true
printf '%s\n' "$sessions" | grep -qx 'snap_smoke' ||
  die "snap daemon smoke did not list snap_smoke session"

printf 'snap=%s\n' "$snap_path"
printf 'version=%s\n' "$version"
printf 'smoke=ok\n'
