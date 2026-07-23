#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/check-release-versions.sh [--binary PATH]

Verify that release-facing versions agree:
  - workspace.package.version in Cargo.toml
  - every RMUX workspace crate version from cargo metadata
  - every internal workspace path-dependency requirement
  - RMUX package versions in scripts/fuzz/Cargo.lock
  - snap/snapcraft.yaml, when present
  - rmux.1 release-facing version
  - Windows application manifest assembly version
  - SECURITY.md signed-checksum verification regex
  - optional rmux -V output
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
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

snap_version() {
  awk '
    $1 == "version:" {
      gsub(/"/, "", $2)
      print $2
      exit
    }
  ' snap/snapcraft.yaml
}

root_package_publish() {
  awk '
    /^\[package\]$/ { in_package = 1; next }
    /^\[/ { in_package = 0 }
    in_package && $1 == "publish" {
      print $3
      exit
    }
  ' Cargo.toml
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary)
      [ "$#" -ge 2 ] || die "--binary requires a path"
      binary="$2"
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

cd "$repo_root"

version="$(workspace_version)"
[ -n "$version" ] || die "unable to read workspace.package.version"
root_publish="$(root_package_publish)"
[ "$root_publish" = "true" ] ||
  die "root rmux package must keep publish=true for cargo install rmux"
printf 'root-publish=true\n'

metadata_dir="$(mktemp -d "${TMPDIR:-/tmp}/rmux-metadata.XXXXXX")"
metadata="$metadata_dir/metadata.json"
trap 'rm -rf "$metadata_dir"' EXIT
cargo metadata --locked --no-deps --format-version 1 >"$metadata"

python3 - "$metadata" "$version" <<'PY'
import json
import sys

metadata_path, expected = sys.argv[1], sys.argv[2]
metadata = json.load(open(metadata_path, encoding="utf-8"))
checked = []
bad = []
workspace_names = {package["name"] for package in metadata["packages"]}
dependency_bad = []
for package in metadata["packages"]:
    name = package["name"]
    if name.startswith("rmux") or name in {"ratatui-rmux", "xtask"}:
        checked.append(name)
        if package["version"] != expected:
            bad.append(f"{name}={package['version']}")
    for dependency in package["dependencies"]:
        if dependency.get("path") is None or dependency["name"] not in workspace_names:
            continue
        expected_requirement = f"^{expected}"
        if dependency["req"] != expected_requirement:
            dependency_bad.append(
                f"{name}->{dependency['name']}={dependency['req']}"
            )
if bad:
    print("version mismatch: " + ", ".join(sorted(bad)), file=sys.stderr)
    sys.exit(1)
if dependency_bad:
    print(
        "workspace path dependency version mismatch: "
        + ", ".join(sorted(set(dependency_bad))),
        file=sys.stderr,
    )
    sys.exit(1)
if not checked:
    print("no RMUX workspace packages found in metadata", file=sys.stderr)
    sys.exit(1)
for name in sorted(checked):
    print(f"{name} {expected}")
print(f"workspace-path-dependencies ^{expected}")
PY

[ -f scripts/fuzz/Cargo.lock ] || die "scripts/fuzz/Cargo.lock is missing"
python3 - "$version" <<'PY'
import sys
import tomllib
from pathlib import Path

expected = sys.argv[1]
path = Path("scripts/fuzz/Cargo.lock")
with path.open("rb") as handle:
    lockfile = tomllib.load(handle)

checked = []
bad = []
for package in lockfile.get("package", []):
    name = package.get("name", "")
    if package.get("source") is not None or not name.startswith("rmux") or name == "rmux-fuzz":
        continue
    version = package.get("version")
    checked.append(name)
    if version != expected:
        bad.append(f"{name}={version}")
if bad:
    print("fuzz lock version mismatch: " + ", ".join(sorted(bad)), file=sys.stderr)
    sys.exit(1)
if not checked:
    print("scripts/fuzz/Cargo.lock has no RMUX packages", file=sys.stderr)
    sys.exit(1)
print(f"fuzz-lock {expected}")
PY

if [ -f snap/snapcraft.yaml ]; then
  snap="$(snap_version)"
  [ "$snap" = "$version" ] || die "snap version $snap does not match workspace $version"
  printf 'snap %s\n' "$snap"
fi

python3 - "$version" <<'PY'
import re
import sys
from pathlib import Path

version = sys.argv[1]

manpage = Path("docs/man/rmux.1").read_text(encoding="utf-8")
th = next((line for line in manpage.splitlines() if line.startswith(".TH RMUX 1 ")), None)
if th is None:
    print("rmux.1 is missing .TH RMUX 1 header", file=sys.stderr)
    sys.exit(1)
if f'"RMUX {version}"' not in th:
    print(f"rmux.1 .TH version does not match workspace {version}: {th}", file=sys.stderr)
    sys.exit(1)
print(f"manpage RMUX {version}")

security = Path("SECURITY.md").read_text(encoding="utf-8")
match = re.search(r"--certificate-identity-regexp '([^']+)'", security)
if match is None:
    print("SECURITY.md is missing cosign certificate identity regexp", file=sys.stderr)
    sys.exit(1)
identity_re = match.group(1)
identity = (
    "https://github.com/Helvesec/rmux/.github/workflows/"
    f"release.yml@refs/tags/v{version}"
)
if re.fullmatch(identity_re, identity) is None:
    print(
        "SECURITY.md cosign identity regexp does not match "
        f"release tag v{version}: {identity_re}",
        file=sys.stderr,
    )
    sys.exit(1)
print(f"security-regex v{version}")

manifest_path = Path("resources/windows/rmux.exe.manifest")
if manifest_path.exists():
    manifest = manifest_path.read_text(encoding="utf-8")
    expected_manifest_version = f'{version}.0'
    match = re.search(r'<assemblyIdentity\b[^>]*\bversion="([^"]+)"', manifest, re.S)
    if match is None:
        print("Windows manifest is missing assemblyIdentity version", file=sys.stderr)
        sys.exit(1)
    if match.group(1) != expected_manifest_version:
        print(
            "Windows manifest version does not match workspace "
            f"{version}: {match.group(1)}",
            file=sys.stderr,
        )
        sys.exit(1)
    print(f"windows-manifest {expected_manifest_version}")
PY

if [ -n "$binary" ]; then
  [ -x "$binary" ] || die "binary is not executable: $binary"
  output="$("$binary" -V)"
  [ "$output" = "rmux $version" ] || die "unexpected version output: $output"
  printf 'binary %s\n' "$output"
fi

printf 'release-version-check=ok\n'
