#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <installation-directory>" >&2
  exit 2
fi

install_root=$1
version=2.93.0
archive="gh_${version}_linux_amd64.tar.gz"
expected_sha256=02d1290eba130e0b896f3709ffff22e1c75a51475ddb70476a85abc6b5807af0
url="https://github.com/cli/cli/releases/download/v${version}/${archive}"

[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || {
  echo "the pinned verifier CLI installer supports only Linux x86_64" >&2
  exit 2
}
command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }
command -v sha256sum >/dev/null || { echo "sha256sum is required" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 \
  --output "$work/$archive" "$url"
printf '%s  %s\n' "$expected_sha256" "$work/$archive" | sha256sum --check --strict
tar -xzf "$work/$archive" -C "$work"
mkdir -p "$install_root"
install -m 0755 "$work/gh_${version}_linux_amd64/bin/gh" "$install_root/gh"

first_line=$("$install_root/gh" --version | head -n 1)
[[ $first_line == "gh version $version "* ]] || {
  echo "unexpected gh version: $first_line" >&2
  exit 1
}
"$install_root/gh" release verify --help >/dev/null
"$install_root/gh" release verify-asset --help >/dev/null
"$install_root/gh" attestation verify --help >/dev/null
printf 'gh-bin=%s\n' "$install_root/gh"
