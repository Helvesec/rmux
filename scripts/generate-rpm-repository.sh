#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-rpm-repository.sh --input-dir <dir> --output-dir <dir> [options]

Generate a static RPM/DNF repository for RMUX Fedora packages.

Options:
  --input-dir <dir>              Directory containing rmux-<version>-<release>.<arch>.rpm
  --output-dir <dir>             Repository output directory
  --baseurl <url>                Public repository base URL (default: https://packages.rmux.io/rpm)
  --repo-id <id>                 DNF repo id (default: rmux)
  --repo-name <name>             DNF repo name (default: RMUX)
  --gpg-key-url <url>            Public RPM GPG key URL (default: <baseurl>/RPM-GPG-KEY-rmux)
  --repo-gpg-key-url <url>       Public repodata GPG key URL
  --repo-signing-key <key-id>    GPG key id/fingerprint for repodata/repomd.xml.asc
  --rpm-signing-key <key-id>     RPM signing key name/fingerprint for rpmsign --addsign
  -h, --help                     Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

createrepo_cmd() {
  if command -v createrepo_c >/dev/null 2>&1; then
    printf 'createrepo_c\n'
  elif command -v createrepo >/dev/null 2>&1; then
    printf 'createrepo\n'
  else
    die "missing required command: createrepo_c or createrepo"
  fi
}

input_dir=""
output_dir=""
baseurl="${RMUX_PACKAGES_RPM_BASE_URL:-https://packages.rmux.io/rpm}"
repo_id="${RMUX_RPM_REPO_ID:-rmux}"
repo_name="${RMUX_RPM_REPO_NAME:-RMUX}"
gpg_key_url=""
repo_gpg_key_url=""
repo_signing_key="${RMUX_RPM_REPO_GPG_KEY:-}"
rpm_signing_key="${RMUX_RPM_GPG_KEY:-}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --input-dir)
      [ "$#" -ge 2 ] || die "--input-dir requires a value"
      input_dir="$2"
      shift 2
      ;;
    --output-dir)
      [ "$#" -ge 2 ] || die "--output-dir requires a value"
      output_dir="$2"
      shift 2
      ;;
    --baseurl)
      [ "$#" -ge 2 ] || die "--baseurl requires a value"
      baseurl="$2"
      shift 2
      ;;
    --repo-id)
      [ "$#" -ge 2 ] || die "--repo-id requires a value"
      repo_id="$2"
      shift 2
      ;;
    --repo-name)
      [ "$#" -ge 2 ] || die "--repo-name requires a value"
      repo_name="$2"
      shift 2
      ;;
    --gpg-key-url)
      [ "$#" -ge 2 ] || die "--gpg-key-url requires a value"
      gpg_key_url="$2"
      shift 2
      ;;
    --repo-gpg-key-url)
      [ "$#" -ge 2 ] || die "--repo-gpg-key-url requires a value"
      repo_gpg_key_url="$2"
      shift 2
      ;;
    --repo-signing-key)
      [ "$#" -ge 2 ] || die "--repo-signing-key requires a value"
      repo_signing_key="$2"
      shift 2
      ;;
    --rpm-signing-key)
      [ "$#" -ge 2 ] || die "--rpm-signing-key requires a value"
      rpm_signing_key="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[ -n "$input_dir" ] || die "--input-dir is required"
[ -d "$input_dir" ] || die "input directory not found: $input_dir"
[ -n "$output_dir" ] || die "--output-dir is required"
case "$output_dir" in /|.|..) die "--output-dir is too broad: $output_dir" ;; esac
case "$baseurl" in http://*|https://*) ;; *) die "--baseurl must be an http(s) URL" ;; esac
case "$repo_id" in *[!A-Za-z0-9_.:-]*|""|.*) die "invalid repo id: $repo_id" ;; esac
if [ -z "$gpg_key_url" ]; then
  gpg_key_url="${baseurl%/}/RPM-GPG-KEY-rmux"
fi
case "$gpg_key_url" in http://*|https://*) ;; *) die "--gpg-key-url must be an http(s) URL" ;; esac
if [ -n "$repo_signing_key" ] && [ -z "$repo_gpg_key_url" ]; then
  if [ -n "$rpm_signing_key" ] && [ "$repo_signing_key" = "$rpm_signing_key" ]; then
    repo_gpg_key_url="$gpg_key_url"
  else
    repo_gpg_key_url="${baseurl%/}/RPM-GPG-KEY-rmux-repository"
  fi
fi
if [ -n "$repo_gpg_key_url" ]; then
  case "$repo_gpg_key_url" in http://*|https://*) ;; *) die "--repo-gpg-key-url must be an http(s) URL" ;; esac
fi

repo_tool="$(createrepo_cmd)"
input_dir="$(cd "$input_dir" && pwd)"
output_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
if [ -n "${HOME:-}" ]; then
  home_dir="$(cd "$HOME" && pwd)"
  [ "$output_dir" != "$home_dir" ] || die "--output-dir must not be HOME"
fi
rm -rf "$output_dir"/*

found=0
for rpm in "$input_dir"/rmux-*.rpm; do
  [ -e "$rpm" ] || continue
  cp "$rpm" "$output_dir/"
  found=1
done
[ "$found" -eq 1 ] || die "no rmux-*.rpm files found in $input_dir"

if [ -n "$rpm_signing_key" ]; then
  need rpmsign
  for rpm in "$output_dir"/rmux-*.rpm; do
    rpmsign --define "_gpg_name $rpm_signing_key" --addsign "$rpm"
  done
fi

"$repo_tool" "$output_dir"

rm -f "$output_dir/repodata/repomd.xml.asc"
if [ -n "$repo_signing_key" ]; then
  need gpg
  gpg --batch --yes --local-user "$repo_signing_key" --digest-algo SHA256 \
    --armor --detach-sign --output "$output_dir/repodata/repomd.xml.asc" "$output_dir/repodata/repomd.xml"
fi

gpgcheck=0
repo_gpgcheck=0
if [ -n "$rpm_signing_key" ]; then
  gpgcheck=1
fi

public_key_urls=""
if [ -n "$rpm_signing_key" ]; then
  public_key_urls="$gpg_key_url"
fi
if [ -n "$repo_gpg_key_url" ] && [ "$repo_gpg_key_url" != "$public_key_urls" ]; then
  public_key_urls="${public_key_urls:+$public_key_urls }$repo_gpg_key_url"
fi
[ -n "$public_key_urls" ] || public_key_urls="$gpg_key_url"
if [ -n "$repo_signing_key" ]; then
  repo_gpgcheck=1
fi

cat > "$output_dir/rmux.repo" <<EOF
[$repo_id]
name=$repo_name
baseurl=$baseurl
enabled=1
gpgcheck=$gpgcheck
repo_gpgcheck=$repo_gpgcheck
gpgkey=$public_key_urls
EOF

printf 'repository=%s\n' "$output_dir"
printf 'baseurl=%s\n' "$baseurl"
printf 'rpm_signed=%s\n' "$([ -n "$rpm_signing_key" ] && printf true || printf false)"
printf 'repo_signed=%s\n' "$([ -n "$repo_signing_key" ] && printf true || printf false)"
