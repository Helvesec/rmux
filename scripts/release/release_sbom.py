"""Verify existing SBOM v1 closures against locked Cargo resolution and outputs.

Unsupported targets/profiles fail closed; this module never invents a closure.
"""
from __future__ import annotations

import re
import argparse
import hashlib
import json
from io import BytesIO
import stat
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path, PurePosixPath

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "packaging"))
import sbom
import package_payloads
from release_evidence import file_hash, validate_file
from strict_json import read_json_object


def cargo_resolution(source: Path, target: str, cargo: Path, features=None):
    """Resolve a production feature context, with the source lockfile unchanged."""
    lock = validate_file(source / "Cargo.lock", "Cargo.lock")
    before = file_hash(lock)
    executable = str(cargo.resolve(strict=True))
    options = ["--locked", "--offline", "--manifest-path", str(source / "Cargo.toml")]
    if features is not None:
        options.append("--no-default-features")
        if features:
            options.extend(["--features", ",".join(sorted(features))])
    version = subprocess.run([executable, "-vV"], cwd=source, capture_output=True,
                             check=False, timeout=120)
    hosts = re.findall(rb"^host: (\S+)\r?$", version.stdout, re.MULTILINE)
    if version.returncode != 0 or len(hosts) != 1:
        raise ValueError("Cargo host platform is unavailable")
    result = subprocess.run(
        [executable, "metadata", "--format-version", "1", *options,
         "--filter-platform", target, "--filter-platform", hosts[0].decode("ascii")],
        cwd=source, capture_output=True, check=False, timeout=120,
    )
    if result.returncode != 0:
        raise ValueError("cargo metadata --locked failed; no complete resolution")
    metadata = sbom.loads(result.stdout)
    if not metadata.get("resolve") or not metadata.get("packages"):
        raise ValueError("Cargo metadata has no dependency resolution")
    # Unlike metadata's unified feature graph, tree resolves features with dev
    # targets disabled. Keep Cargo in charge of optional, host/build and target
    # activation; metadata supplies the locked identities and dependency aliases.
    result = subprocess.run([executable, "tree", *options, "--target", target,
        "--package", metadata["resolve"]["root"], "--edges", "normal,build",
        "--prefix", "depth", "--no-dedupe", "--color", "never", "--format", "{p}|{f}"],
        cwd=source, capture_output=True, check=False, timeout=120)
    if result.returncode != 0:
        raise ValueError("cargo tree --locked failed; no complete production resolution")
    metadata["resolve"] = production_graph(metadata, result.stdout.decode("utf-8"))
    if file_hash(lock) != before:
        raise ValueError("Cargo.lock changed during resolution")
    return metadata, sbom.toml_reader.loads(lock.read_text()), before


def production_graph(metadata, tree):
    """Join Cargo's non-dev tree to metadata, refusing ambiguous identities."""
    identities = {}
    for package in metadata["packages"]:
        identities.setdefault((package["name"], package["version"]), []).append(package["id"])
    resolved = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    nodes, stack = {}, []
    for line in tree.splitlines():
        match = re.fullmatch(r"(\d+)(\S+) v(\S+)(?: \([^|]*\))?\|([^\s|]*)", line)
        if not match:
            raise ValueError("Cargo production tree has an unsupported row")
        depth, name, version, features = match.groups()
        candidates = identities.get((name, version), [])
        if len(candidates) != 1 or int(depth) > len(stack):
            raise ValueError("Cargo production tree has an ambiguous package/depth")
        identity = candidates[0]
        node = nodes.setdefault(identity, dict(id=identity, features=set(), deps=[]))
        node["features"].update(filter(None, features.split(",")))
        if int(depth):
            parent = stack[int(depth) - 1]
            dependencies = [d for d in resolved[parent]["deps"] if d["pkg"] == identity
                and any(k["kind"] in (None, "build") for k in d["dep_kinds"])]
            if len(dependencies) != 1:
                raise ValueError("Cargo production tree has an ambiguous dependency alias")
            if dependencies[0] not in nodes[parent]["deps"]:
                nodes[parent]["deps"].append(dependencies[0])
        elif stack or identity != metadata["resolve"]["root"]:
            raise ValueError("Cargo production tree has a foreign root")
        stack[int(depth):] = [identity]
    if not nodes:
        raise ValueError("Cargo production tree is empty")
    for node in nodes.values():
        node["features"] = sorted(node["features"])
    return dict(root=metadata["resolve"]["root"], nodes=list(nodes.values()))


def validate_resolution(document, metadata, lock, lock_sha, source: Path, resolutions=None):
    contexts = document["resolution_contexts"]
    if not contexts or any(row["lock_sha256"] != lock_sha for row in contexts):
        raise ValueError("SBOM Cargo.lock binding differs")
    actual = {(p["name"], p["version"], p["source"]): p for p in metadata["packages"]}
    locked = {(p["name"], p["version"], p.get("source")): p for p in lock["package"]}
    packages = document["packages"]
    if not packages or not document["build_units"] or not document["binary_roots"]:
        raise ValueError("SBOM Cargo closure is incomplete")
    seen = set()
    for package in packages:
        origin = package["source"]
        key = (package["name"], package["version"],
               None if origin == "workspace:" + document["payload_source"]["commit"] else origin)
        if key in seen or key not in actual or key not in locked:
            raise ValueError("SBOM package is duplicated or absent from locked resolution")
        seen.add(key)
        resolved = actual[key]
        if (package["checksum"] != locked[key].get("checksum")
                or package["license"] != resolved["license"]):
            raise ValueError("SBOM package checksum/license differs from resolution")
        if key[2] is None:
            manifest = Path(resolved["manifest_path"])
            if not manifest.resolve(strict=True).is_relative_to(source.resolve(strict=True)):
                raise ValueError("SBOM local package is outside the candidate source")
            if file_hash(validate_file(manifest, "package manifest")) != package["manifest_sha256"]:
                raise ValueError("SBOM package manifest differs from source")

    validate_cargo_closures(document, metadata, resolutions)


def context_key(context):
    return context["target"], tuple(sorted(context["features"]))


def cargo_closure(metadata):
    """Follow Cargo's filtered, renamed normal/build edges from its real root."""
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    if root not in nodes or len(nodes) != len(metadata["resolve"]["nodes"]):
        raise ValueError("Cargo closure has no unique package root")
    pending, reached, edges = [root], set(), set()
    while pending:
        current = pending.pop()
        if current in reached:
            continue
        reached.add(current)
        for dependency in nodes[current]["deps"]:
            if any(k["kind"] in (None, "build") for k in dependency["dep_kinds"]):
                target = dependency["pkg"]
                if target not in nodes:
                    raise ValueError("Cargo closure has an unresolved dependency")
                edges.add((current, target, dependency["name"]))
                pending.append(target)
    return root, nodes, reached, edges


def validate_cargo_closures(document, metadata, resolutions):
    packages = {p["id"]: p for p in document["packages"]}
    units = {u["id"]: u for u in document["build_units"]}
    roots = {r["role"]: r for r in document["binary_roots"]}
    contexts = document["resolution_contexts"]
    if (len(units) != len(document["build_units"]) or len(packages) != len(document["packages"])
            or len(roots) != len(document["binary_roots"])
            or len({c["role"] for c in contexts}) != len(contexts)
            or set(roots) != {c["role"] for c in contexts}
            or any(u["package"] not in packages for u in units.values())):
        raise ValueError("SBOM Cargo closure has duplicate or foreign roots/units")
    adjacency = {identity: [] for identity in units}
    for relation in document["relationships"]:
        if relation["kind"] == "depends_on":
            if relation["source"] not in units or relation["target"] not in units:
                raise ValueError("SBOM Cargo closure has a dangling dependency")
            adjacency[relation["source"]].append((relation["target"], relation["name"]))
    all_units, all_packages = set(), set()
    for context in contexts:
        current = metadata if resolutions is None else resolutions[context_key(context)][0]
        root_id, nodes, required, expected_edges = cargo_closure(current)
        identities = {(p["name"], p["version"], p["source"]): p["id"] for p in current["packages"]}
        by_package = {}
        for identifier, package in packages.items():
            origin = package["source"]
            origin = None if origin == "workspace:" + document["payload_source"]["commit"] else origin
            by_package[identifier] = identities.get((package["name"], package["version"], origin))
        root = roots[context["role"]]
        if root["unit"] not in units:
            raise ValueError("SBOM Cargo closure has no binary unit")
        binary = units[root["unit"]]
        targets = {"bin-" + t["name"] for p in current["packages"] if p["id"] == root_id
                   for t in p["targets"] if "bin" in t["kind"]}
        if (by_package[binary["package"]] != root_id or binary["kind"] not in targets
                or binary["context"] != "target"
                or set(binary["features"]) != set(context["features"])
                or set(context["features"]) != set(nodes[root_id]["features"])):
            raise ValueError("SBOM Cargo closure binary root/features differ")
        pending, reached, observed_edges = [root["unit"]], set(), set()
        while pending:
            identity = pending.pop()
            if identity in reached:
                continue
            reached.add(identity)
            unit = units[identity]
            package_id = by_package[unit["package"]]
            if package_id not in required or not set(unit["features"]) <= set(nodes[package_id]["features"]):
                raise ValueError("SBOM Cargo closure contains a foreign unit/features")
            for target, name in adjacency[identity]:
                target_package = by_package[units[target]["package"]]
                if package_id != target_package:
                    observed_edges.add((package_id, target_package, name))
                pending.append(target)
        package_ids = {units[u]["package"] for u in reached}
        if ({by_package[p] for p in package_ids} != required or observed_edges != expected_edges
                or root["reachable_units"] != len(reached)):
            raise ValueError("SBOM Cargo closure packages/edges differ from resolve.nodes")
        all_units.update(reached)
        all_packages.update(package_ids)
    if all_units != set(units) or all_packages != set(packages):
        raise ValueError("SBOM Cargo closure contains unreachable units/packages")


def verify_sboms(records, assets: Path, documents, inputs, descriptors,
                 source: Path, source_sha: str, source_tree: str, cargo: Path):
    """One supported, reconstructed SBOM for every binary package output."""
    # Notices and checksums are release metadata, not binary package outputs.
    outputs = {r["name"]: r for r in records
               if r["role"] in {"archive", "debian", "rpm", "snap-amd64", "snap-arm64"}}
    if not outputs or any(set(mapping) != set(outputs)
                          for mapping in (documents, inputs, descriptors)):
        raise ValueError("SBOM required exactly once for every package output")
    resolutions = {}
    for name, record in sorted(outputs.items()):
        path = validate_file(documents[name], "SBOM")
        document = read_json_object(path, "SBOM")
        profile = document.get("profile")
        operations = {sbom.PROFILE: sbom.reconstruct, sbom.RPM_PROFILE: sbom.reconstruct_rpm,
                      sbom.DEB_PROFILE: sbom.reconstruct_deb}
        operations.update({p: sbom.reconstruct_repro for p in sbom.REPRO_PROFILES})
        if profile not in operations:
            raise ValueError("unsupported SBOM profile; no verified closure: " + name)
        if path.read_bytes() != sbom.canonical(document):
            raise ValueError("SBOM is not canonical JSON")
        expected_subject = dict(name=name, bytes=record["size"], sha256=record["sha256"])
        if document.get("subject") != expected_subject:
            raise ValueError("SBOM subject differs from release output: " + name)
        if document.get("payload_source") != dict(commit=source_sha, tree=source_tree):
            raise ValueError("SBOM source differs from candidate")
        input_path = validate_file(inputs[name], "SBOM inputs")
        descriptor = validate_file(descriptors[name], "SBOM descriptor")
        expected, schema = operations[profile](descriptor, input_path, file_hash(input_path), assets / name)
        sbom.validate_document(document, expected, schema)
        targets = {context["target"] for context in document["resolution_contexts"]}
        if len(targets) != 1:
            raise ValueError("SBOM must resolve exactly one output target")
        target = targets.pop()
        expected_target = {"linux-x86_64": "x86_64-unknown-linux-gnu",
                           "linux-aarch64": "aarch64-unknown-linux-gnu",
                           "macos-x86_64": "x86_64-apple-darwin",
                           "macos-aarch64": "aarch64-apple-darwin",
                           "windows-x86_64": "x86_64-pc-windows-msvc"}[record["platform_key"]]
        if target != expected_target:
            raise ValueError("SBOM target differs from output platform")
        selected = {}
        for context in document["resolution_contexts"]:
            key = context_key(context)
            if key not in resolutions:
                resolutions[key] = cargo_resolution(source, target, cargo, key[1])
            selected[key] = resolutions[key]
        metadata, lock, lock_sha = selected[context_key(document["resolution_contexts"][0])]
        metadata = dict(metadata, packages=list({p["id"]: p for result in selected.values()
                                                for p in result[0]["packages"]}.values()))
        validate_resolution(document, metadata, lock, lock_sha, source, selected)
    return len(outputs)


LOCAL_PROFILE = "local-archive-cargo-nondev-resolution-v1"
LOCAL_TARGETS = {"aarch64-apple-darwin", "x86_64-apple-darwin",
                 "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu",
                 "x86_64-pc-windows-msvc"}
LOCAL_ROLES = {"cli": ("binary", "rmux"), "helper": ("helper_binary", "rmux"),
               "daemon": ("daemon_binary", "rmux-daemon")}


def source_identity(source):
    def git(*args):
        return subprocess.run(["git", "-C", str(source), *args], check=True,
                              capture_output=True, text=True, timeout=30).stdout.strip()
    if git("status", "--porcelain", "--untracked-files=no"):
        raise ValueError("local SBOM source must be clean")
    return dict(commit=git("rev-parse", "HEAD"), tree=git("rev-parse", "HEAD^{tree}"))


def local_archive(subject, raw=None):
    """Inventory bounded regular entries without extracting or executing them."""
    raw = package_payloads.read_bounded(subject) if raw is None else raw
    files, captured, seen, expanded, root = {}, {}, set(), 0, None

    def consume(name, size, mode, directory, regular, open_member):
        nonlocal expanded, root
        parts = PurePosixPath(name).parts
        if (not parts or name.startswith("/") or "\\" in name or ".." in parts
                or str(PurePosixPath(name)) != name or name in seen or len(seen) >= 1024):
            raise ValueError("unsafe/duplicate local archive entry")
        seen.add(name)
        if root is None:
            root = parts[0]
        if parts[0] != root:
            raise ValueError("local archive must have one package root")
        relative = "/".join(parts[1:])
        if directory:
            return
        if not regular or not relative or not 0 <= size <= 512 * 1024 * 1024:
            raise ValueError("unsupported local archive entry type/size")
        expanded += size
        if expanded > 1024 * 1024 * 1024:
            raise ValueError("local archive expansion bound exceeded")
        capture = relative.endswith("artifact-metadata.json")
        if capture and size > 1024 * 1024:
            raise ValueError("local archive metadata bound exceeded")
        digest, content, total = hashlib.sha256(), bytearray(), 0
        with open_member() as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                total += len(block)
                if total > size:
                    raise ValueError("local archive content bound exceeded")
                digest.update(block)
                if capture:
                    content.extend(block)
        if total != size:
            raise ValueError("short local archive entry")
        files[relative] = dict(path=relative, bytes=total, sha256=digest.hexdigest(), mode=mode)
        if capture:
            captured[relative] = bytes(content)

    kind = package_payloads.package_kind(subject.name, raw)
    if kind:
        for entry in package_payloads.entries(raw, kind, max_members=1024):
            if entry.section == "metadata":
                continue
            relative = ("@control/" if entry.section == "control" else "") + entry.name
            consume("package/" + relative, len(entry.data), entry.mode, False, True,
                    lambda: BytesIO(entry.data))
    elif subject.name.endswith(".tar.gz"):
        with tarfile.open(fileobj=BytesIO(raw), mode="r|gz") as archive:
            for member in archive:
                consume(member.name, member.size, member.mode, member.isdir(), member.isfile(),
                        lambda: archive.extractfile(member))
    elif subject.suffix == ".zip":
        with zipfile.ZipFile(BytesIO(raw)) as archive:
            for member in archive.infolist():
                mode = member.external_attr >> 16
                consume(member.filename.rstrip("/"), member.file_size, None, member.is_dir(),
                        stat.S_IFMT(mode) in (0, stat.S_IFREG), lambda: archive.open(member))
    else:
        raise ValueError("unsupported local SBOM container")
    if len(captured) != 1:
        raise ValueError("local archive needs exactly one artifact metadata file")
    return files, sbom.loads(next(iter(captured.values())))


def local_build_context(source, path, binary, target):
    """Read successful Cargo binary builds, never a dev/test feature graph."""
    path = validate_file(path, "local Cargo build log")
    if path.stat().st_size > 32 * 1024 * 1024:
        raise ValueError("local Cargo build log exceeds bound")
    data = path.read_bytes()
    rows = [sbom.loads(line) for line in data.splitlines() if line.strip()]
    if not rows or rows[-1] != dict(reason="build-finished", success=True):
        raise ValueError("local Cargo build log has no successful completion")
    roots = [row for row in rows if row.get("reason") == "compiler-artifact"
             and row.get("target", {}).get("name") == binary
             and row["target"]["kind"] == ["bin"] and row.get("executable")]
    if len(roots) != 1:
        raise ValueError("local Cargo build log needs exactly one binary root")
    root = roots[0]
    executable = Path(root["executable"])
    if (Path(root["manifest_path"]).resolve() != (source / "Cargo.toml").resolve()
            or root["profile"]["test"] is not False or target not in executable.parts):
        raise ValueError("local Cargo build root differs from source/target or is a test")
    return sorted(root["features"]), dict(bytes=len(data), sha256=sbom.digest(data))


def local_document(source: Path, subject: Path, cargo: Path, build_logs):
    """Bind an unsigned local archive to its locked non-dev resolution.

    This profile describes package resolution, not the historical v1 compiled
    unit graph or authenticated provenance. Existing pinned profiles stay strict.
    """
    source, subject = source.resolve(strict=True), validate_file(subject, "local package output")
    identity = source_identity(source)
    raw = package_payloads.read_bounded(subject)
    subject_before = dict(name=subject.name, bytes=len(raw), sha256=sbom.digest(raw))
    files, metadata = local_archive(subject, raw)
    kind = package_payloads.package_kind(subject.name, raw)
    target = metadata["target"]
    if (metadata.get("git_commit") != identity["commit"] or metadata.get("git_dirty") is not False
            or metadata.get("payload_provenance") is not None or target not in LOCAL_TARGETS):
        raise ValueError("local package source/target differs from clean candidate")
    if set(build_logs) != set(LOCAL_ROLES):
        raise ValueError("local SBOM needs exactly cli/helper/daemon build logs")
    packages, contexts, roots, edges, log_refs, resolutions = {}, [], [], set(), {}, {}
    for role, (prefix, binary) in LOCAL_ROLES.items():
        path = metadata[prefix + "_path"]
        if kind:
            # System packages record install paths; archive profiles keep their
            # existing package-relative convention and closure checks.
            path = path.removeprefix("/")
        if path not in files or any(files[path][key] != metadata[prefix + "_" + key]
                                    for key in ("bytes", "sha256")):
            raise ValueError("local package binary differs from metadata")
        features, log_refs[role] = local_build_context(source, build_logs[role], binary, target)
        key = tuple(features)
        if key not in resolutions:
            resolutions[key] = cargo_resolution(source, target, cargo, features)
        current, lock, lock_sha = resolutions[key]
        root, nodes, required, relations = cargo_closure(current)
        if set(nodes[root]["features"]) != set(features):
            raise ValueError("local Cargo binary features differ from production resolution")
        locked = {(p["name"], p["version"], p.get("source")): p for p in lock["package"]}
        ids = {}
        for package in current["packages"]:
            if package["id"] not in required:
                continue
            origin = package["source"] or "workspace:" + identity["commit"]
            identifier = "package:" + sbom.digest(sbom.canonical([package["name"], package["version"], origin]))
            ids[package["id"]] = identifier
            manifest_sha = None
            if package["source"] is None:
                manifest = Path(package["manifest_path"]).resolve(strict=True)
                if not manifest.is_relative_to(source):
                    raise ValueError("local Cargo package is outside candidate source")
                manifest_sha = file_hash(validate_file(manifest, "local Cargo package manifest"))
            packages[identifier] = dict(id=identifier, name=package["name"], version=package["version"],
                source=origin, checksum=locked[(package["name"], package["version"], package["source"])].get("checksum"),
                license=package["license"], manifest_sha256=manifest_sha)
        contexts.append(dict(role=role, target=target, features=features, lock_sha256=lock_sha,
                             packages=sorted(ids.values()),
                             package_features={ids[p]: nodes[p]["features"] for p in sorted(required)}))
        roots.append(dict(role=role, path=path, package=ids[root], sha256=files[path]["sha256"]))
        edges.update((role, ids[a], ids[b], name) for a, b, name in relations)
    if source_identity(source) != identity or file_hash(subject) != subject_before["sha256"]:
        raise ValueError("local SBOM source or package changed during resolution")
    return dict(format="rmux-local-sbom", version=1, profile=LOCAL_PROFILE,
                component_scope="archive-files-and-locked-nondev-cargo-resolution",
                signature_status="unsigned", build_provenance="local-logs-not-attested",
                subject=subject_before, payload_source=identity,
                files=sorted(files.values(), key=lambda f: f["path"]),
                packages=sorted(packages.values(), key=lambda p: p["id"]),
                resolution_contexts=contexts, binary_roots=roots, build_logs=log_refs,
                relationships=[dict(role=r, source=a, target=b, name=n, kind="depends_on")
                               for r, a, b, n in sorted(edges)])


def verify_local_sboms(assets, documents, source, cargo, build_logs):
    if assets.is_symlink() or not assets.is_dir():
        raise ValueError("local package outputs must be a real directory")
    outputs = {p.name: p for p in assets.iterdir()
               if p.name.endswith((".tar.gz", ".zip", ".deb", ".rpm", ".snap"))}
    if not outputs or set(outputs) != set(documents):
        raise ValueError("SBOM required exactly once for every package output")
    for name, subject in sorted(outputs.items()):
        path = validate_file(documents[name], "local SBOM")
        data = path.read_bytes()
        document = sbom.loads(data)
        if data != sbom.canonical(document):
            raise ValueError("local SBOM is not canonical JSON")
        if data != sbom.canonical(local_document(source, subject, cargo, build_logs)):
            raise ValueError("local SBOM differs from reconstructed package/resolution: " + name)
    return len(outputs)


def local_paths(values):
    result = {}
    for value in values:
        name, separator, path = value.partition("=")
        if not separator or not path or not name or Path(name).name != name or name in result:
            raise ValueError("local SBOM mapping must be unique NAME=PATH")
        result[name] = Path(path)
    return result


def local_main(argv=None):
    parser = argparse.ArgumentParser(description="Generate or verify unsigned local package SBOMs")
    parser.add_argument("operation", choices=("generate-local", "verify-local"))
    parser.add_argument("--source-dir", required=True, type=Path)
    parser.add_argument("--cargo", required=True, type=Path)
    parser.add_argument("--build-log", required=True, action="append", metavar="ROLE=PATH")
    parser.add_argument("--subject", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--assets-dir", type=Path)
    parser.add_argument("--sbom", action="append", default=[], metavar="NAME=PATH")
    args = parser.parse_args(argv)
    try:
        logs = local_paths(args.build_log)
        if args.operation == "generate-local":
            if not args.subject or not args.output or args.assets_dir or args.sbom:
                raise ValueError("generate-local requires --subject and --output only")
            document = local_document(args.source_dir, args.subject, args.cargo, logs)
            sbom.write_exclusive(args.output, sbom.canonical(document))
            result = dict(status="GENERATED_LOCAL_SBOM", subject=document["subject"])
        else:
            if not args.assets_dir or not args.sbom or args.subject or args.output:
                raise ValueError("verify-local requires --assets-dir and --sbom only")
            count = verify_local_sboms(args.assets_dir, local_paths(args.sbom), args.source_dir, args.cargo, logs)
            result = dict(status="VERIFIED_LOCAL_SBOMS", sboms=count)
        print(json.dumps(dict(result, profile=LOCAL_PROFILE, signature_status="unsigned"), sort_keys=True))
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError,
            tarfile.TarError, zipfile.BadZipFile) as error:
        print("local-sbom: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(local_main())
