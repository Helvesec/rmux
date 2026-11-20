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
  --allow-stale-binary            Allow --skip-build for local-only packaging
  --payload-provenance <file>     Declare where the packaged binaries were
                                  built, for a payload this repository did not
                                  build; every declared digest must be the
                                  digest of the file really staged
  SOURCE_DATE_EPOCH=<seconds>     Archive mtime override (default: commit time)
  RMUX_PACKAGE_CODESIGN_ADHOC=1   Ad-hoc sign the binary before hashing (macOS)
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
  epoch="${SOURCE_DATE_EPOCH:-$(git show -s --format=%ct HEAD)}"
  case "$epoch" in ''|*[!0-9]*) die "SOURCE_DATE_EPOCH must be a non-negative integer" ;; esac
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

strip_linux_tiny_binary() {
  local binary_path
  binary_path="$1"

  [ "$configuration" = "release" ] || return 0
  [ "${RMUX_PACKAGE_STRIP_TINY:-1}" = "1" ] || return 0
  case "$target" in
    *-unknown-linux-gnu) ;;
    *) return 0 ;;
  esac
  command -v strip >/dev/null 2>&1 || return 0
  strip -s "$binary_path" || die "failed to strip package binary: $binary_path"
}

layout_tool() {
  printf '%s\n' "$repo_root/scripts/packaging/layout-current"
}

# Preparing sources and staging them are the same operations for every shell
# builder; they live beside the descriptor and are called from here. The path
# comes from this script, which is known before any option is read.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/packaging/prepare-sources.sh"

# Copy what the shared package layout descriptor projects for one variant.
# Refuse a staged package that omits a declared file, carries an extra one or
# uses another mode, before anything is archived.
# A builder writes its generated files - the metadata, the manifest, the
# checksums, the Debian copyright - through its own umask, which differs
# between hosts. Their declared mode is applied here, so one payload does not
# reach two modes on two machines.
apply_generated_modes() {
  local variant stage path mode
  variant="$1"
  stage="$2"
  while IFS="$(printf '\t')" read -r path mode; do
    [ -n "$path" ] || continue
    [ -e "$stage/$path" ] || continue
    chmod "$mode" "$stage/$path"
  done <<EOF
$("$(layout_tool)" generated-modes --variant "$variant" --package-root "$stage")
EOF
}

check_package_layout() {
  "$(layout_tool)" check-tree --variant "$1" --root "$2" ||
    die "staged package does not match the layout descriptor"
}

unix_payload_provenance() {
  python3 - "$repo_root" "$stage_dir" "$configuration" "$1" <<'PYTHON'
import json
from pathlib import Path
import sys

repo, stage, configuration, declaration_path = sys.argv[1:]
sys.path.insert(0, str(Path(repo) / "scripts/packaging"))
import layout

try:
    declaration = json.loads(Path(declaration_path).read_text())
    if not isinstance(declaration, dict) or declaration.get("configuration") != configuration:
        raise layout.LayoutError("payload provenance configuration differs")
    from layout_profiles import with_notices
    descriptor = with_notices(layout.load(Path(repo) / "scripts/packaging/rmux-package-layout.json"))
    result = layout.payload_provenance(descriptor, "unix-archive", stage, declaration)
    roles = layout.payload_binary_roles(descriptor, "unix-archive")
    for item in declaration["files"]:
        if "bytes" in item and (type(item["bytes"]) is not int
                                or item["bytes"] != (Path(stage) / roles[item["role"]]).stat().st_size):
            raise layout.LayoutError("payload provenance size differs: " + item["role"])
    print(layout.embeddable_field("payload_provenance", result))
except (OSError, ValueError, layout.LayoutError) as error:
    print("payload provenance: " + str(error), file=sys.stderr)
    sys.exit(1)
PYTHON
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

validate_platform_label() {
  case "$1" in
    ""|*[!A-Za-z0-9_.-]*)
      die "platform label must contain only ASCII letters, digits, '.', '_' or '-'"
      ;;
  esac
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
configuration="release"
target=""
output_dir="target/dist"
platform_label=""
skip_build=0
allow_stale_binary=0
payload_provenance_file=""

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
    --payload-provenance)
      [ "$#" -ge 2 ] || die "--payload-provenance requires a value"
      payload_provenance_file="$2"
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
validate_platform_label "$platform_label"

profile_dir="debug"
cargo_args=(build --locked)
if [ "$configuration" = "release" ]; then
  profile_dir="release"
  cargo_args+=(--release)
fi
compatible_build=("$repo_root/scripts/release/cargo-build-compatible.sh" --target "$target" --)

target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/$target/$profile_dir/rmux"
helper_binary="$target_dir/$target/$profile_dir/rmux-full"
daemon_binary="$target_dir/$target/$profile_dir/rmux-daemon"
completion_cache="${RMUX_COMPLETIONS_DIR:-$target_dir/$target/$profile_dir/completions}"

if [ "$skip_build" -eq 0 ]; then
  "${RMUX_PYTHON:-python3}" "$repo_root/scripts/packaging/notices.py" build \
    --binary-dir "$(dirname "$binary")" -- "${compatible_build[@]}" "${cargo_args[@]}" --package rmux
elif [ "$allow_stale_binary" -eq 0 ]; then
  die "--skip-build is local-only packaging; pass --allow-stale-binary to acknowledge that"
fi

[ -f "$binary" ] || die "expected binary was not found: $binary"
[ -x "$binary" ] || die "expected binary is not executable: $binary"
[ -f "$helper_binary" ] || die "expected private helper binary was not found: $helper_binary"
[ -x "$helper_binary" ] || die "expected private helper binary is not executable: $helper_binary"
[ -f "$daemon_binary" ] || die "expected daemon binary was not found: $daemon_binary"
[ -x "$daemon_binary" ] || die "expected daemon binary is not executable: $daemon_binary"
if [ "${RMUX_PACKAGE_CODESIGN_ADHOC:-0}" = "1" ]; then
  command -v codesign >/dev/null 2>&1 || die "RMUX_PACKAGE_CODESIGN_ADHOC=1 requires codesign"
  codesign --force --sign - "$binary"
  codesign --force --sign - "$helper_binary"
  codesign --force --sign - "$daemon_binary"
  codesign --verify --verbose=2 "$binary"
  codesign --verify --verbose=2 "$helper_binary"
  codesign --verify --verbose=2 "$daemon_binary"
fi

dist_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
package_name="rmux-$version-$platform_label"
stage_dir="$dist_dir/$package_name"
archive_path="$dist_dir/$package_name.tar.gz"
checksums_path="$dist_dir/SHA256SUMS.txt"
completion_tmp=""
tmp_tar=""
cleanup_package_work() {
  [ -z "$completion_tmp" ] || rm -rf "$completion_tmp"
  [ -z "$tmp_tar" ] || rm -f "$tmp_tar"
  rm -rf "$stage_dir"
}
trap cleanup_package_work EXIT

case "$stage_dir" in "$dist_dir"/*) ;; *) die "stage path escapes output dir" ;; esac
rm -rf "$stage_dir"
mkdir -p "$stage_dir/bin" "$stage_dir/libexec/rmux" "$stage_dir/share/man/man1" "$stage_dir/share/rmux"

install -m 0755 "$binary" "$stage_dir/bin/rmux"
install -m 0755 "$helper_binary" "$stage_dir/libexec/rmux/rmux"
install -m 0755 "$daemon_binary" "$stage_dir/bin/rmux-daemon"
strip_linux_tiny_binary "$stage_dir/bin/rmux"
completion_tmp="$(mktemp -d "${TMPDIR:-/tmp}/rmux-completions.XXXXXX")"
if [ "$skip_build" -eq 0 ]; then
  # The generator writes under its own names and says what it wrote; those
  # files are then prepared under the names the descriptor gives them, so a
  # source renamed there is served by the generator's real output.
  completion_generated="$completion_tmp/generated"
  mkdir -p "$completion_generated"
  cargo run --quiet --package xtask -- generate-completions \
    --output-dir "$completion_generated" > "$completion_tmp/generated.tsv"
  prepare_generated_sources "unix-archive" completion \
    "$completion_tmp/generated.tsv" "$completion_tmp"
  rm -rf "$completion_cache"
  mkdir -p "$completion_cache"
  prepare_cached_sources "unix-archive" completion "$completion_tmp" "$completion_cache" \
    "generated completion is missing"
else
  prepare_cached_sources "unix-archive" completion "$completion_cache" "$completion_tmp" \
    "--skip-build requires prebuilt completions in $completion_cache; rerun without --skip-build or set RMUX_COMPLETIONS_DIR"
fi
# Every packaged file that is not a built binary or generated here - the
# installer, the man page, the five completions and the licenses - is staged
# from the shared layout descriptor, with the destination and mode it declares.
stage_from_descriptor "unix-archive" "$stage_dir" \
  --repo "$repo_root" --completions "$completion_tmp" --only-sources repo,completion

notice_required=(--optional)
[ "$skip_build" -ne 0 ] || notice_required=(--required)
"${RMUX_PYTHON:-python3}" "$repo_root/scripts/packaging/notices.py" stage \
  --binary-dir "$(dirname "$binary")" --root "$stage_dir" --variant unix-archive "${notice_required[@]}"

packaged_binary="$stage_dir/bin/rmux"
packaged_helper="$stage_dir/libexec/rmux/rmux"
packaged_daemon="$stage_dir/bin/rmux-daemon"
binary_abs="$(cd "$(dirname "$packaged_binary")" && pwd)/$(basename "$packaged_binary")"
helper_binary_abs="$(cd "$(dirname "$packaged_helper")" && pwd)/$(basename "$packaged_helper")"
daemon_binary_abs="$(cd "$(dirname "$packaged_daemon")" && pwd)/$(basename "$packaged_daemon")"
binary_sha256="$(sha256_file "$packaged_binary")"
helper_binary_sha256="$(sha256_file "$packaged_helper")"
daemon_binary_sha256="$(sha256_file "$packaged_daemon")"
binary_bytes="$(wc -c < "$packaged_binary" | tr -d ' ')"
helper_binary_bytes="$(wc -c < "$packaged_helper" | tr -d ' ')"
daemon_binary_bytes="$(wc -c < "$packaged_daemon" | tr -d ' ')"
# Two file slots may legitimately carry the same image in a local package: the
# descriptor requires three paths, not three different builds. Saying so here
# keeps the package honest without asserting a tiny dispatcher or a delegation
# to the helper that this payload does not implement.
full_cli_reused_in_two_slots=false
if [ "$binary_sha256" = "$helper_binary_sha256" ]; then
  full_cli_reused_in_two_slots=true
fi

# A payload this repository did not build is declared, bound to the staged
# files by the projection, and refused unless every digest matches.
payload_provenance_field=""
if [ -n "$payload_provenance_file" ]; then
  [ -f "$payload_provenance_file" ] || die "payload provenance file not found: $payload_provenance_file"
  payload_provenance_field="$(unix_payload_provenance "$payload_provenance_file")" ||
    die "declared payload provenance does not match the staged payload"
fi

glibc_metadata_fields=""
package_glibc_min=""
case "$target" in
  *-unknown-linux-gnu)
    glibc_floor_script="$repo_root/scripts/glibc-symbol-floor.sh"
    binary_glibc_min="$("$glibc_floor_script" "$packaged_binary")"
    helper_binary_glibc_min="$("$glibc_floor_script" "$packaged_helper")"
    daemon_binary_glibc_min="$("$glibc_floor_script" "$packaged_daemon")"
    package_glibc_min="$("$glibc_floor_script" "$packaged_binary" "$packaged_helper" "$packaged_daemon")"
    max_supported_glibc="${RMUX_MAX_SUPPORTED_GLIBC:-2.31}"
    case "$max_supported_glibc" in ''|*[!0-9.]*|.*|*.|*..*) die "invalid RMUX_MAX_SUPPORTED_GLIBC: $max_supported_glibc" ;; esac
    if [ "$(printf '%s\n%s\n' "$package_glibc_min" "$max_supported_glibc" | LC_ALL=C sort -V | tail -n 1)" != "$max_supported_glibc" ]; then
      die "packaged binaries require GLIBC_$package_glibc_min, newer than supported GLIBC_$max_supported_glibc; rebuild in the oldest supported sysroot"
    fi
    glibc_metadata_fields="$(printf '%s\n' \
      "  \"binary_glibc_min\": \"$binary_glibc_min\"," \
      "  \"helper_binary_glibc_min\": \"$helper_binary_glibc_min\"," \
      "  \"daemon_binary_glibc_min\": \"$daemon_binary_glibc_min\"," \
      "  \"package_glibc_min\": \"$package_glibc_min\"," \
      "  \"max_supported_glibc\": \"$max_supported_glibc\",")"
    ;;
esac
git_commit="$(git rev-parse HEAD)"
git_dirty=false
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  git_dirty=true
fi
release_artifact=true
if [ "$configuration" != "release" ] || [ "$skip_build" -eq 1 ] || [ "$git_dirty" = true ]; then
  release_artifact=false
fi
generated_at_utc="$(commit_time_iso)"

# The format identity the package declares is the descriptor's, so a change of
# package contract moves it in one place for builders, validators and
# installers at once.
package_layout_id="$("$(layout_tool)" layout-id --variant unix-archive)" ||
  die "could not project the package layout identity"

cat > "$stage_dir/share/rmux/artifact-metadata.json" <<EOF
{
  "schema": 1,
  "artifact_kind": "unix-package-binary",
  "binary_path": "bin/rmux",
  "binary_sha256": "$binary_sha256",
  "binary_bytes": $binary_bytes,
  "helper_binary_path": "libexec/rmux/rmux",
  "helper_binary_sha256": "$helper_binary_sha256",
  "helper_binary_bytes": $helper_binary_bytes,
  "daemon_binary_path": "bin/rmux-daemon",
  "daemon_binary_sha256": "$daemon_binary_sha256",
  "daemon_binary_bytes": $daemon_binary_bytes,
$glibc_metadata_fields
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
  "package_layout": "$package_layout_id",
  "full_cli_reused_in_two_slots": $full_cli_reused_in_two_slots,
$payload_provenance_field
  "archive_format": "tar.gz",
  "archive_reproducibility": "normalized-mtime-gzip-no-name",
  "skip_build": $([ "$skip_build" -eq 1 ] && printf true || printf false),
  "release_artifact": $release_artifact,
  "generated_at_utc": "$generated_at_utc"
}
EOF

# The installer inside the package installs what the descriptor projects, so
# the projection travels with the package instead of being repeated there.
"$(layout_tool)" manifest --variant unix-archive > "$stage_dir/share/rmux/package-layout.tsv" ||
  die "could not project the package manifest"
chmod 0644 "$stage_dir/share/rmux/package-layout.tsv"

write_package_checksums "$stage_dir" "$stage_dir/SHA256SUMS.txt"
apply_generated_modes "unix-archive" "$stage_dir"
check_package_layout "unix-archive" "$stage_dir"
touch_stamp="$(commit_touch_timestamp)"
# Directory permissions must not depend on the staging process umask.
find "$stage_dir" -type d -exec chmod 0755 {} +
# commit_touch_timestamp formats UTC; touch otherwise interprets the local TZ.
TZ=UTC find "$stage_dir" -exec touch -t "$touch_stamp" {} +

rm -f "$archive_path"
if command -v gzip >/dev/null 2>&1; then
  tmp_tar="$archive_path.tmp.tar"
  rm -f "$tmp_tar"
  case "$(tar --version 2>/dev/null)" in
    *"GNU tar"*)
      # Fix Linux headers and traversal independently of the host user,
      # filesystem insertion order and tar's default extended format.
      COPYFILE_DISABLE=1 LC_ALL=C tar --sort=name --format=ustar \
        --owner=0 --group=0 --numeric-owner \
        -cf "$tmp_tar" -C "$dist_dir" "$package_name"
      ;;
    *) COPYFILE_DISABLE=1 tar -cf "$tmp_tar" -C "$dist_dir" "$package_name" ;;
  esac
  gzip -n -c "$tmp_tar" > "$archive_path"
  rm -f "$tmp_tar"
else
  COPYFILE_DISABLE=1 tar -czf "$archive_path" -C "$dist_dir" "$package_name"
fi
archive_sha256="$(sha256_file "$archive_path")"
printf '%s  %s\n' "$archive_sha256" "$(basename "$archive_path")" > "$checksums_path"
"${RMUX_PYTHON:-python3}" "$repo_root/scripts/packaging/notices.py" publish \
  --binary-dir "$(dirname "$binary")" --archive "$archive_path" "${notice_required[@]}"

printf 'package=%s\n' "$archive_path"
printf 'sha256=%s\n' "$archive_sha256"
printf 'binary_sha256=%s\n' "$binary_sha256"
printf 'helper_binary_sha256=%s\n' "$helper_binary_sha256"
printf 'daemon_binary_sha256=%s\n' "$daemon_binary_sha256"
[ -z "$package_glibc_min" ] || printf 'glibc_min=%s\n' "$package_glibc_min"
printf 'release_artifact=%s\n' "$release_artifact"
printf 'full_cli_reused_in_two_slots=%s\n' "$full_cli_reused_in_two_slots"
