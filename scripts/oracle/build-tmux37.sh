#!/usr/bin/env bash
set -euo pipefail

TMUX_VERSION="3.7b"
TMUX_SOURCE_COMMIT="e802909de06012a4df6209d55e86487c56223163"
TMUX_SOURCE_TARBALL_SHA256="87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96"
TMUX_SOURCE_URL="https://github.com/tmux/tmux/releases/download/${TMUX_VERSION}/tmux-${TMUX_VERSION}.tar.gz"
DEFAULT_PREFIX="/opt/rmux/reference/tmux-frozen/${TMUX_SOURCE_COMMIT}"

usage() {
  cat <<'USAGE'
Usage: scripts/oracle/build-tmux37.sh [options]

Build the pinned RMUX 0.9 release-line tmux oracle (tmux 3.7b) from the upstream
release tarball and install it as <prefix>/tmux. The script writes
<prefix>/tmux.reference so the Rust harness can verify source SHA, tarball SHA,
version, and binary hash.

Options:
  --prefix DIR       Install oracle files under DIR. Defaults to /opt/rmux/reference/tmux-frozen/<source-sha>.
  --cache-dir DIR    Cache downloaded source tarballs under DIR. Defaults to ~/.cache/rmux-oracle.
  --jobs N           make -j value. Defaults to nproc/sysctl, then 2.
  -h, --help         Show this help.

Linux CI prerequisites:
  build-essential bison libevent-dev libncurses-dev libutf8proc-dev pkg-config curl ca-certificates
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    die "missing sha256sum, shasum, or openssl"
  fi
}

default_jobs() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.ncpu
  else
    printf '2\n'
  fi
}

prefix="$DEFAULT_PREFIX"
cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/rmux-oracle"
jobs="$(default_jobs)"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix requires a directory"
      prefix="$2"
      shift 2
      ;;
    --cache-dir)
      [ "$#" -ge 2 ] || die "--cache-dir requires a directory"
      cache_dir="$2"
      shift 2
      ;;
    --jobs)
      [ "$#" -ge 2 ] || die "--jobs requires a value"
      jobs="$2"
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

case "$prefix" in
  /*) ;;
  *) prefix="$(pwd)/$prefix" ;;
esac
case "$cache_dir" in
  /*) ;;
  *) cache_dir="$(pwd)/$cache_dir" ;;
esac

need awk
need cc
need curl
need make
need pkg-config
need tar

mkdir -p "$cache_dir"
tarball="$cache_dir/tmux-${TMUX_VERSION}.tar.gz"
if [ ! -f "$tarball" ]; then
  download_tmp="$(mktemp "${tarball}.tmp.XXXXXX")"
  if ! curl -L --fail --show-error --silent "$TMUX_SOURCE_URL" -o "$download_tmp"; then
    rm -f "$download_tmp"
    die "failed to download tmux source tarball"
  fi
  actual_download_sha="$(sha256_file "$download_tmp")"
  if [ "$actual_download_sha" != "$TMUX_SOURCE_TARBALL_SHA256" ]; then
    rm -f "$download_tmp"
    die "tmux source tarball sha256 mismatch: got $actual_download_sha expected $TMUX_SOURCE_TARBALL_SHA256"
  fi
  mv -f "$download_tmp" "$tarball"
fi

workdir="$(mktemp -d "${TMPDIR:-/tmp}/rmux-tmux37-build.XXXXXX")"
cleanup() {
  rm -rf "$workdir"
}
trap cleanup EXIT

mkdir -p "$workdir/source" "$workdir/install"
verified_tarball="$workdir/tmux-${TMUX_VERSION}.tar.gz"
cp "$tarball" "$verified_tarball"
actual_tarball_sha="$(sha256_file "$verified_tarball")"
if [ "$actual_tarball_sha" != "$TMUX_SOURCE_TARBALL_SHA256" ]; then
  rm -f "$tarball"
  die "tmux source tarball sha256 mismatch: got $actual_tarball_sha expected $TMUX_SOURCE_TARBALL_SHA256"
fi
tar -xzf "$verified_tarball" -C "$workdir/source"
src="$workdir/source/tmux-${TMUX_VERSION}"
[ -d "$src" ] || die "expected source directory $src"

(
  cd "$src"
  configure_args=(--prefix="$workdir/install")
  if pkg-config --exists utf8proc; then
    configure_args+=(--enable-utf8proc)
  else
    configure_args+=(--disable-utf8proc)
  fi
  ./configure "${configure_args[@]}"
  make -j "$jobs"
  make install
)

mkdir -p "$prefix"
install -m 755 "$workdir/install/bin/tmux" "$prefix/tmux"
version="$("$prefix/tmux" -V)"
[ "$version" = "tmux ${TMUX_VERSION}" ] || die "built oracle reports '$version', expected 'tmux ${TMUX_VERSION}'"
binary_sha="$(sha256_file "$prefix/tmux")"

cat >"$prefix/tmux.reference" <<EOF
source_tag: "${TMUX_VERSION}"
source_sha: "${TMUX_SOURCE_COMMIT}"
source_tarball_url: "${TMUX_SOURCE_URL}"
source_tarball_sha256: "${TMUX_SOURCE_TARBALL_SHA256}"
version: "tmux ${TMUX_VERSION}"
binary_sha256: "${binary_sha}"
binary_path: "${prefix}/tmux"
EOF

printf 'tmux_oracle=%s\n' "$prefix/tmux"
printf 'tmux_version=%s\n' "$version"
printf 'source_sha=%s\n' "$TMUX_SOURCE_COMMIT"
printf 'source_tarball_sha256=%s\n' "$TMUX_SOURCE_TARBALL_SHA256"
printf 'binary_sha256=%s\n' "$binary_sha"
