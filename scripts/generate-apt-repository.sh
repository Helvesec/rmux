#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-apt-repository.sh --input-dir <dir> --output-dir <dir> [options]

Generate a static APT repository for RMUX Debian/Ubuntu packages.

Options:
  --input-dir <dir>          Directory containing rmux_<version>_<arch>.deb
  --output-dir <dir>         Repository output directory
  --suite <name>             APT suite/codename (default: stable)
  --component <name>         APT component (default: main)
  --architecture <arch>      Debian architecture; repeat for multiple arches (default: amd64)
  --origin <name>            Release Origin field (default: RMUX)
  --label <name>             Release Label field (default: RMUX)
  --signing-key <key-id>     GPG key id/fingerprint for InRelease and Release.gpg
  -h, --help                 Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

hash_file() {
  local algo file
  algo="$1"
  file="$2"
  case "$algo" in
    md5) md5sum "$file" | awk '{print $1}' ;;
    sha256) sha256sum "$file" | awk '{print $1}' ;;
    *) die "unsupported hash: $algo" ;;
  esac
}

release_hash_block() {
  local algo root file hash size relative
  algo="$1"
  root="$2"
  shift 2
  for file in "$@"; do
    relative="${file#"$root"/}"
    hash="$(hash_file "$algo" "$file")"
    size="$(wc -c < "$file" | tr -d ' ')"
    printf ' %s %16s %s\n' "$hash" "$size" "$relative"
  done
}

input_dir=""
output_dir=""
suite="stable"
component="main"
architectures=()
origin="RMUX"
label="RMUX"
signing_key="${RMUX_APT_GPG_KEY:-}"

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
    --suite)
      [ "$#" -ge 2 ] || die "--suite requires a value"
      suite="$2"
      shift 2
      ;;
    --component)
      [ "$#" -ge 2 ] || die "--component requires a value"
      component="$2"
      shift 2
      ;;
    --architecture)
      [ "$#" -ge 2 ] || die "--architecture requires a value"
      architectures+=("$2")
      shift 2
      ;;
    --origin)
      [ "$#" -ge 2 ] || die "--origin requires a value"
      origin="$2"
      shift 2
      ;;
    --label)
      [ "$#" -ge 2 ] || die "--label requires a value"
      label="$2"
      shift 2
      ;;
    --signing-key)
      [ "$#" -ge 2 ] || die "--signing-key requires a value"
      signing_key="$2"
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
case "$suite" in *[!A-Za-z0-9_.-]*|""|.*) die "invalid suite: $suite" ;; esac
case "$component" in *[!A-Za-z0-9_.-]*|""|.*) die "invalid component: $component" ;; esac
if [ "${#architectures[@]}" -eq 0 ]; then
  architectures=(amd64)
fi
for architecture in "${architectures[@]}"; do
  case "$architecture" in *[!A-Za-z0-9_-]*|"") die "invalid architecture: $architecture" ;; esac
done

need dpkg-deb
need gzip
need md5sum
need sha256sum

input_dir="$(cd "$input_dir" && pwd)"
output_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
pool_dir="$output_dir/pool/$component/r/rmux"
rm -rf "$output_dir/dists/$suite" "$pool_dir"
mkdir -p "$pool_dir"

release_files=()
for architecture in "${architectures[@]}"; do
  binary_dir="$output_dir/dists/$suite/$component/binary-$architecture"
  mkdir -p "$binary_dir"

  found=0
  for deb in "$input_dir"/rmux_*_"$architecture".deb; do
    [ -e "$deb" ] || continue
    cp "$deb" "$pool_dir/"
    found=1
  done
  [ "$found" -eq 1 ] || die "no rmux_*_${architecture}.deb files found in $input_dir"

  packages="$binary_dir/Packages"
  : > "$packages"
  find "$pool_dir" -type f -name "rmux_*_${architecture}.deb" | LC_ALL=C sort |
    while IFS= read -r deb; do
      relative="${deb#"$output_dir"/}"
      size="$(wc -c < "$deb" | tr -d ' ')"
      md5="$(hash_file md5 "$deb")"
      sha256="$(hash_file sha256 "$deb")"
      dpkg-deb -f "$deb" >> "$packages"
      {
        printf 'Filename: %s\n' "$relative"
        printf 'Size: %s\n' "$size"
        printf 'MD5sum: %s\n' "$md5"
        printf 'SHA256: %s\n\n' "$sha256"
      } >> "$packages"
    done
  gzip -n -c "$packages" > "$packages.gz"
  release_files+=("$packages" "$packages.gz")
done

release="$output_dir/dists/$suite/Release"
date_utc="$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')"
cat > "$release" <<EOF
Origin: $origin
Label: $label
Suite: $suite
Codename: $suite
Date: $date_utc
Architectures: ${architectures[*]}
Components: $component
Description: RMUX APT repository
MD5Sum:
$(release_hash_block md5 "$output_dir/dists/$suite" "${release_files[@]}")
SHA256:
$(release_hash_block sha256 "$output_dir/dists/$suite" "${release_files[@]}")
EOF

rm -f "$output_dir/dists/$suite/InRelease" "$output_dir/dists/$suite/Release.gpg"
if [ -n "$signing_key" ]; then
  need gpg
  gpg --batch --yes --local-user "$signing_key" --digest-algo SHA256 \
    --clearsign --output "$output_dir/dists/$suite/InRelease" "$release"
  gpg --batch --yes --local-user "$signing_key" --digest-algo SHA256 \
    --armor --detach-sign --output "$output_dir/dists/$suite/Release.gpg" "$release"
fi

printf 'repository=%s\n' "$output_dir"
printf 'suite=%s\n' "$suite"
printf 'component=%s\n' "$component"
printf 'architectures=%s\n' "${architectures[*]}"
printf 'signed=%s\n' "$([ -n "$signing_key" ] && printf true || printf false)"
