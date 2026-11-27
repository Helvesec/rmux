"""Archive-only, offline Cargo source replacement for the installed witnesses.

This is a private packaging closure, not a registry publisher. Cargo produces
the RMUX archives; unmodified normalized manifests and archive bytes are then
installed as a directory source. Third-party archives must match Cargo.lock.
"""

import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tarfile

from toml_reader import loads

# crate_package_set imports its release siblings by bare name.
sys.path.insert(0, str(Path(__file__).resolve().parent / "release"))
from crate_package_set import file_hash  # noqa: E402

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def read_toml(path):
    return loads(Path(path).read_bytes().decode('utf-8'))


def identities(lock):
    return {(p["name"], p["version"], p.get("source"), p.get("checksum"))
            for p in read_toml(lock)["package"]}


def unpack(archive, destination, name, version):
    """Reject escaping members/links; preserve every regular archive file."""
    root = destination / f"{name}-{version}"
    root.mkdir(parents=True, exist_ok=False)
    files = {}
    with tarfile.open(archive, "r:gz") as package:
        for member in package.getmembers():
            path = Path(member.name)
            if (path.is_absolute() or ".." in path.parts or not path.parts
                    or path.parts[0] != root.name or not member.isfile()
                    or len(path.parts) < 2):
                raise ValueError(f"unsafe crate member: {member.name}")
            relative = Path(*path.parts[1:])
            if relative.as_posix() in files:
                raise ValueError(f"duplicate crate member: {member.name}")
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            with package.extractfile(member) as source, target.open("wb") as output:
                shutil.copyfileobj(source, output)
            target.chmod(member.mode & 0o777)
            files[relative.as_posix()] = file_hash(target)
    (root / ".cargo-checksum.json").write_text(json.dumps({
        "package": file_hash(archive), "files": files}, sort_keys=True))
    metadata = read_toml(root / "Cargo.toml")
    if (metadata["package"]["name"], metadata["package"]["version"]) != (name, version):
        raise ValueError("archive package identity differs")
    return root


def install_locked(runner, cache):
    packages = read_toml(runner.source / "Cargo.lock")["package"]
    entries = []
    archive_root = runner.output / "archives"
    archive_root.mkdir()
    runner.vendor.mkdir()
    for package in packages:
        if "source" not in package:
            continue
        if package["source"] != REGISTRY:
            raise ValueError(f"unsupported locked source: {package['name']}")
        filename = f"{package['name']}-{package['version']}.crate"
        source = cache / filename
        if source.is_symlink() or file_hash(source) != package["checksum"]:
            raise ValueError(f"locked archive mismatch: {filename}")
        target = archive_root / filename
        shutil.copyfile(source, target)
        if file_hash(target) != package["checksum"]:
            raise ValueError(f"copied archive mismatch: {filename}")
        unpack(target, runner.vendor, package["name"], package["version"])
        entries.append({**package, "archive": str(target), "copied_from": str(source)})
    runner.receipt["locked_archives"] = entries
    runner.save()


def closure(metadata):
    # Reuse release topology; include dev dependencies needed by Cargo packaging.
    path = Path(__file__).parent / "release/canonical-crate-set.py"
    spec = importlib.util.spec_from_file_location("canonical_crates", path)
    canonical = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(canonical)
    packages = {p["name"]: p for p in metadata["packages"]}
    names = {"rmux-sdk", "ratatui-rmux"}
    pending = list(names)
    while pending:
        package = packages[pending.pop()]
        for dependency in package["dependencies"]:
            name = dependency["name"]
            if dependency.get("path") and name not in names:
                names.add(name)
                pending.append(name)
    ordered, _ = canonical.dependency_order([packages[name] for name in names])
    return [packages[name] for name in ordered]


def check_lock(lock, original, archives, root_name=None):
    """Only local->registry RMUX identities and the external root may change."""
    allowed = identities(original)
    allowed |= {(p["name"], p["version"], REGISTRY, p["sha256"]) for p in archives}
    for identity in identities(lock):
        if root_name and identity == (root_name, "0.0.0", None, None):
            continue
        if identity not in allowed:
            raise ValueError(f"resolution changed a locked identity: {identity}")


def package_facades(runner, metadata):
    entries = []
    runner.receipt["packages"] = entries
    for package in closure(metadata):
        name, version = package["name"], package["version"]
        runner.run("package-" + name, [runner.cargo, "package", "-p", name,
                   "--offline", "--locked", "--no-verify", "--target-dir",
                   runner.output / "package-target"], runner.source)
        archive = runner.output / "archives" / f"{name}-{version}.crate"
        shutil.copyfile(runner.output / "package-target/package" / archive.name, archive)
        installed = unpack(archive, runner.vendor, name, version)
        vcs = json.loads((installed / ".cargo_vcs_info.json").read_text())
        relative = Path(package["manifest_path"]).parent.relative_to(runner.source).as_posix()
        if vcs != {"git": {"sha1": runner.receipt["source_sha"]}, "path_in_vcs": relative}:
            raise ValueError(f"packaged VCS identity differs: {vcs}")
        if (installed / "Cargo.toml.orig").read_bytes() != Path(package["manifest_path"]).read_bytes():
            raise ValueError(f"original package manifest changed: {name}")
        entry = {"name": name, "version": version, "sha256": file_hash(archive),
                 "archive": str(archive), "source": relative, "vcs": vcs,
                 "manifest_sha256": file_hash(installed / "Cargo.toml"),
                 "original_manifest_sha256": file_hash(installed / "Cargo.toml.orig")}
        entries.append(entry)
        check_lock(installed / "Cargo.lock", runner.source / "Cargo.lock", entries)
        runner.save()
    return entries


def verify_install(vendor):
    for package in vendor.iterdir():
        expected = json.loads((package / ".cargo-checksum.json").read_text())["files"]
        actual = {p.relative_to(package).as_posix(): file_hash(p)
                  for p in package.rglob("*") if p.is_file() and p.name != ".cargo-checksum.json"}
        if actual != expected:
            raise ValueError(f"installed archive bytes changed: {package.name}")
