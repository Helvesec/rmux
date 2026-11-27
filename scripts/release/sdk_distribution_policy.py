"""RMUX 1.0 additional-SDK exclusion at canonical artifact acceptance.

Native archives use the layouts emitted by package-unix.sh/package-windows.ps1.
Crate sources retain Rust code, tests and the existing embedded Web assets.
This checks package contents, not executable behavior or historical SDK health.
"""
from __future__ import annotations

import json
import re
import sys
import tarfile
import zipfile
import xml.etree.ElementTree as ET
from pathlib import Path, PurePosixPath
from typing import Any

from strict_json import _unique_object

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from toml_reader import loads as read_toml

METADATA_PATH = "share/rmux/artifact-metadata.json"
COMPLETIONS = {
    "share/bash-completion/completions/rmux",
    "share/zsh/site-functions/_rmux",
    "share/fish/vendor_completions.d/rmux.fish",
    "share/powershell/Completions/_rmux.ps1",
    "share/elvish/lib/rmux.elv",
}
COMMON_FILES = COMPLETIONS | {METADATA_PATH, "SHA256SUMS.txt", "LICENSE-APACHE", "LICENSE-MIT"}
NATIVE_FILES = {
    "tar.gz": COMMON_FILES | {"bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux",
                               "install.sh", "share/man/man1/rmux.1"},
    "zip": COMMON_FILES | {"rmux.exe", "rmux-daemon.exe", "libexec/rmux/rmux.exe",
                            "install.ps1", "README.md", "rmux.1", "conpty.dll", "OpenConsole.exe",
                            "libexec/rmux/conpty.dll", "libexec/rmux/OpenConsole.exe",
                            "share/rmux/conpty-runtime.json",
                            "share/licenses/Microsoft.Windows.Console.ConPTY/LICENSE"},
}
METADATA_FIELDS = set("""schema artifact_kind binary_path binary_sha256 binary_bytes
helper_binary_path helper_binary_sha256 helper_binary_bytes daemon_binary_path
daemon_binary_sha256 daemon_binary_bytes rmux_version git_commit git_dirty target
platform_label configuration package_schema package_name package_target
package_target_label package_layout archive_format skip_build release_artifact
generated_at_utc""".split())
LINUX_FIELDS = set("""binary_glibc_min helper_binary_glibc_min daemon_binary_glibc_min
package_glibc_min max_supported_glibc""".split())
MAX_METADATA_BYTES = 1024 * 1024
MAX_MEMBERS = 20000


def applies(version: str) -> bool:
    return re.fullmatch(r"1\.0\.[0-9]+(?:-rc\.[0-9]+)?", version) is not None


def json_object(data: bytes, label: str) -> dict[str, Any]:
    if len(data) > MAX_METADATA_BYTES:
        raise ValueError(f"{label} exceeds the metadata limit")
    value = json.loads(data.decode("utf-8-sig"), object_pairs_hook=_unique_object)
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object")
    return value


def portable_name(name: str) -> str:
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or "\\" in name or ":" in name
            or any(part in {"", ".", ".."} for part in name.split("/"))):
        raise ValueError(f"noncanonical package member: {name}")
    return name


def native_archive(path: Path, platform: dict[str, Any], version: str) -> None:
    """Inspect real archive members before the canonical record is accepted."""
    fmt = platform["archive_format"]
    expected_name = f"rmux-{version}-{platform['key']}.{fmt}"
    if path.name != expected_name:
        raise ValueError(f"1.0 native archive must be {expected_name}")
    prefix = expected_name[:-len(fmt)-1]
    expected = {f"{prefix}/{name}" for name in NATIVE_FILES[fmt]}
    directories = {prefix}
    for name in expected:
        directories.update(str(parent) for parent in PurePosixPath(name).parents if str(parent) != ".")
    seen: set[str] = set()
    metadata = None

    def member(name: str, is_dir: bool) -> None:
        name = portable_name(name.rstrip("/") if is_dir else name)
        if name in seen or name not in (directories if is_dir else expected):
            raise ValueError(f"1.0 package member outside native layout: {name}")
        seen.add(name)

    metadata_name = f"{prefix}/{METADATA_PATH}"
    if fmt == "zip":
        with zipfile.ZipFile(path) as archive:
            entries = archive.infolist()
            if len(entries) > MAX_MEMBERS:
                raise ValueError("native archive member limit exceeded")
            for entry in entries:
                member(entry.filename, entry.is_dir())
                if (entry.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError("native archive contains a link")
                if entry.filename == metadata_name:
                    if entry.file_size > MAX_METADATA_BYTES:
                        raise ValueError("native metadata exceeds the limit")
                    metadata = json_object(archive.read(entry), "native metadata")
    else:
        with tarfile.open(path, "r:gz") as archive:
            for index, entry in enumerate(archive):
                if index >= MAX_MEMBERS or not (entry.isdir() or entry.isfile()):
                    raise ValueError("native archive has unsupported members")
                member(entry.name, entry.isdir())
                if entry.name == metadata_name:
                    if entry.size > MAX_METADATA_BYTES:
                        raise ValueError("native metadata exceeds the limit")
                    stream = archive.extractfile(entry)
                    assert stream is not None
                    metadata = json_object(stream.read(), "native metadata")
    if seen & expected != expected:
        raise ValueError("1.0 native package file set is incomplete")
    fields = METADATA_FIELDS | (
        {"reuse_release_binaries", "conpty_runtime_manifest"} if fmt == "zip" else {"archive_reproducibility"}
    )
    if platform["key"].startswith("linux-"):
        fields |= LINUX_FIELDS
    if metadata is None or set(metadata) != fields:
        raise ValueError("1.0 native metadata fields differ; additional SDK support is excluded")
    if metadata["rmux_version"] != version:
        raise ValueError("native metadata version differs from release intent")


def chocolatey_archive(path: Path) -> None:
    # generate-chocolatey-package.sh emits two tools and the rmux nuspec;
    # choco pack adds only the NuGet envelope metadata.
    expected = {"rmux.nuspec", "tools/chocolateyInstall.ps1", "tools/chocolateyUninstall.ps1",
                "_rels/.rels", "[Content_Types].xml"}
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        envelope = {name for name in names if re.fullmatch(
            r"package/services/metadata/core-properties/[0-9a-f]+\.psmdcp", name)}
        if len(envelope) != 1 or len(names) != len(set(names)) or set(names) != expected | envelope:
            raise ValueError("1.0 Chocolatey package file set differs")
        info = archive.getinfo("rmux.nuspec")
        if info.file_size > MAX_METADATA_BYTES:
            raise ValueError("Chocolatey metadata exceeds the limit")
        root = ET.fromstring(archive.read(info))
        metadata = root.find("{*}metadata")
        allowed = {"id", "version", "title", "authors", "owners", "projectUrl", "iconUrl",
                   "requireLicenseAcceptance", "description", "summary", "releaseNotes",
                   "copyright", "tags", "licenseUrl", "projectSourceUrl", "bugTrackerUrl",
                   "docsUrl", "mailingListUrl", "packageSourceUrl", "dependencies"}
        if metadata is None:
            raise ValueError("Chocolatey metadata is missing")
        fields = [child.tag.rsplit("}", 1)[-1] for child in metadata]
        if (len(fields) != len(set(fields)) or not set(fields) <= allowed
                or metadata.findtext("{*}id") != "rmux"):
            raise ValueError("1.0 Chocolatey metadata differs; additional SDK support is excluded")
        dependencies = metadata.find("{*}dependencies")
        if dependencies is not None:
            ids = [node.get("id") for node in dependencies.iter() if node.tag.rsplit("}", 1)[-1] == "dependency"]
            if ids != ["vcredist140"]:
                raise ValueError("1.0 Chocolatey dependencies differ; additional SDK support is excluded")


def additional_payload(relative: str) -> bool:
    parts = PurePosixPath(relative).parts
    return ("librmux" in parts or "node_modules" in parts
            or relative.endswith((".py", ".pyi", ".pyc", ".whl", ".js", ".mjs", ".cjs", ".ts", ".tgz", ".tar.gz", ".zip"))
            or parts[-1] in {"package.json", "pyproject.toml", "setup.cfg"})


def snap_package_tree(root: Path, version: str) -> None:
    # Snapcraft also stages native loader/library files. Keep its native layout
    # authority; reject additional-language packages in the installed payload.
    if not applies(version):
        return
    for entry in root.rglob("*"):
        if entry.is_file() and additional_payload(entry.relative_to(root).as_posix()):
            raise ValueError("additional SDK payload is excluded from the RMUX 1.0 Snap")


def wasm_package_metadata(data: bytes, version: str) -> None:
    if not applies(version):
        return
    value = json_object(data, "WASM package metadata")
    # wasm-pack's browser module has its own package identity. It is not @rmux/sdk.
    allowed = {"name", "version", "description", "license", "repository", "files",
               "module", "types", "type", "sideEffects", "homepage", "keywords", "author"}
    if value.get("name") != "rmux-web-crypto" or not set(value) <= allowed:
        raise ValueError(
            "1.0 WASM package identity/fields differ; additional SDK support is excluded"
        )


def crate_build_metadata_allowed(name: str, metadata: Any) -> bool:
    # The publishable Web crate disables wasm-opt for the canonical wasm-pack
    # build (build-web-crypto-wasm.sh). Approve that exact configuration only;
    # sibling tables/keys are still unapproved support or build surfaces.
    if metadata == {}:
        return True
    return (name == "rmux-web-crypto"
            and metadata == {"wasm-pack": {"profile": {"release": {"wasm-opt": False}}}}
            and metadata["wasm-pack"]["profile"]["release"]["wasm-opt"] is False)


def crate_archive(path: Path, name: str, version: str) -> None:
    """Do not ship additional-language implementations inside a Rust crate."""
    if not applies(version):
        return
    # Cargo's actual package name remains authoritative; Python/npm packages
    # cannot acquire an accepted crate identity by changing the extension.
    if name in {"librmux", "rmux-python", "rmux-typescript", "@rmux/sdk"}:
        raise ValueError("additional SDK distribution is excluded from RMUX 1.0")
    inventory = json_object(
        (Path(__file__).resolve().parents[1] / "source-inventory/declarations.json").read_bytes(),
        "source inventory",
    )["files"]
    declared_tests = set(inventory["tests"]) | set(inventory["specifications"])
    with tarfile.open(path, "r:gz") as archive:
        seen: set[str] = set()
        for index, member in enumerate(archive):
            if index >= MAX_MEMBERS or not member.isfile():
                raise ValueError("crate contains unsupported members")
            portable_name(member.name)
            prefix = f"{name}-{version}/"
            if not member.name.startswith(prefix) or member.name in seen:
                raise ValueError("crate member identity differs")
            seen.add(member.name)
            relative = member.name[len(prefix):]
            parts = PurePosixPath(relative).parts
            if relative == "Cargo.toml":
                if member.size > MAX_METADATA_BYTES:
                    raise ValueError("crate manifest exceeds the metadata limit")
                source = archive.extractfile(member)
                assert source is not None
                package = read_toml(source.read().decode("utf-8")).get("package", {})
                if (package.get("name") != name or package.get("version") != version
                        or not crate_build_metadata_allowed(name, package.get("metadata", {}))):
                    raise ValueError(
                        "1.0 crate identity/metadata differs; additional SDK support is excluded"
                    )
            # Reuse declared test/specification paths, not an unbounded tests/
            # exception or a second source inventory. Docs can name both SDKs.
            if "librmux" in parts or "node_modules" in parts:
                raise ValueError("additional SDK implementation is excluded from RMUX 1.0")
            source_path = relative if name == "rmux" else f"crates/{name}/{relative}"
            if relative.startswith("tests/") and source_path in declared_tests:
                continue
            if additional_payload(relative):
                raise ValueError(f"additional-language payload outside Rust package surface: {relative}")
        if not seen:
            raise ValueError("crate archive is empty")


def linux_package_tree(root: Path, layout: str, version: str) -> None:
    """Check the payload extracted by the existing Debian/RPM verifiers."""
    if not applies(version.replace("~rc.", "-rc.")):
        return
    expected = {f"usr/{name}" for name in COMPLETIONS | {
        "bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux", METADATA_PATH,
        "share/doc/rmux/README.md", "share/doc/rmux/LICENSE-APACHE",
        "share/doc/rmux/LICENSE-MIT",
    }}
    if layout == "deb":
        # package-debian.sh always emits the Debian copyright-format file.
        expected.add("usr/share/doc/rmux/copyright")
    man = "usr/share/man/man1/rmux.1"
    expected.add(man + ".gz" if layout == "deb" or (root / (man + ".gz")).is_file() else man)
    actual: set[str] = set()
    for entry in root.rglob("*"):
        relative = entry.relative_to(root).as_posix()
        # RPM 4.18.2 appends .1, .2, ... to duplicate build IDs. Only canonical
        # positive decimal suffixes are allowed, still targeting declared binaries.
        if entry.is_symlink():
            if (layout == "rpm" and re.fullmatch(
                    r"usr/lib/\.build-id/[0-9a-f]{2}/[0-9a-f]+(?:\.[1-9][0-9]*)?", relative)
                    and entry.resolve() in {root.resolve() / n for n in (
                        "usr/bin/rmux", "usr/bin/rmux-daemon", "usr/libexec/rmux/rmux")}):
                continue
            raise ValueError("Linux package contains an unexpected link")
        if entry.is_dir():
            continue
        if not entry.is_file():
            raise ValueError("Linux package contains a non-file payload")
        actual.add(relative)
    if actual != expected:
        raise ValueError(
            f"1.0 Linux package file set differs: extra={sorted(actual - expected)}, "
            f"missing={sorted(expected - actual)}"
        )
    metadata = json_object((root / "usr" / METADATA_PATH).read_bytes(), "Linux package metadata")
    if set(metadata) != METADATA_FIELDS | LINUX_FIELDS | {"reuse_release_binary"}:
        raise ValueError("1.0 Linux metadata fields differ; additional SDK support is excluded")
    if metadata["rmux_version"] != version.replace("~rc.", "-rc."):
        raise ValueError("Linux metadata version differs from package version")


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--layout", required=True, choices=("deb", "rpm", "snap"))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    if args.layout == "snap":
        snap_package_tree(args.root, args.version)
    else:
        linux_package_tree(args.root, args.layout, args.version)
