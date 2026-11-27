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
    md5|sha256)
      if command -v "${algo}sum" >/dev/null 2>&1; then
        "${algo}sum" < "$file" | awk '{print $1}'
      else
        openssl dgst "-$algo" < "$file" | awk '{print $NF}'
      fi
      ;;
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
release_hash_entry() {
  local release section relative
  release="$1"
  section="$2"
  relative="$3"
  awk -v header="$section:" -v target="$relative" '
    BEGIN { in_section = 0; found = 0; malformed = 0 }
    $0 == header { in_section = 1; next }
    in_section && $0 !~ /^ / { in_section = 0 }
    in_section && NF {
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
release_sha256_entry() {
  release_hash_entry "$1" SHA256 "$2"
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
release_acquire_by_hash() {
  local release
  release="$1"
  awk '
    BEGIN { seen = 0; value = "no" }
    /^[ \t]/ { next }
    {
      colon = index($0, ":")
      if (colon == 0) { next }
      if (tolower(substr($0, 1, colon - 1)) != "acquire-by-hash") { next }
      seen += 1
      value = substr($0, colon + 1)
      gsub(/^[ \t]+|[ \t\r]+$/, "", value)
      value = tolower(value)
    }
    END {
      if (seen > 1) { exit 1 }
      print value
    }
  ' "$release"
}
process_previous_by_hash() {
  local mode release release_resolved advertised architecture name relative entry digest size canonical by_hash destination
  mode="$1"
  release="$previous_repository_dir/dists/$suite/Release"
  [ -f "$release" ] && [ ! -L "$release" ] ||
    die "authenticated previous APT Release is missing or unsafe"
  release_resolved="$(cd "$(dirname "$release")" && pwd -P)/$(basename "$release")"
  case "$release_resolved" in
    "$previous_repository_dir"/*) ;;
    *) die "authenticated previous APT Release escaped its repository" ;;
  esac
  advertised="$(release_acquire_by_hash "$release")" ||
    die "authenticated previous APT Release repeats Acquire-By-Hash"
  case "$advertised" in
    yes)
      by_hash_retention="retained"
      ;;
    no)
      # A publication that never advertised Acquire-By-Hash has no by-hash URL a
      # client can still be resolving, so the first by-hash generation has
      # nothing to retain. Its canonical indexes are still authenticated below.
      by_hash_retention="bootstrap"
      if [ "$mode" = validate ]; then
        printf 'notice: previous APT Release does not advertise Acquire-By-Hash; publishing the first by-hash generation\n' >&2
      fi
      ;;
    *)
      die "authenticated previous APT Release has an unsupported Acquire-By-Hash value: $advertised"
      ;;
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
      verify_index_file \
        "$canonical" "$digest" "$size" "authenticated previous index $relative" \
        "$previous_repository_dir"
      [ "$by_hash_retention" = retained ] || continue

      by_hash="$previous_repository_dir/dists/$suite/$component/binary-$architecture/by-hash/SHA256/$digest"
      verify_index_file \
        "$by_hash" "$digest" "$size" "authenticated previous by-hash index $relative" \
        "$previous_repository_dir"

      [ "$mode" = copy ] || continue
      destination="$output_dir/dists/$suite/$component/binary-$architecture/by-hash/SHA256/$digest"
      if [ -e "$destination" ]; then
        verify_index_file \
          "$destination" "$digest" "$size" "retained by-hash index $relative" \
          "$output_dir"
      else
        cp -- "$by_hash" "$destination"
      fi
    done
  done
}
validate_allowed_entries() {
  local root label entry name allowed matched
  root="$1"
  label="$2"
  shift 2
  [ -d "$root" ] && [ ! -L "$root" ] ||
    die "$label is missing or unsafe"
  collect_find_entries "$label" -P "$root" -mindepth 1 -maxdepth 1
  [ "${#inventory_collected_entries[@]}" -gt 0 ] || return 0
  for entry in "${inventory_collected_entries[@]}"; do
    name="${entry##*/}"
    matched=0
    for allowed in "$@"; do
      if [ "$name" = "$allowed" ]; then
        matched=1
        break
      fi
    done
    [ "$matched" -eq 1 ] ||
      die "$label contains an unexpected entry: $name"
  done
}
render_packages_index() {
  local repository pool architecture deb relative size md5 sha256
  repository="$1"
  pool="$2"
  architecture="$3"
  collect_sorted_find_entries "APT package pool" \
    -P "$pool" -maxdepth 1 -type f -name "rmux_*_${architecture}.deb"
  [ "${#inventory_collected_entries[@]}" -gt 0 ] || return 0
  for deb in "${inventory_collected_entries[@]}"; do
    relative="${deb#"$repository"/}"
    size="$(wc -c < "$deb" | tr -d ' ')" ||
      die "cannot read APT package size: $deb"
    md5="$(hash_file md5 "$deb")" || die "cannot hash APT package: $deb"
    sha256="$(hash_file sha256 "$deb")" || die "cannot hash APT package: $deb"
    dpkg-deb -f "$deb" || die "cannot read APT package metadata: $deb"
    printf 'Filename: %s\n' "$relative"
    printf 'Size: %s\n' "$size"
    printf 'MD5sum: %s\n' "$md5"
    printf 'SHA256: %s\n\n' "$sha256"
  done
}
validate_release_shape() {
  local release architecture paths
  release="$1"
  paths=""
  for architecture in "${architectures[@]}"; do
    paths="${paths:+$paths,}$component/binary-$architecture/Packages"
    paths="$paths,$component/binary-$architecture/Packages.gz"
  done
  awk \
    -v expected_origin="$origin" -v expected_label="$label" \
    -v expected_suite="$suite" -v expected_component="$component" \
    -v expected_architectures="${architectures[*]}" -v expected_paths="$paths" '
    BEGIN {
      key[1] = "Origin"; value[1] = expected_origin
      key[2] = "Label"; value[2] = expected_label
      key[3] = "Suite"; value[3] = expected_suite
      key[4] = "Codename"; value[4] = expected_suite
      key[5] = "Date"; value[5] = ""
      key[6] = "Architectures"; value[6] = expected_architectures
      key[7] = "Components"; value[7] = expected_component
      key[8] = "Description"; value[8] = "RMUX APT repository"
      key[9] = "Acquire-By-Hash"; value[9] = "yes"
      path_count = split(expected_paths, path, ",")
      for (i = 1; i <= path_count; i++) wanted[path[i]] = 1
      header = 0; section = ""; invalid = 0
    }
    section == "" {
      if ($0 == "MD5Sum:") {
        if (header != 9) invalid = 1
        section = "MD5Sum"
        next
      }
      colon = index($0, ":")
      header += 1
      if (colon == 0 || substr($0, colon, 2) != ": " ||
          header > 9 || substr($0, 1, colon - 1) != key[header]) {
        invalid = 1
        next
      }
      observed = substr($0, colon + 2)
      if ((header == 5 && observed == "") ||
          (header != 5 && observed != value[header])) invalid = 1
      next
    }
    section == "MD5Sum" && $0 == "SHA256:" {
      section = "SHA256"
      next
    }
    {
      if ($0 !~ /^ / || NF != 3 || !($3 in wanted) ||
          $2 !~ /^[0-9]+$/ || seen[section, $3]++) {
        invalid = 1
        next
      }
      if ((section == "MD5Sum" &&
           (length($1) != 32 || $1 ~ /[^0-9a-f]/)) ||
          (section == "SHA256" &&
           (length($1) != 64 || $1 ~ /[^0-9a-f]/))) invalid = 1
    }
    END {
      if (header != 9 || section != "SHA256") invalid = 1
      for (i = 1; i <= path_count; i++) {
        if (seen["MD5Sum", path[i]] != 1 ||
            seen["SHA256", path[i]] != 1) invalid = 1
      }
      exit invalid
    }
  ' "$release" || die "output directory is not an RMUX APT repository: invalid Release"
}
verify_managed_index() {
  local file relative release_file entry digest size actual
  file="$1"
  relative="$2"
  release_file="$3"
  entry="$(release_sha256_entry "$release_file" "$relative")" ||
    die "managed APT Release lacks one SHA-256 entry for $relative"
  read -r digest size <<< "$entry"
  verify_index_file "$file" "$digest" "$size" "managed index $relative" "$output_dir"
  entry="$(release_hash_entry "$release_file" MD5Sum "$relative")" ||
    die "managed APT Release lacks one MD5 entry for $relative"
  read -r digest size <<< "$entry"
  actual="$(hash_file md5 "$file")"
  [ "$actual" = "$digest" ] && [ "$(wc -c < "$file" | tr -d ' ')" = "$size" ] ||
    die "managed APT index $relative does not match its MD5 entry"
}
validate_managed_output() {
  local suite_root pool_root release architecture binary name entry digest count matched package package_architecture
  [ -e "$output_dir" ] || return 0
  [ -d "$output_dir" ] && [ ! -L "$output_dir" ] ||
    die "output directory is not one real directory: $output_dir"
  inventory_tree_entries "$output_dir" "APT output"
  [ "${#inventory_collected_entries[@]}" -gt 0 ] || return 0
  validate_allowed_entries "$output_dir" "APT output" dists pool rmux.asc
  suite_root="$output_dir/dists/$suite"
  pool_root="$output_dir/pool/$component/r/rmux"
  validate_allowed_entries "$output_dir/dists" "APT dists root" "$suite"
  validate_allowed_entries "$output_dir/pool" "APT pool root" "$component"
  validate_allowed_entries "$output_dir/pool/$component" "APT component pool" r
  validate_allowed_entries "$output_dir/pool/$component/r" "APT package prefix" rmux
  [ -d "$pool_root" ] || die "APT RMUX package pool is missing"
  validate_allowed_entries "$suite_root" "APT suite" Release InRelease Release.gpg "$component"
  [ -f "$suite_root/Release" ] || die "managed APT Release is missing"
  if [ -e "$suite_root/InRelease" ] || [ -e "$suite_root/Release.gpg" ]; then
    [ -s "$suite_root/InRelease" ] && [ -f "$suite_root/InRelease" ] &&
      [ -s "$suite_root/Release.gpg" ] && [ -f "$suite_root/Release.gpg" ] ||
      die "managed APT signatures are incomplete"
  fi
  if [ -e "$output_dir/rmux.asc" ]; then
    [ -s "$output_dir/rmux.asc" ] && [ -f "$output_dir/rmux.asc" ] ||
      die "managed APT public key is unsafe"
  fi
  release="$suite_root/Release"
  validate_release_shape "$release"
  collect_find_entries "managed APT package pool" \
    -P "$pool_root" -mindepth 1 -maxdepth 1
  if [ "${#inventory_collected_entries[@]}" -gt 0 ]; then
    for package in "${inventory_collected_entries[@]}"; do
      [ -f "$package" ] || die "managed APT pool contains a non-file"
      name="${package##*/}"
      case "$name" in *$'\n'*) die "managed APT package name contains a newline" ;; esac
      matched=0
      package_architecture=""
      for architecture in "${architectures[@]}"; do
        case "$name" in
          rmux_*_"$architecture".deb)
            matched=1
            package_architecture="$architecture"
            ;;
        esac
      done
      [ "$matched" -eq 1 ] || die "managed APT pool contains an unexpected entry: $name"
      validate_apt_package_identity "$package" "$package_architecture"
    done
  fi

  validate_allowed_entries "$suite_root/$component" "APT component indexes" \
    "${architectures[@]/#/binary-}"
  for architecture in "${architectures[@]}"; do
    binary="$suite_root/$component/binary-$architecture"
    validate_allowed_entries "$binary" "APT binary index" Packages Packages.gz by-hash
    validate_allowed_entries "$binary/by-hash" "APT by-hash root" SHA256
    collect_find_entries "managed APT $architecture packages" \
      -P "$pool_root" -maxdepth 1 -type f -name "rmux_*_${architecture}.deb"
    [ "${#inventory_collected_entries[@]}" -gt 0 ] ||
      die "managed APT pool lacks architecture: $architecture"
    cmp -s "$binary/Packages" \
      <(render_packages_index "$output_dir" "$pool_root" "$architecture") ||
      die "managed APT Packages index does not match its package pool"
    cmp -s "$binary/Packages" <(gzip -cd "$binary/Packages.gz") ||
      die "managed APT Packages.gz does not match Packages"
    for name in Packages Packages.gz; do
      verify_managed_index \
        "$binary/$name" "$component/binary-$architecture/$name" "$release"
      digest="$(hash_file sha256 "$binary/$name")"
      cmp -s "$binary/$name" "$binary/by-hash/SHA256/$digest" ||
        die "managed APT by-hash lacks the current $architecture/$name"
    done
    count=0
    collect_find_entries "managed APT $architecture by-hash" \
      -P "$binary/by-hash/SHA256" -mindepth 1 -maxdepth 1
    if [ "${#inventory_collected_entries[@]}" -gt 0 ]; then
      for entry in "${inventory_collected_entries[@]}"; do
        [ -f "$entry" ] || die "managed APT by-hash contains a non-file"
        digest="${entry##*/}"
        [ "${#digest}" -eq 64 ] || die "managed APT by-hash has an invalid name"
        case "$digest" in *[!0-9a-f]*) die "managed APT by-hash has an invalid name" ;; esac
        [ "$(hash_file sha256 "$entry")" = "$digest" ] ||
          die "managed APT by-hash content does not match its name"
        count=$((count + 1))
      done
    fi
    [ "$count" -le 4 ] || die "managed APT by-hash retains too many generations"
  done
}
input_dir=""
output_dir=""
previous_repository_dir=""
by_hash_retention="disabled"
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
[ -n "$output_dir" ] || die "--output-dir is required"
case "$suite" in *[!A-Za-z0-9_.-]*|""|.*) die "invalid suite: $suite" ;; esac
case "$component" in *[!A-Za-z0-9_.-]*|""|.*) die "invalid component: $component" ;; esac
if [ "${#architectures[@]}" -eq 0 ]; then
  architectures=(amd64)
fi
for architecture in "${architectures[@]}"; do
  case "$architecture" in *[!A-Za-z0-9_-]*|"") die "invalid architecture: $architecture" ;; esac
done

for command in awk basename cat cmp cp date dirname dpkg-deb find gzip mkdir readlink rm sort tr wc; do
  need "$command"
done
for algorithm in md5 sha256; do
  command -v "${algorithm}sum" >/dev/null 2>&1 || need openssl
done
if [ -n "$signing_key" ]; then
  need gpg
fi
source "$(dirname -- "${BASH_SOURCE[0]}")/apt-repository-inventory.sh"

input_dir="$(resolve_without_symlinks "$input_dir" "input directory")"
[ -d "$input_dir" ] || die "input directory not found: $input_dir"
output_dir="$(resolve_without_symlinks "$output_dir" "output directory")"
case "$output_dir" in /) die "--output-dir is too broad: $output_dir" ;; esac
paths_overlap "$input_dir" "$output_dir" &&
  die "--input-dir and --output-dir cannot overlap"
if [ -n "$previous_repository_dir" ]; then
  previous_repository_dir="$(
    resolve_without_symlinks "$previous_repository_dir" "previous repository directory"
  )"
  [ -d "$previous_repository_dir" ] ||
    die "previous repository directory not found: $previous_repository_dir"
  inventory_tree_entries "$previous_repository_dir" "previous repository directory"
  paths_overlap "$previous_repository_dir" "$output_dir" &&
    die "--previous-repository-dir and --output-dir cannot overlap"
fi

validate_input_packages
if [ -n "$signing_key" ]; then
  gpg --batch --list-secret-keys "$signing_key" >/dev/null 2>&1 ||
    die "APT signing key is unavailable: $signing_key"
fi
validate_managed_output
if [ -n "$previous_repository_dir" ]; then
  process_previous_by_hash validate
fi

mkdir -p -- "$output_dir"
pool_dir="$output_dir/pool/$component/r/rmux"
rm -rf -- "$output_dir/dists/$suite" "$pool_dir"
mkdir -p -- "$pool_dir"
for deb in "${input_packages[@]}"; do
  cp -- "$deb" "$pool_dir/"
done

release_files=()
for architecture in "${architectures[@]}"; do
  binary_dir="$output_dir/dists/$suite/$component/binary-$architecture"
  mkdir -p -- "$binary_dir"

  packages="$binary_dir/Packages"
  render_packages_index "$output_dir" "$pool_dir" "$architecture" > "$packages"
  gzip -n -c "$packages" > "$packages.gz"
  by_hash_dir="$binary_dir/by-hash/SHA256"
  mkdir -p -- "$by_hash_dir"
  for index in "$packages" "$packages.gz"; do
    index_hash="$(hash_file sha256 "$index")"
    cp -- "$index" "$by_hash_dir/$index_hash"
  done
  release_files+=("$packages" "$packages.gz")
done

if [ -n "$previous_repository_dir" ]; then
  process_previous_by_hash copy
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

rm -f -- "$output_dir/dists/$suite/InRelease" "$output_dir/dists/$suite/Release.gpg"
if [ -n "$signing_key" ]; then
  gpg --batch --yes --local-user "$signing_key" --digest-algo SHA256 \
    --clearsign --output "$output_dir/dists/$suite/InRelease" "$release"
  gpg --batch --yes --local-user "$signing_key" --digest-algo SHA256 \
    --armor --detach-sign --output "$output_dir/dists/$suite/Release.gpg" "$release"
fi

printf 'repository=%s\n' "$output_dir"
printf 'suite=%s\n' "$suite"
printf 'component=%s\n' "$component"
printf 'architectures=%s\n' "${architectures[*]}"
printf 'by_hash_retention=%s\n' "$by_hash_retention"
printf 'signed=%s\n' "$([ -n "$signing_key" ] && printf true || printf false)"
