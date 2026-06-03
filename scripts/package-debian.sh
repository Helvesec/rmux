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
if [ "$skip_build" -eq 0 ]; then
  cargo "${cargo_args[@]}"
fi

target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/$target/$profile_dir/rmux"
[ -x "$binary" ] || die "expected executable binary was not found: $binary"

dist_dir="$(mkdir -p "$output_dir" && cd "$output_dir" && pwd)"
package_name="rmux_${version}_${deb_arch}"
stage_dir="$dist_dir/debian/$package_name"
archive_path="$dist_dir/$package_name.deb"
checksums_path="$dist_dir/SHA256SUMS.txt"

rm -rf "$stage_dir"
mkdir -p \
  "$stage_dir/DEBIAN" \
  "$stage_dir/usr/bin" \
  "$stage_dir/usr/share/doc/rmux" \
  "$stage_dir/usr/share/man/man1" \
  "$stage_dir/usr/share/rmux"

install -m 0755 "$binary" "$stage_dir/usr/bin/rmux"
gzip -n -c rmux.1 > "$stage_dir/usr/share/man/man1/rmux.1.gz"
install -m 0644 README.md "$stage_dir/usr/share/doc/rmux/README.md"
install -m 0644 LICENSE-APACHE LICENSE-MIT "$stage_dir/usr/share/doc/rmux/"

binary_abs="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"
binary_sha256="$(sha256_file "$binary")"
binary_bytes="$(wc -c < "$binary" | tr -d ' ')"
git_commit="$(git rev-parse HEAD)"
git_dirty=false
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  git_dirty=true
fi
release_artifact=true
if [ "$git_dirty" = true ] || { [ "$skip_build" -eq 1 ] && [ "$reuse_release_binary" -eq 0 ]; }; then
  release_artifact=false
fi
generated_at_utc="$(git show -s --format=%cI HEAD)"

cat > "$stage_dir/usr/share/rmux/artifact-metadata.json" <<EOF
{
  "schema": 1,
  "artifact_kind": "debian-package-binary",
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
  "package_layout": "rmux-debian-package-v1",
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
Depends: libc6 (>= 2.31), libgcc-s1 (>= 3.0)
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
printf 'release_artifact=%s\n' "$release_artifact"
