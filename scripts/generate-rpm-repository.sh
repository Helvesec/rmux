#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-rpm-repository.sh --input-dir <dir> --output-dir <dir> [options]

Generate a static RPM/DNF repository for RMUX Fedora packages.

Options:
  --input-dir <dir>              Directory containing rmux-<version>-<release>.<arch>.rpm
  --output-dir <dir>             Repository output directory
  --baseurl <url>                Public repository base URL (default: https://packages.rmux.io/rpm)
  --repo-id <id>                 DNF repo id (default: rmux)
  --repo-name <name>             DNF repo name (default: RMUX)
  --gpg-key-url <url>            Public RPM GPG key URL (default: <baseurl>/RPM-GPG-KEY-rmux)
  --repo-gpg-key-url <url>       Public repodata GPG key URL
  --repo-signing-key <key-id>    GPG key id/fingerprint for repodata/repomd.xml.asc
  --previous-repository-dir <dir> Preserve the signed previous repodata generation
  --rpm-signing-key <key-id>     RPM signing key name/fingerprint for rpmsign --addsign
  --rpm-signing-version <version> Only sign packages with this RPM metadata version
  -h, --help                     Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

createrepo_cmd() {
  if command -v createrepo_c >/dev/null 2>&1; then
    printf 'createrepo_c\n'
  elif command -v createrepo >/dev/null 2>&1; then
    printf 'createrepo\n'
  else
    die "missing required command: createrepo_c or createrepo"
  fi
}

input_dir=""
output_dir=""
invocation_dir="$(pwd -P)"
baseurl="${RMUX_PACKAGES_RPM_BASE_URL:-https://packages.rmux.io/rpm}"
repo_id="${RMUX_RPM_REPO_ID:-rmux}"
repo_name="${RMUX_RPM_REPO_NAME:-RMUX}"
gpg_key_url=""
repo_gpg_key_url=""
repo_signing_key="${RMUX_RPM_REPO_GPG_KEY:-}"
previous_repository_dir=""
rpm_signing_key="${RMUX_RPM_GPG_KEY:-}"
rpm_signing_version=""

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
    --baseurl)
      [ "$#" -ge 2 ] || die "--baseurl requires a value"
      baseurl="$2"
      shift 2
      ;;
    --repo-id)
      [ "$#" -ge 2 ] || die "--repo-id requires a value"
      repo_id="$2"
      shift 2
      ;;
    --repo-name)
      [ "$#" -ge 2 ] || die "--repo-name requires a value"
      repo_name="$2"
      shift 2
      ;;
    --gpg-key-url)
      [ "$#" -ge 2 ] || die "--gpg-key-url requires a value"
      gpg_key_url="$2"
      shift 2
      ;;
    --repo-gpg-key-url)
      [ "$#" -ge 2 ] || die "--repo-gpg-key-url requires a value"
      repo_gpg_key_url="$2"
      shift 2
      ;;
    --repo-signing-key)
      [ "$#" -ge 2 ] || die "--repo-signing-key requires a value"
      repo_signing_key="$2"
      shift 2
      ;;
    --previous-repository-dir)
      [ "$#" -ge 2 ] || die "--previous-repository-dir requires a value"
      previous_repository_dir="$2"
      shift 2
      ;;
    --rpm-signing-key)
      [ "$#" -ge 2 ] || die "--rpm-signing-key requires a value"
      rpm_signing_key="$2"
      shift 2
      ;;
    --rpm-signing-version)
      [ "$#" -ge 2 ] || die "--rpm-signing-version requires a value"
      rpm_signing_version="$2"
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
[ -d "$input_dir" ] || die "input directory not found: $input_dir"
[ -n "$output_dir" ] || die "--output-dir is required"
case "$output_dir" in /|.|..) die "--output-dir is too broad: $output_dir" ;; esac
case "/$output_dir/" in */../*) die "--output-dir must not contain a parent component" ;; esac
[ ! -L "$output_dir" ] || die "--output-dir must not be a symbolic link"
case "$baseurl" in http://*|https://*) ;; *) die "--baseurl must be an http(s) URL" ;; esac
case "$repo_id" in *[!A-Za-z0-9_.:-]*|""|.*) die "invalid repo id: $repo_id" ;; esac
if [ -z "$gpg_key_url" ]; then
  gpg_key_url="${baseurl%/}/RPM-GPG-KEY-rmux"
fi
case "$gpg_key_url" in http://*|https://*) ;; *) die "--gpg-key-url must be an http(s) URL" ;; esac
if [ -n "$repo_signing_key" ] && [ -z "$repo_gpg_key_url" ]; then
  if [ -n "$rpm_signing_key" ] && [ "$repo_signing_key" = "$rpm_signing_key" ]; then
    repo_gpg_key_url="$gpg_key_url"
  else
    repo_gpg_key_url="${baseurl%/}/RPM-GPG-KEY-rmux-repository"
  fi
fi
if [ -n "$repo_gpg_key_url" ]; then
  case "$repo_gpg_key_url" in http://*|https://*) ;; *) die "--repo-gpg-key-url must be an http(s) URL" ;; esac
fi

if [ -n "$rpm_signing_key" ] && [ -n "$repo_signing_key" ]; then
  [ "$rpm_signing_key" != "$repo_signing_key" ] || \
    die "RPM package and repository signing keys must be distinct"
  need gpg
  rpm_fingerprint="$(gpg --batch --with-colons --fingerprint "$rpm_signing_key" | awk -F: '$1 == "fpr" { print $10; exit }')"
  repo_fingerprint="$(gpg --batch --with-colons --fingerprint "$repo_signing_key" | awk -F: '$1 == "fpr" { print $10; exit }')"
  [ -n "$rpm_fingerprint" ] || die "unable to resolve RPM package signing key fingerprint"
  [ -n "$repo_fingerprint" ] || die "unable to resolve RPM repository signing key fingerprint"
  [ "$rpm_fingerprint" != "$repo_fingerprint" ] || \
    die "RPM package and repository signing keys must be distinct"
fi
if [ -n "$rpm_signing_key" ]; then
  [ -n "$rpm_signing_version" ] || \
    die "--rpm-signing-version is required with --rpm-signing-key"
  case "$rpm_signing_version" in
    *[!0-9A-Za-z._+~-]*|"") die "invalid RPM signing version: $rpm_signing_version" ;;
  esac
fi

repo_tool="$(createrepo_cmd)"
input_dir="$(cd "$input_dir" && pwd -P)"
mkdir -p "$output_dir"
[ ! -L "$output_dir" ] || die "--output-dir must not be a symbolic link"
output_dir="$(cd "$output_dir" && pwd -P)"
[ "$output_dir" != / ] || die "--output-dir is too broad: $output_dir"
case "$invocation_dir/" in
  "$output_dir/"*) die "--output-dir must not contain the working directory" ;;
esac
case "$input_dir/" in
  "$output_dir/"*) die "--output-dir must not contain the input directory" ;;
esac
if [ -n "${HOME:-}" ]; then
  home_dir="$(cd "$HOME" && pwd -P)"
  [ "$output_dir" != "$home_dir" ] || die "--output-dir must not be HOME"
fi

retained_metadata_dir=""
history_work=""
cleanup() {
  if [ -n "$history_work" ]; then
    rm -rf "$history_work"
  fi
}
trap cleanup EXIT

if [ -n "$previous_repository_dir" ]; then
  [ -n "$repo_signing_key" ] || \
    die "--previous-repository-dir requires --repo-signing-key"
  [ -d "$previous_repository_dir" ] || \
    die "previous RPM repository not found: $previous_repository_dir"
  [ ! -L "$previous_repository_dir" ] || \
    die "previous RPM repository must not be a symbolic link"
  previous_repository_dir="$(cd "$previous_repository_dir" && pwd -P)"
  case "$previous_repository_dir/" in
    "$output_dir/"*) die "previous RPM repository must be outside --output-dir" ;;
  esac
  case "$output_dir/" in
    "$previous_repository_dir/"*) die "--output-dir must be outside the previous RPM repository" ;;
  esac

  previous_repodata="$previous_repository_dir/repodata"
  [ -d "$previous_repodata" ] && [ ! -L "$previous_repodata" ] || \
    die "previous RPM repodata directory is missing or unsafe"
  for signed_file in repomd.xml repomd.xml.asc; do
    [ -f "$previous_repodata/$signed_file" ] && [ ! -L "$previous_repodata/$signed_file" ] || \
      die "previous RPM repository lacks safe signed repodata"
    [ "$(wc -c < "$previous_repodata/$signed_file" | tr -d ' ')" -le 1048576 ] || \
      die "previous RPM signed repodata exceeds the bounded size limit"
  done

  need gpg
  need gpgv
  need python3
  history_work="$(mktemp -d "${TMPDIR:-/tmp}/rmux-rpm-metadata.XXXXXX")"
  retained_metadata_dir="$history_work/retained"
  mkdir "$retained_metadata_dir"
  verified_repomd="$history_work/previous-repomd.xml"
  verified_signature="$history_work/previous-repomd.xml.asc"
  cp "$previous_repodata/repomd.xml" "$verified_repomd"
  cp "$previous_repodata/repomd.xml.asc" "$verified_signature"
  for signed_file in "$verified_repomd" "$verified_signature"; do
    [ "$(wc -c < "$signed_file" | tr -d ' ')" -le 1048576 ] || \
      die "previous RPM signed repodata exceeds the bounded size limit"
  done
  keyring="$history_work/repository-signing-key.gpg"
  repo_fingerprints="$(
    gpg --batch --with-colons --fingerprint "$repo_signing_key" | \
      awk -F: '$1 == "pub" { primary = 1; next } $1 == "sub" { primary = 0 } $1 == "fpr" && primary { print $10; primary = 0 }'
  )"
  [ "$(printf '%s\n' "$repo_fingerprints" | sed '/^$/d' | wc -l | tr -d ' ')" -eq 1 ] || \
    die "RPM repository signing key must resolve to exactly one primary key"
  repo_fingerprint="$(printf '%s\n' "$repo_fingerprints" | sed '/^$/d')"
  [[ "$repo_fingerprint" =~ ^([0-9A-Fa-f]{40}|[0-9A-Fa-f]{64})$ ]] || \
    die "RPM repository signing key has a malformed fingerprint"
  gpg --batch --export "$repo_fingerprint" > "$keyring"
  [ -s "$keyring" ] || die "unable to export RPM repository signing key"
  gpgv --homedir "$history_work" --keyring "$keyring" \
    "$verified_signature" "$verified_repomd" >/dev/null

  python3 - "$previous_repository_dir" "$retained_metadata_dir" "$verified_repomd" <<'PY'
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import xml.etree.ElementTree as ET

MAX_FILES = 64
MAX_FILE_SIZE = 64 * 1024 * 1024
MAX_REPOMD_SIZE = 1024 * 1024
MAX_TOTAL_SIZE = 256 * 1024 * 1024
NAMESPACE = "{http://linux.duke.edu/metadata/repo}"
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*")


def fail(message: str) -> None:
    raise ValueError(message)


def one(node: ET.Element, name: str) -> ET.Element:
    matches = node.findall(NAMESPACE + name)
    if len(matches) != 1:
        fail(f"RPM repomd data must contain exactly one {name}")
    return matches[0]


def copy_verified(source: Path, destination: Path, digest: str, size: int) -> None:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    descriptor = os.open(source, flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            fail(f"RPM metadata is not a regular file: {source.name}")
        if metadata.st_size != size:
            fail(f"RPM metadata size differs from signed repomd: {source.name}")
        observed = hashlib.sha256()
        with os.fdopen(descriptor, "rb", closefd=False) as input_file, destination.open("xb") as output_file:
            for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
                observed.update(chunk)
                output_file.write(chunk)
        if observed.hexdigest() != digest:
            destination.unlink()
            fail(f"RPM metadata checksum differs from signed repomd: {source.name}")
    finally:
        os.close(descriptor)


def main() -> None:
    repository = Path(sys.argv[1])
    destination = Path(sys.argv[2])
    repomd = Path(sys.argv[3])
    if repomd.stat().st_size > MAX_REPOMD_SIZE:
        fail("RPM repomd exceeds the bounded size limit")
    xml_bytes = repomd.read_bytes()
    upper_xml = xml_bytes.upper()
    if b"<!DOCTYPE" in upper_xml or b"<!ENTITY" in upper_xml:
        fail("RPM repomd must not contain a document type or entity declaration")
    root = ET.fromstring(xml_bytes)
    if root.tag != NAMESPACE + "repomd":
        fail("RPM repomd root or namespace is invalid")
    data_nodes = root.findall(NAMESPACE + "data")
    if not data_nodes or len(data_nodes) > MAX_FILES:
        fail("RPM repomd contains an invalid metadata file count")

    seen: set[str] = set()
    total_size = 0
    for data in data_nodes:
        location = one(data, "location")
        if set(location.attrib) != {"href"}:
            fail("RPM repomd location attributes are invalid")
        relative = location.attrib["href"]
        path = PurePosixPath(relative)
        if (
            path.is_absolute()
            or path.parts[:1] != ("repodata",)
            or len(path.parts) != 2
            or not SAFE_NAME.fullmatch(path.name)
            or path.name in {"repomd.xml", "repomd.xml.asc"}
            or relative in seen
        ):
            fail(f"unsafe or duplicate RPM metadata path: {relative}")
        seen.add(relative)

        checksum = one(data, "checksum")
        if checksum.attrib != {"type": "sha256"}:
            fail(f"RPM metadata checksum type is not sha256: {relative}")
        digest = (checksum.text or "").strip().lower()
        if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            fail(f"RPM metadata checksum is malformed: {relative}")
        raw_size = (one(data, "size").text or "").strip()
        if re.fullmatch(r"0|[1-9][0-9]*", raw_size) is None:
            fail(f"RPM metadata size is malformed: {relative}")
        size = int(raw_size)
        total_size += size
        if size > MAX_FILE_SIZE or total_size > MAX_TOTAL_SIZE:
            fail("retained RPM metadata exceeds the bounded size limit")

        source = repository.joinpath(*path.parts)
        try:
            source.relative_to(repository)
        except ValueError:
            fail(f"RPM metadata escaped repository root: {relative}")
        if source.is_symlink():
            fail(f"RPM metadata must not be a symbolic link: {relative}")
        copy_verified(source, destination / path.name, digest, size)


try:
    main()
except (ET.ParseError, OSError, ValueError) as error:
    print(f"error: unable to retain previous RPM metadata: {error}", file=sys.stderr)
    raise SystemExit(1) from error
PY
fi

# The repository is regenerated from scratch, so --output-dir is emptied first.
# Ownership is proven by the directory's own contents, never by a marker file:
# every byte inside --output-dir is published verbatim to the public package
# repository, and nothing may be written outside it.
#
# Two rules keep a mistyped --output-dir from costing unrelated data. A non-empty
# directory must carry repodata/repomd.xml, which only a previous run of this
# generator writes, so a bare repodata/, a lone rmux.repo or a foreign rmux-*.rpm
# is refused rather than wiped. And RPM-GPG-KEY-* is deliberately NOT accepted:
# this generator never produces those files, both release call sites export them
# after it into a directory created fresh in the same job, and accepting them
# would make a mistyped --output-dir of /etc/pki/rpm-gpg delete the host's RPM
# signing keyring.
if [ -n "$(find "$output_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  [ -f "$output_dir/repodata/repomd.xml" ] && [ ! -L "$output_dir/repodata/repomd.xml" ] || \
    die "--output-dir is not empty and carries no RMUX-generated repodata/repomd.xml"
  output_entries=()
  while IFS= read -r -d '' output_entry; do
    case "${output_entry##*/}" in
      repodata|rmux.repo|rmux-*.rpm) ;;
      *) die "--output-dir is not an RMUX-managed RPM repository: ${output_entry##*/}" ;;
    esac
    output_entries+=("$output_entry")
  done < <(find "$output_dir" -mindepth 1 -maxdepth 1 -print0)
  # Nothing is removed until every entry has been accepted, and only the accepted
  # entries are removed.
  [ "${#output_entries[@]}" -eq 0 ] || rm -rf "${output_entries[@]}"
fi

found=0
for rpm in "$input_dir"/rmux-*.rpm; do
  [ -e "$rpm" ] || continue
  cp "$rpm" "$output_dir/"
  found=1
done
[ "$found" -eq 1 ] || die "no rmux-*.rpm files found in $input_dir"

if [ -n "$rpm_signing_key" ]; then
  need rpmsign
  need rpm
  signed_current=0
  for rpm in "$output_dir"/rmux-*.rpm; do
    rpm_identity="$(rpm -qp --queryformat '%{NAME}\t%{VERSION}' "$rpm")" || \
      die "unable to read RPM identity: ${rpm##*/}"
    IFS=$'\t' read -r rpm_name rpm_version rpm_extra <<< "$rpm_identity"
    [ "$rpm_name" = rmux ] || die "unexpected RPM package name: $rpm_name"
    [ -n "$rpm_version" ] && [ -z "${rpm_extra:-}" ] || \
      die "malformed RPM package identity: ${rpm##*/}"
    if [ "$rpm_version" = "$rpm_signing_version" ]; then
      rpmsign --define "_gpg_name $rpm_signing_key" --addsign "$rpm"
      signed_current=1
    fi
  done
  [ "$signed_current" -eq 1 ] || \
    die "no RPM metadata matched current version $rpm_signing_version"
fi

"$repo_tool" "$output_dir"

if [ -n "$retained_metadata_dir" ]; then
  for metadata in "$retained_metadata_dir"/*; do
    destination="$output_dir/repodata/${metadata##*/}"
    if [ -e "$destination" ]; then
      cmp -s "$metadata" "$destination" || \
        die "retained RPM metadata collides with the current generation"
    else
      cp "$metadata" "$destination"
    fi
  done
fi

rm -f "$output_dir/repodata/repomd.xml.asc"
if [ -n "$repo_signing_key" ]; then
  need gpg
  gpg --batch --yes --local-user "$repo_signing_key" --digest-algo SHA256 \
    --armor --detach-sign --output "$output_dir/repodata/repomd.xml.asc" "$output_dir/repodata/repomd.xml"
fi

gpgcheck=0
repo_gpgcheck=0
if [ -n "$rpm_signing_key" ]; then
  gpgcheck=1
fi

public_key_urls=""
if [ -n "$rpm_signing_key" ]; then
  public_key_urls="$gpg_key_url"
fi
if [ -n "$repo_gpg_key_url" ] && [ "$repo_gpg_key_url" != "$public_key_urls" ]; then
  public_key_urls="${public_key_urls:+$public_key_urls }$repo_gpg_key_url"
fi
[ -n "$public_key_urls" ] || public_key_urls="$gpg_key_url"
if [ -n "$repo_signing_key" ]; then
  repo_gpgcheck=1
fi

cat > "$output_dir/rmux.repo" <<EOF
[$repo_id]
name=$repo_name
baseurl=$baseurl
enabled=1
gpgcheck=$gpgcheck
repo_gpgcheck=$repo_gpgcheck
gpgkey=$public_key_urls
EOF

printf 'repository=%s\n' "$output_dir"
printf 'baseurl=%s\n' "$baseurl"
printf 'rpm_signed=%s\n' "$([ -n "$rpm_signing_key" ] && printf true || printf false)"
printf 'repo_signed=%s\n' "$([ -n "$repo_signing_key" ] && printf true || printf false)"
