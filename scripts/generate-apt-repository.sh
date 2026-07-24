#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-apt-repository.sh --input-dir <dir> --output-dir <dir> [options]

Generate a static APT repository for RMUX Debian/Ubuntu packages.

Options:
  --input-dir <dir>          Directory containing rmux_<version>_<arch>.deb
  --output-dir <dir>         Repository output directory
  --previous-repository-dir <dir>
                             Previously authenticated APT repository root
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

release_sha256_entry() {
  local release relative
  release="$1"
  relative="$2"
  awk -v target="$relative" '
    BEGIN { in_sha256 = 0; found = 0; malformed = 0 }
    $0 == "SHA256:" { in_sha256 = 1; next }
    in_sha256 && $0 !~ /^ / { in_sha256 = 0 }
    in_sha256 && NF {
      if (NF != 3) {
        malformed = 1
        next
      }
      if ($3 == target) {
        if (found) {
          malformed = 1
        } else {
          digest = $1
          size = $2
          found = 1
        }
      }
    }
    END {
      if (malformed || found != 1) {
        exit 1
      }
      print digest, size
    }
  ' "$release"
}

verify_index_file() {
  local file expected_hash expected_size label allowed_root resolved actual_size actual_hash
  file="$1"
  expected_hash="$2"
  expected_size="$3"
  label="$4"
  allowed_root="$5"
  [ -f "$file" ] && [ ! -L "$file" ] ||
    die "APT $label is missing or unsafe"
  resolved="$(cd "$(dirname "$file")" && pwd -P)/$(basename "$file")"
  case "$resolved" in
    "$allowed_root"/*) ;;
    *) die "APT $label escaped its repository" ;;
  esac
  actual_size="$(wc -c < "$file" | tr -d ' ')"
  actual_hash="$(hash_file sha256 "$file")"
  [ "$actual_size" = "$expected_size" ] && [ "$actual_hash" = "$expected_hash" ] ||
    die "APT $label does not match its SHA-256 name"
}

retain_previous_by_hash() {
  local release release_resolved architecture name relative entry digest size canonical by_hash destination
  release="$previous_repository_dir/dists/$suite/Release"
  [ -f "$release" ] && [ ! -L "$release" ] ||
    die "authenticated previous APT Release is missing or unsafe"
  release_resolved="$(cd "$(dirname "$release")" && pwd -P)/$(basename "$release")"
  case "$release_resolved" in
    "$previous_repository_dir"/*) ;;
    *) die "authenticated previous APT Release escaped its repository" ;;
  esac
  for architecture in "${architectures[@]}"; do
    for name in Packages Packages.gz; do
      relative="$component/binary-$architecture/$name"
      entry="$(release_sha256_entry "$release" "$relative")" ||
        die "authenticated previous APT Release lacks one SHA-256 entry for $relative"
      read -r digest size <<< "$entry"
      [ "${#digest}" -eq 64 ] || die "invalid previous APT SHA-256 for $relative"
      case "$digest" in *[!0-9a-f]*) die "invalid previous APT SHA-256 for $relative" ;; esac
      case "$size" in *[!0-9]*|"") die "invalid previous APT size for $relative" ;; esac

      canonical="$previous_repository_dir/dists/$suite/$relative"
      by_hash="$previous_repository_dir/dists/$suite/$component/binary-$architecture/by-hash/SHA256/$digest"
      verify_index_file \
        "$canonical" "$digest" "$size" "authenticated previous index $relative" \
        "$previous_repository_dir"
      verify_index_file \
        "$by_hash" "$digest" "$size" "authenticated previous by-hash index $relative" \
        "$previous_repository_dir"

      destination="$output_dir/dists/$suite/$component/binary-$architecture/by-hash/SHA256/$digest"
      if [ -e "$destination" ]; then
        verify_index_file \
          "$destination" "$digest" "$size" "retained by-hash index $relative" \
          "$output_dir"
      else
        cp "$by_hash" "$destination"
      fi
    done
  done
}

input_dir=""
output_dir=""
previous_repository_dir=""
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
    --previous-repository-dir)
      [ "$#" -ge 2 ] || die "--previous-repository-dir requires a value"
      previous_repository_dir="$2"
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
if [ -n "$previous_repository_dir" ]; then
  [ -d "$previous_repository_dir" ] ||
    die "previous repository directory not found: $previous_repository_dir"
  previous_repository_dir="$(cd "$previous_repository_dir" && pwd -P)"
  case "$previous_repository_dir" in
    "$output_dir"|"$output_dir"/*)
      die "--previous-repository-dir and --output-dir cannot overlap"
      ;;
  esac
  case "$output_dir" in
    "$previous_repository_dir"|"$previous_repository_dir"/*)
      die "--previous-repository-dir and --output-dir cannot overlap"
      ;;
  esac
fi
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
  by_hash_dir="$binary_dir/by-hash/SHA256"
  mkdir -p "$by_hash_dir"
  for index in "$packages" "$packages.gz"; do
    index_hash="$(hash_file sha256 "$index")"
    cp "$index" "$by_hash_dir/$index_hash"
  done
  release_files+=("$packages" "$packages.gz")
done

if [ -n "$previous_repository_dir" ]; then
  retain_previous_by_hash
fi

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
Acquire-By-Hash: yes
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
