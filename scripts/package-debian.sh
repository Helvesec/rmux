#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/package-debian.sh [options]

Build a Debian/Ubuntu .deb package for RMUX from the Linux release binary.

Options:
  --configuration debug|release   Cargo profile to package (default: release)
  --target <triple>               Cargo target triple (default: x86_64-unknown-linux-gnu)
  --output-dir <path>             Output directory (default: target/dist)
  --skip-build                    Package an existing binary
  --allow-stale-binary            Allow --skip-build for local-only packaging
  --reuse-release-binary          Allow --skip-build for a release binary built earlier in CI
  --maintainer <name>             Debian maintainer field (default: Helvesec <release@rmux.io>)
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

deb_arch_for_target() {
  case "$1" in
    x86_64-unknown-linux-gnu) printf 'amd64' ;;
    aarch64-unknown-linux-gnu) printf 'arm64' ;;
    *) die "unsupported Debian target: $1" ;;
  esac
}

platform_label_for_target() {
  case "$1" in
    x86_64-unknown-linux-gnu) printf 'linux-x86_64' ;;
    aarch64-unknown-linux-gnu) printf 'linux-aarch64' ;;
    *) die "unsupported Debian target: $1" ;;
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

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
configuration="release"
target="x86_64-unknown-linux-gnu"
output_dir="target/dist"
skip_build=0
allow_stale_binary=0
reuse_release_binary=0
maintainer="${RMUX_DEB_MAINTAINER:-Helvesec <release@rmux.io>}"
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
    --maintainer)
      [ "$#" -ge 2 ] || die "--maintainer requires a value"
      maintainer="$2"
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
if [ "$allow_stale_binary" -eq 1 ] && [ "$reuse_release_binary" -eq 1 ]; then
  die "--allow-stale-binary and --reuse-release-binary are mutually exclusive"
fi
if [ "$reuse_release_binary" -eq 1 ] && [ "$skip_build" -eq 0 ]; then
  die "--reuse-release-binary requires --skip-build"
fi
if [ "$skip_build" -eq 1 ] && [ "$allow_stale_binary" -eq 0 ] && [ "$reuse_release_binary" -eq 0 ]; then
  die "--skip-build requires --allow-stale-binary for local packaging or --reuse-release-binary in release CI"
fi

need dpkg-deb
need gzip
need sha256sum

cd "$repo_root"
version="$(workspace_version)"
[ -n "$version" ] || die "unable to read workspace package version"
deb_arch="$(deb_arch_for_target "$target")"
platform_label="$(platform_label_for_target "$target")"

profile_dir="$configuration"
cargo_args=(build --package rmux --locked --target "$target")
if [ "$configuration" = "release" ]; then
  cargo_args+=(--release)
fi

target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/$target/$profile_dir/rmux"
helper_binary="$target_dir/$target/$profile_dir/rmux-full"
daemon_binary="$target_dir/$target/$profile_dir/rmux-daemon"
completion_cache="${RMUX_COMPLETIONS_DIR:-$target_dir/$target/$profile_dir/completions}"
if [ "$skip_build" -eq 0 ]; then
  cargo "${cargo_args[@]}" --bin rmux
  cp "$binary" "$helper_binary"
  cargo "${cargo_args[@]}" --features tiny-cli --bin rmux
  cargo "${cargo_args[@]}" --bin rmux-daemon
fi
[ -x "$binary" ] || die "expected executable binary was not found: $binary"
[ -x "$helper_binary" ] || die "expected executable private helper binary was not found: $helper_binary"
[ -x "$daemon_binary" ] || die "expected executable daemon binary was not found: $daemon_binary"

dist_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
package_name="rmux_${version}_${deb_arch}"
stage_dir="$dist_dir/debian/$package_name"
archive_path="$dist_dir/$package_name.deb"
checksums_path="$dist_dir/SHA256SUMS.txt"
completion_tmp=""
cleanup_package_work() {
  [ -z "$completion_tmp" ] || rm -rf "$completion_tmp"
  rm -rf "$stage_dir"
}
trap cleanup_package_work EXIT

rm -rf "$stage_dir"
mkdir -p \
  "$stage_dir/DEBIAN" \
  "$stage_dir/usr/bin" \
  "$stage_dir/usr/libexec/rmux" \
  "$stage_dir/usr/share/doc/rmux" \
  "$stage_dir/usr/share/bash-completion/completions" \
  "$stage_dir/usr/share/zsh/site-functions" \
  "$stage_dir/usr/share/fish/vendor_completions.d" \
  "$stage_dir/usr/share/powershell/Completions" \
  "$stage_dir/usr/share/elvish/lib" \
  "$stage_dir/usr/share/man/man1" \
  "$stage_dir/usr/share/rmux"

install -m 0755 "$binary" "$stage_dir/usr/bin/rmux"
install -m 0755 "$helper_binary" "$stage_dir/usr/libexec/rmux/rmux"
install -m 0755 "$daemon_binary" "$stage_dir/usr/bin/rmux-daemon"
strip_linux_tiny_binary "$stage_dir/usr/bin/rmux"
gzip -n -c docs/man/rmux.1 > "$stage_dir/usr/share/man/man1/rmux.1.gz"
completion_tmp="$(mktemp -d "${TMPDIR:-/tmp}/rmux-completions.XXXXXX")"
if [ "$skip_build" -eq 0 ]; then
  cargo run --quiet --package xtask -- generate-completions --output-dir "$completion_tmp" >/dev/null
  rm -rf "$completion_cache"
  mkdir -p "$completion_cache"
  cp "$completion_tmp/rmux.bash" "$completion_tmp/_rmux" "$completion_tmp/rmux.fish" \
    "$completion_tmp/_rmux.ps1" "$completion_tmp/rmux.elv" "$completion_cache/"
else
  for completion_file in rmux.bash _rmux rmux.fish _rmux.ps1 rmux.elv; do
    [ -f "$completion_cache/$completion_file" ] || die "--skip-build requires prebuilt completions in $completion_cache; rerun without --skip-build or set RMUX_COMPLETIONS_DIR"
    cp "$completion_cache/$completion_file" "$completion_tmp/$completion_file"
  done
fi
install -m 0644 "$completion_tmp/rmux.bash" "$stage_dir/usr/share/bash-completion/completions/rmux"
install -m 0644 "$completion_tmp/_rmux" "$stage_dir/usr/share/zsh/site-functions/_rmux"
install -m 0644 "$completion_tmp/rmux.fish" "$stage_dir/usr/share/fish/vendor_completions.d/rmux.fish"
install -m 0644 "$completion_tmp/_rmux.ps1" "$stage_dir/usr/share/powershell/Completions/_rmux.ps1"
install -m 0644 "$completion_tmp/rmux.elv" "$stage_dir/usr/share/elvish/lib/rmux.elv"
install -m 0644 README.md "$stage_dir/usr/share/doc/rmux/README.md"
install -m 0644 LICENSE-APACHE LICENSE-MIT "$stage_dir/usr/share/doc/rmux/"

packaged_binary="$stage_dir/usr/bin/rmux"
packaged_helper="$stage_dir/usr/libexec/rmux/rmux"
packaged_daemon="$stage_dir/usr/bin/rmux-daemon"
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
binary_glibc_min="$($glibc_floor_script "$packaged_binary")"
helper_binary_glibc_min="$($glibc_floor_script "$packaged_helper")"
daemon_binary_glibc_min="$($glibc_floor_script "$packaged_daemon")"
package_glibc_min="$($glibc_floor_script "$packaged_binary" "$packaged_helper" "$packaged_daemon")"
max_supported_glibc="${RMUX_MAX_SUPPORTED_GLIBC:-2.35}"
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

cat > "$stage_dir/usr/share/rmux/artifact-metadata.json" <<EOF
{
  "schema": 1,
  "artifact_kind": "debian-package-binary",
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
  "package_name": "$package_name",
  "package_target": "$target",
  "package_target_label": "$platform_label",
  "package_layout": "rmux-debian-package-v2",
  "archive_format": "deb",
  "skip_build": $([ "$skip_build" -eq 1 ] && printf true || printf false),
  "reuse_release_binary": $([ "$reuse_release_binary" -eq 1 ] && printf true || printf false),
  "release_artifact": $release_artifact,
  "generated_at_utc": "$generated_at_utc"
}
EOF

cat > "$stage_dir/usr/share/doc/rmux/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: RMUX
Source: https://github.com/Helvesec/rmux

Files: *
Copyright: Helvesec
License: MIT or Apache-2.0

License: MIT
 See /usr/share/doc/rmux/LICENSE-MIT.

License: Apache-2.0
 See /usr/share/doc/rmux/LICENSE-APACHE.
EOF

installed_size="$(du -sk "$stage_dir/usr" | awk '{print $1}')"
cat > "$stage_dir/DEBIAN/control" <<EOF
Package: rmux
Version: $version
Section: utils
Priority: optional
Architecture: $deb_arch
Installed-Size: $installed_size
Maintainer: $maintainer
Depends: libc6 (>= $package_glibc_min), libgcc-s1 (>= 3.0)
Homepage: $homepage
Description: Terminal multiplexer with a tmux-style CLI
 RMUX is a local terminal multiplexer with a tmux-compatible command surface,
 a daemon runtime, a Rust SDK, and native Windows support.
EOF

rm -f "$archive_path"
dpkg-deb --root-owner-group --build "$stage_dir" "$archive_path"
archive_sha256="$(update_checksums "$checksums_path" "$archive_path")"

printf 'package=%s\n' "$archive_path"
printf 'sha256=%s\n' "$archive_sha256"
printf 'binary_sha256=%s\n' "$binary_sha256"
printf 'helper_binary_sha256=%s\n' "$helper_binary_sha256"
printf 'daemon_binary_sha256=%s\n' "$daemon_binary_sha256"
printf 'release_artifact=%s\n' "$release_artifact"
