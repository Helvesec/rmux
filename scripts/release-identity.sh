#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release-identity.sh <release-tag>

Validate an immutable stable or RC tag against Cargo.toml and print the
separate tag, release, and package identities as KEY=value lines.
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

if [ "$#" -ne 1 ]; then
  usage >&2
  exit 2
fi

case "$1" in
  -h|--help)
    usage
    exit 0
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

release_ref="$1"
case "$release_ref" in
  v*) ;;
  *) die "release tag must start with v: $release_ref" ;;
esac

package_version="$(workspace_version)"
[ -n "$package_version" ] || die "unable to read workspace.package.version"
release_version="${release_ref#v}"
is_prerelease=false

if [ "$release_version" = "$package_version" ]; then
  :
elif [[ "$release_version" == "$package_version-rc."* ]]; then
  rc_number="${release_version#"$package_version-rc."}"
  case "$rc_number" in
    ''|*[!0-9]*|0|0*) die "RC tag must end in -rc.N with N >= 1 and no leading zero: $release_ref" ;;
  esac
  is_prerelease=true
else
  die "release tag $release_ref does not identify Cargo package version $package_version or its -rc.N candidate"
fi

source_git_sha="$(git rev-parse HEAD)"
case "$source_git_sha" in
  *[!0-9a-f]*|'') die "unable to resolve source Git SHA" ;;
  *) ;;
esac
[ "${#source_git_sha}" -eq 40 ] || die "source Git SHA is not full length: $source_git_sha"

printf 'RELEASE_REF=%s\n' "$release_ref"
printf 'RELEASE_VERSION=%s\n' "$release_version"
printf 'PACKAGE_VERSION=%s\n' "$package_version"
printf 'IS_PRERELEASE=%s\n' "$is_prerelease"
printf 'SOURCE_GIT_SHA=%s\n' "$source_git_sha"
