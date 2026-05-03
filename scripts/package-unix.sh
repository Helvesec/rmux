#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-unix.sh [options]

Build a local-first RMUX Unix package for Linux or macOS.

Options:
  --configuration debug|release   Cargo profile to package (default: release)
  --target <triple>               Cargo target triple (default: host target)
  --output-dir <path>             Output directory (default: target/dist)
  --platform-label <label>        Artifact label override (default: inferred)
  --skip-build                    Repackage an existing binary
  --allow-stale-binary            Allow --skip-build for local-only reference
  -h, --help                      Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    die "no SHA256 tool found"
  fi
}

json_escape() {
  sed 's/\\/\\\\/g; s/"/\\"/g'
}

commit_time_iso() {
  git show -s --format=%cI HEAD
}

commit_touch_timestamp() {
  local epoch
  epoch="$(git show -s --format=%ct HEAD)"
  if date -u -r "$epoch" +%Y%m%d%H%M.%S >/dev/null 2>&1; then
    date -u -r "$epoch" +%Y%m%d%H%M.%S
  else
    date -u -d "@$epoch" +%Y%m%d%H%M.%S
  fi
}

write_package_checksums() {
  local root output file hash relative
  root="$1"
  output="$2"

  (
    cd "$root"
    find . -type f ! -path './SHA256SUMS.txt' | LC_ALL=C sort |
      while IFS= read -r file; do
        relative="${file#./}"
        case "$relative" in
          /*|../*|*/../*|*\\*) die "non-portable package checksum path: $relative" ;;
        esac
        hash="$(sha256_file "$file")"
        printf '%s  %s\n' "$hash" "$relative"
      done
  ) > "$output"
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

host_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os:$arch" in
    Linux:x86_64) printf 'x86_64-unknown-linux-gnu' ;;
    Linux:aarch64|Linux:arm64) printf 'aarch64-unknown-linux-gnu' ;;
    Darwin:x86_64) printf 'x86_64-apple-darwin' ;;
    Darwin:arm64|Darwin:aarch64) printf 'aarch64-apple-darwin' ;;
    *) die "unsupported host for default target: $os $arch; pass --target and --platform-label" ;;
  esac
}

target_label() {
  case "$1" in
    x86_64-unknown-linux-gnu) printf 'linux-x86_64' ;;
    aarch64-unknown-linux-gnu) printf 'linux-aarch64' ;;
    x86_64-apple-darwin) printf 'macos-x86_64' ;;
    aarch64-apple-darwin) printf 'macos-aarch64' ;;
    *) printf '%s' "$1" | tr -c 'A-Za-z0-9_.-' '-' ;;
  esac
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
configuration="release"
target=""
output_dir="target/dist"
platform_label=""
skip_build=0
allow_stale_binary=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --configuration)
      [ "$#" -ge 2 ] || die "--configuration requires a value"
      configuration="$2"
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || die "--target requires a value"
      target="$2"
      shift 2
      ;;
    --output-dir)
      [ "$#" -ge 2 ] || die "--output-dir requires a value"
      output_dir="$2"
      shift 2
      ;;
    --platform-label)
      [ "$#" -ge 2 ] || die "--platform-label requires a value"
      platform_label="$2"
      shift 2
      ;;
    --skip-build)
      skip_build=1
      shift
      ;;
    --allow-stale-binary)
      allow_stale_binary=1
      shift
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

[ "$configuration" = "debug" ] || [ "$configuration" = "release" ] || die "unsupported configuration: $configuration"

cd "$repo_root"
version="$(workspace_version)"
[ -n "$version" ] || die "unable to read workspace package version"

if [ -z "$target" ]; then
  target="$(host_target)"
fi
if [ -z "$platform_label" ]; then
  platform_label="$(target_label "$target")"
fi

profile_dir="debug"
cargo_args=(build --locked --target "$target")
if [ "$configuration" = "release" ]; then
  profile_dir="release"
  cargo_args+=(--release)
fi

if [ "$skip_build" -eq 0 ]; then
  cargo "${cargo_args[@]}"
elif [ "$allow_stale_binary" -eq 0 ]; then
  die "--skip-build is local-only reference; pass --allow-stale-binary to acknowledge that"
fi

target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/$target/$profile_dir/rmux"
[ -f "$binary" ] || die "expected binary was not found: $binary"
[ -x "$binary" ] || die "expected binary is not executable: $binary"

dist_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
package_name="rmux-$version-$platform_label"
stage_dir="$dist_dir/$package_name"
archive_path="$dist_dir/$package_name.tar.gz"
checksums_path="$dist_dir/SHA256SUMS.txt"

case "$stage_dir" in "$dist_dir"/*) ;; *) die "stage path escapes output dir" ;; esac
rm -rf "$stage_dir"
mkdir -p "$stage_dir/bin" "$stage_dir/docs" "$stage_dir/share/man/man1"

cp "$binary" "$stage_dir/bin/rmux"
cp public overview release notes "$stage_dir/"
cp rmux.1 "$stage_dir/share/man/man1/rmux.1"
for doc in diagnose.txt macos-validation.txt platform-support.txt release-packaging.txt; do
  cp "docs/$doc" "$stage_dir/docs/$doc"
done
if [ -f LICENSE ]; then
  cp LICENSE "$stage_dir/LICENSE"
elif ls LICENSE.* >/dev/null 2>&1; then
  cp LICENSE.* "$stage_dir/"
fi

binary_abs="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"
binary_sha256="$(sha256_file "$binary")"
binary_bytes="$(wc -c < "$binary" | tr -d ' ')"
git_commit="$(git rev-parse HEAD)"
git_dirty=false
if [ -n "$(git status --porcelain)" ]; then
  git_dirty=true
fi
release_reference=true
if [ "$skip_build" -eq 1 ] || [ "$git_dirty" = true ]; then
  release_reference=false
fi
generated_at_utc="$(commit_time_iso)"

cat > "$stage_dir/docs/artifact-metadata.json" <<EOF
{
  "schema": 1,
  "artifact_kind": "unix-package-binary",
  "binary_path": "$(printf '%s' "$binary_abs" | json_escape)",
  "binary_sha256": "$binary_sha256",
  "binary_bytes": $binary_bytes,
  "rmux_version": "$version",
  "git_commit": "$git_commit",
  "git_dirty": $git_dirty,
  "target": "$target",
  "platform_label": "$platform_label",
  "configuration": "$configuration",
  "package_schema": 1,
  "package_name": "$package_name",
  "package_target": "$target",
  "package_target_label": "$platform_label",
  "package_layout": "rmux-package-v1",
  "archive_format": "tar.gz",
  "archive_reproducibility": "normalized-mtime-gzip-no-name",
  "skip_build": $([ "$skip_build" -eq 1 ] && printf true || printf false),
  "release_reference": $release_reference,
  "generated_at_utc": "$generated_at_utc"
}
EOF

write_package_checksums "$stage_dir" "$stage_dir/SHA256SUMS.txt"
touch_stamp="$(commit_touch_timestamp)"
find "$stage_dir" -exec touch -t "$touch_stamp" {} +

rm -f "$archive_path"
if command -v gzip >/dev/null 2>&1; then
  tmp_tar="$archive_path.tmp.tar"
  rm -f "$tmp_tar"
  COPYFILE_DISABLE=1 tar -cf "$tmp_tar" -C "$dist_dir" "$package_name"
  gzip -n -c "$tmp_tar" > "$archive_path"
  rm -f "$tmp_tar"
else
  COPYFILE_DISABLE=1 tar -czf "$archive_path" -C "$dist_dir" "$package_name"
fi
archive_sha256="$(sha256_file "$archive_path")"
printf '%s  %s\n' "$archive_sha256" "$(basename "$archive_path")" > "$checksums_path"

printf 'package=%s\n' "$archive_path"
printf 'sha256=%s\n' "$archive_sha256"
printf 'binary_sha256=%s\n' "$binary_sha256"
printf 'release_reference=%s\n' "$release_reference"
