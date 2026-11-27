#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-rpm.sh [options]

Build a Fedora/RPM package for RMUX from the Linux release binary.

Options:
  --configuration debug|release   Cargo profile to package (default: release)
  --target <triple>               Cargo target triple (default: x86_64-unknown-linux-gnu)
  --output-dir <path>             Output directory (default: target/dist)
  --release <rpm-release>         RPM release field (default: 1)
  --skip-build                    Package an existing binary
  --allow-stale-binary            Allow --skip-build for local-only packaging
  --reuse-release-binary          Allow --skip-build for a release binary built earlier in CI
  --payload-provenance <file>     Bind declared payload source to staged binaries
  SOURCE_DATE_EPOCH=<seconds>     RPM/changelog epoch (default: commit time)
  --homepage <url>                Package homepage (default: https://rmux.io)
  -h, --help                      Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

json_escape() {
  sed 's/\\/\\\\/g; s/"/\\"/g'
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

rpm_arch_for_target() {
  case "$1" in
    x86_64-unknown-linux-gnu) printf 'x86_64' ;;
    aarch64-unknown-linux-gnu) printf 'aarch64' ;;
    *) die "unsupported RPM target: $1" ;;
  esac
}

platform_label_for_target() {
  case "$1" in
    x86_64-unknown-linux-gnu) printf 'linux-x86_64' ;;
    aarch64-unknown-linux-gnu) printf 'linux-aarch64' ;;
    *) die "unsupported RPM target: $1" ;;
  esac
}

update_checksums() {
  local manifest file hash name tmp
  manifest="$1"
  file="$2"
  hash="$(sha256_file "$file")"
  name="$(basename "$file")"
  tmp="$(mktemp "${manifest}.XXXXXX")"
  if [ -f "$manifest" ]; then
    awk -v name="$name" '$2 != name { print }' "$manifest" > "$tmp"
  fi
  printf '%s  %s\n' "$hash" "$name" >> "$tmp"
  LC_ALL=C sort -k2,2 "$tmp" > "$manifest"
  rm -f "$tmp"
  printf '%s\n' "$hash"
}

strip_linux_tiny_binary() {
  local binary_path
  binary_path="$1"

  [ "$configuration" = "release" ] || return 0
  [ "${RMUX_PACKAGE_STRIP_TINY:-1}" = "1" ] || return 0
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

# The generated spec installs fresh copies. Pin their archive timestamps and
# the RPM build header to the source epoch, independently of wall clock and
# the hostname of the builder. Payload bytes and declared modes stay intact.
build_rpm_archive() {
  local topdir spec arch epoch
  topdir="$1"
  spec="$2"
  arch="$3"
  epoch="${SOURCE_DATE_EPOCH-$(git show -s --format=%ct HEAD)}"
  case "$epoch" in ''|*[!0-9]*) die "invalid SOURCE_DATE_EPOCH: $epoch" ;; esac
  SOURCE_DATE_EPOCH="$epoch" rpmbuild \
    --define "_topdir $topdir" \
    --define "_buildhost rmux" \
    --define "use_source_date_epoch_as_buildtime 1" \
    --define "clamp_mtime_to_source_date_epoch 1" \
    --target "$arch" \
    -bb "$spec"
}

# Resolve time once for the entire packaging operation. The exported value is
# also consumed by build_rpm_archive, so changelog and RPM headers agree.
set_rpm_source_time() {
  SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH-$(git show -s --format=%ct HEAD)}"
  case "$SOURCE_DATE_EPOCH" in ''|*[!0-9]*) die "invalid SOURCE_DATE_EPOCH: $SOURCE_DATE_EPOCH" ;; esac
  changelog_date="$(LC_ALL=C date -u -d "@$SOURCE_DATE_EPOCH" '+%a %b %d %Y')" ||
    die "invalid SOURCE_DATE_EPOCH: $SOURCE_DATE_EPOCH"
  export SOURCE_DATE_EPOCH
}

# RPM sources are flat, while the canonical provenance validator binds package
# paths. A temporary hardlink view exposes exactly those staged bytes without
# duplicating or executing them. The descriptor supplies both names and paths.
rpm_payload_provenance() {
  python3 - "$repo_root" "$work_dir" "$configuration" "$1" <<'PYTHON'
import json
import os
from pathlib import Path
import sys
import tempfile

repo, work, configuration, declaration_path = sys.argv[1:]
sys.path.insert(0, str(Path(repo) / "scripts/packaging"))
import layout

try:
    declaration = json.loads(Path(declaration_path).read_text())
    if not isinstance(declaration, dict) or declaration.get("configuration") != configuration:
        raise layout.LayoutError("payload provenance configuration differs")
    from layout_profiles import with_notices
    descriptor = with_notices(layout.load(Path(repo) / "scripts/packaging/rmux-package-layout.json"))
    roles = layout.payload_binary_roles(descriptor, "rpm")
    sources = {entry["id"]: entry["source"]["name"]
               for entry, _ in layout.entries(descriptor, "rpm") if entry["id"] in roles}
    with tempfile.TemporaryDirectory(prefix="payload-provenance.", dir=work) as root:
        for role, relative in roles.items():
            destination = Path(root) / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            os.link(Path(work) / "SOURCES" / sources[role], destination)
        result = layout.payload_provenance(descriptor, "rpm", root, declaration)
    print(layout.embeddable_field("payload_provenance", result))
except (OSError, ValueError, layout.LayoutError) as error:
    print("payload provenance: " + str(error), file=sys.stderr)
    sys.exit(1)
PYTHON
}

# Copy every non-binary package source the layout descriptor names into the
# RPM sources tree, under the file name the generated spec installs from.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
configuration="release"
target="x86_64-unknown-linux-gnu"
output_dir="target/dist"
rpm_release="1"
skip_build=0
allow_stale_binary=0
reuse_release_binary=0
payload_provenance_file=""
homepage="${RMUX_HOMEPAGE:-https://rmux.io}"

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
    --release)
      [ "$#" -ge 2 ] || die "--release requires a value"
      rpm_release="$2"
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
    --reuse-release-binary)
      reuse_release_binary=1
      shift
      ;;
    --payload-provenance)
      [ "$#" -ge 2 ] || die "--payload-provenance requires a value"
      payload_provenance_file="$2"
      shift 2
      ;;
    --homepage)
      [ "$#" -ge 2 ] || die "--homepage requires a value"
      homepage="$2"
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
case "$homepage" in http://*|https://*) ;; *) die "--homepage must be an http(s) URL" ;; esac
case "$rpm_release" in *[!0-9A-Za-z._~+%{}-]*|""|.*) die "invalid RPM release: $rpm_release" ;; esac
if [ "$allow_stale_binary" -eq 1 ] && [ "$reuse_release_binary" -eq 1 ]; then
  die "--allow-stale-binary and --reuse-release-binary are mutually exclusive"
fi
if [ "$reuse_release_binary" -eq 1 ] && [ "$skip_build" -eq 0 ]; then
  die "--reuse-release-binary requires --skip-build"
fi
if [ "$skip_build" -eq 1 ] && [ "$allow_stale_binary" -eq 0 ] && [ "$reuse_release_binary" -eq 0 ]; then
  die "--skip-build requires --allow-stale-binary for local packaging or --reuse-release-binary in release CI"
fi

need rpmbuild
need sha256sum

cd "$repo_root"
set_rpm_source_time
version="$(workspace_version)"
[ -n "$version" ] || die "unable to read workspace package version"
rpm_arch="$(rpm_arch_for_target "$target")"
platform_label="$(platform_label_for_target "$target")"

profile_dir="$configuration"
cargo_args=(build --package rmux --locked)
if [ "$configuration" = "release" ]; then
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
    --binary-dir "$(dirname "$binary")" -- "${compatible_build[@]}" "${cargo_args[@]}"
fi
notice_required=(--optional)
if [ "$skip_build" -eq 0 ] || [ "$reuse_release_binary" -eq 1 ]; then notice_required=(--required); fi
"${RMUX_PYTHON:-python3}" "$repo_root/scripts/packaging/notices.py" verify \
  --binary-dir "$(dirname "$binary")" "${notice_required[@]}"
[ -x "$binary" ] || die "expected executable binary was not found: $binary"
[ -x "$helper_binary" ] || die "expected executable private helper binary was not found: $helper_binary"
[ -x "$daemon_binary" ] || die "expected executable daemon binary was not found: $daemon_binary"

dist_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
work_dir="$dist_dir/rpmbuild"
spec_path="$work_dir/SPECS/rmux.spec"
checksums_path="$dist_dir/SHA256SUMS.txt"
completion_tmp=""
cleanup_package_work() {
  [ -z "$completion_tmp" ] || rm -rf "$completion_tmp"
  rm -rf "$work_dir"
}
trap cleanup_package_work EXIT
rm -rf "$work_dir"
mkdir -p "$work_dir/BUILD" "$work_dir/BUILDROOT" "$work_dir/RPMS" "$work_dir/SOURCES" "$work_dir/SPECS" "$work_dir/SRPMS"

packaged_binary="$work_dir/SOURCES/rmux"
packaged_helper="$work_dir/SOURCES/rmux-full"
packaged_daemon="$work_dir/SOURCES/rmux-daemon"
install -m 0755 "$binary" "$packaged_binary"
install -m 0755 "$helper_binary" "$packaged_helper"
install -m 0755 "$daemon_binary" "$packaged_daemon"
strip_linux_tiny_binary "$packaged_binary"
binary_abs="$(cd "$(dirname "$packaged_binary")" && pwd)/$(basename "$packaged_binary")"
helper_binary_abs="$(cd "$(dirname "$packaged_helper")" && pwd)/$(basename "$packaged_helper")"
daemon_binary_abs="$(cd "$(dirname "$packaged_daemon")" && pwd)/$(basename "$packaged_daemon")"
binary_sha256="$(sha256_file "$packaged_binary")"
helper_binary_sha256="$(sha256_file "$packaged_helper")"
daemon_binary_sha256="$(sha256_file "$packaged_daemon")"
binary_bytes="$(wc -c < "$packaged_binary" | tr -d ' ')"
helper_binary_bytes="$(wc -c < "$packaged_helper" | tr -d ' ')"
daemon_binary_bytes="$(wc -c < "$packaged_daemon" | tr -d ' ')"
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
git_commit="$(git rev-parse HEAD)"
git_dirty=false
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  git_dirty=true
fi
release_artifact=true
if [ "$configuration" != "release" ] || [ "$git_dirty" = true ] || { [ "$skip_build" -eq 1 ] && [ "$reuse_release_binary" -eq 0 ]; }; then
  release_artifact=false
fi
generated_at_utc="$(git show -s --format=%cI HEAD)"
payload_provenance_field=""
if [ -n "$payload_provenance_file" ]; then
  payload_provenance_field="$(rpm_payload_provenance "$payload_provenance_file")" ||
    die "declared payload provenance does not match the staged payload"
fi

# The format identity the package declares is the descriptor's, so a change of
# package contract moves it in one place for builders, validators and
# installers at once.
package_layout_id="$("$(layout_tool)" layout-id --variant rpm)" ||
  die "could not project the package layout identity"

metadata="$work_dir/SOURCES/artifact-metadata.json"
cat > "$metadata" <<EOF
{
  "schema": 1,
  "artifact_kind": "rpm-package-binary",
  "binary_path": "/usr/bin/rmux",
  "binary_sha256": "$binary_sha256",
  "binary_bytes": $binary_bytes,
  "helper_binary_path": "/usr/libexec/rmux/rmux",
  "helper_binary_sha256": "$helper_binary_sha256",
  "helper_binary_bytes": $helper_binary_bytes,
  "daemon_binary_path": "/usr/bin/rmux-daemon",
  "daemon_binary_sha256": "$daemon_binary_sha256",
  "daemon_binary_bytes": $daemon_binary_bytes,
  "binary_glibc_min": "$binary_glibc_min",
  "helper_binary_glibc_min": "$helper_binary_glibc_min",
  "daemon_binary_glibc_min": "$daemon_binary_glibc_min",
  "package_glibc_min": "$package_glibc_min",
  "max_supported_glibc": "$max_supported_glibc",
  "rmux_version": "$version",
  "git_commit": "$git_commit",
  "git_dirty": $git_dirty,
  "target": "$target",
  "platform_label": "$platform_label",
  "configuration": "$configuration",
  "package_schema": 1,
  "package_name": "rmux-$version-$rpm_release.$rpm_arch",
  "package_target": "$target",
  "package_target_label": "$platform_label",
  "package_layout": "$package_layout_id",
$payload_provenance_field
  "archive_format": "rpm",
  "skip_build": $([ "$skip_build" -eq 1 ] && printf true || printf false),
  "reuse_release_binary": $([ "$reuse_release_binary" -eq 1 ] && printf true || printf false),
  "release_artifact": $release_artifact,
  "generated_at_utc": "$generated_at_utc"
}
EOF

completion_tmp="$(mktemp -d "${TMPDIR:-/tmp}/rmux-completions.XXXXXX")"
if [ "$skip_build" -eq 0 ]; then
  # The generator writes under its own names and says what it wrote; those
  # files are then prepared under the names the descriptor gives them, so a
  # source renamed there is served by the generator's real output.
  completion_generated="$completion_tmp/generated"
  mkdir -p "$completion_generated"
  cargo run --quiet --package xtask -- generate-completions \
    --output-dir "$completion_generated" > "$completion_tmp/generated.tsv"
  prepare_generated_sources "rpm" completion \
    "$completion_tmp/generated.tsv" "$completion_tmp"
  rm -rf "$completion_cache"
  mkdir -p "$completion_cache"
  prepare_cached_sources "rpm" completion "$completion_tmp" "$completion_cache" \
    "generated completion is missing"
else
  prepare_cached_sources "rpm" completion "$completion_cache" "$completion_tmp" \
    "--skip-build requires prebuilt completions in $completion_cache; rerun without --skip-build or set RMUX_COMPLETIONS_DIR"
fi
stage_rpm_sources "$work_dir/SOURCES" "$repo_root" "$completion_tmp"

"${RMUX_PYTHON:-python3}" "$repo_root/scripts/packaging/notices.py" stage \
  --binary-dir "$(dirname "$binary")" --root "$work_dir/SOURCES" --variant rpm "${notice_required[@]}"

# The spec installs and owns exactly what the shared layout descriptor
# projects for the RPM variant, with its modes.
rpm_install_lines="$("$(layout_tool)" rpm-spec-install \
  --binaries-dir "$work_dir/SOURCES")" || die "could not project the RPM install list"
rpm_files_lines="$("$(layout_tool)" rpm-files)" || die "could not project the RPM file list"

cat > "$spec_path" <<EOF
%global debug_package %{nil}
%global _build_id_links none
%global __os_install_post %{nil}

Name: rmux
Version: $version
Release: $rpm_release%{?dist}
Summary: Terminal multiplexer with a tmux-style CLI
License: MIT OR Apache-2.0
URL: $homepage
Requires: glibc >= $package_glibc_min
AutoReqProv: no

%description
RMUX is a local terminal multiplexer with a tmux-compatible command surface,
a daemon runtime, a Rust SDK, and native Windows support.

%prep

%build

%install
rm -rf %{buildroot}
$rpm_install_lines

%files
$rpm_files_lines

%changelog
* $changelog_date Helvesec <release@rmux.io> - $version-$rpm_release
- RMUX $version binary package.
EOF

build_rpm_archive "$work_dir" "$spec_path" "$rpm_arch"

rpm_path="$(find "$work_dir/RPMS" -type f -name 'rmux-*.rpm' | LC_ALL=C sort | head -n 1)"
[ -n "$rpm_path" ] || die "rpmbuild did not produce an RPM"
archive_path="$dist_dir/$(basename "$rpm_path")"
rm -f "$archive_path"
mv "$rpm_path" "$archive_path"
archive_sha256="$(update_checksums "$checksums_path" "$archive_path")"
"${RMUX_PYTHON:-python3}" "$repo_root/scripts/packaging/notices.py" publish \
  --binary-dir "$(dirname "$binary")" --archive "$archive_path" "${notice_required[@]}"

printf 'package=%s\n' "$archive_path"
printf 'sha256=%s\n' "$archive_sha256"
printf 'binary_sha256=%s\n' "$binary_sha256"
printf 'helper_binary_sha256=%s\n' "$helper_binary_sha256"
printf 'daemon_binary_sha256=%s\n' "$daemon_binary_sha256"
printf 'release_artifact=%s\n' "$release_artifact"
