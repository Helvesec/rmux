#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <installation-directory>" >&2
  exit 2
fi

install_root=$1
version=2.93.0
system=$(uname -s)
machine=$(uname -m)

case "$system:$machine" in
  Linux:x86_64)
    archive="gh_${version}_linux_amd64.tar.gz"
    expected_sha256=02d1290eba130e0b896f3709ffff22e1c75a51475ddb70476a85abc6b5807af0
    binary="$install_root/gh"
    ;;
  MINGW*:x86_64 | MSYS*:x86_64 | CYGWIN*:x86_64)
    archive="gh_${version}_windows_amd64.zip"
    expected_sha256=77aa01ed7317295ad550de0ad04f3f276b1ef0e9272e3d002ac28dd99853d211
    binary="$install_root/gh.exe"
    ;;
  *)
    echo "the pinned verifier CLI installer does not support $system $machine" >&2
    exit 2
    ;;
esac
url="https://github.com/cli/cli/releases/download/v${version}/${archive}"

command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }
command -v sha256sum >/dev/null || { echo "sha256sum is required" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 \
  --output "$work/$archive" "$url"
printf '%s  %s\n' "$expected_sha256" "$work/$archive" | sha256sum --check --strict
mkdir -p "$install_root"
if [[ $system == Linux ]]; then
  tar -xzf "$work/$archive" -C "$work"
  install -m 0755 "$work/gh_${version}_linux_amd64/bin/gh" "$binary"
else
  command -v cygpath >/dev/null || { echo "cygpath is required" >&2; exit 2; }
  command -v powershell.exe >/dev/null || { echo "powershell.exe is required" >&2; exit 2; }
  RMUX_GH_ARCHIVE=$(cygpath -w "$work/$archive") \
    RMUX_GH_DEST=$(cygpath -w "$work") \
    powershell.exe -NoProfile -NonInteractive -Command \
      'Expand-Archive -LiteralPath $env:RMUX_GH_ARCHIVE -DestinationPath $env:RMUX_GH_DEST -Force'
  cp "$work/bin/gh.exe" "$binary"
  chmod 0755 "$binary"
fi

first_line=$("$binary" --version | head -n 1)
[[ $first_line == "gh version $version "* ]] || {
  echo "unexpected gh version: $first_line" >&2
  exit 1
}
"$binary" release verify --help >/dev/null
"$binary" release verify-asset --help >/dev/null
"$binary" attestation verify --help >/dev/null
printf 'gh-bin=%s\n' "$binary"
