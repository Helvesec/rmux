#!/usr/bin/env python3
"""Pinned, offline SBOM for existing Linux packages and recorded Cargo builds.

No Cargo resolution, build, payload execution or archive extraction is performed.
The internal format describes compiled units, not post-link symbol reachability.
"""
import argparse
import copy
import ctypes
import stat
import struct
import hashlib
import json
import re
import sys
import tarfile
from collections import defaultdict
from pathlib import Path, PurePosixPath
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import toml_reader
import layout

PROFILE = "linux-archive-cargo-build-closure-v1"
RPM_PROFILE = "linux-rpm-cargo-build-closure-v1"
DEB_PROFILE = "linux-deb-cargo-build-closure-v1"
REPRO_PROFILES = {"linux-" + kind + "-repro-cargo-build-closure-v1": (kind, variant, parent)
                  for kind, variant, parent in (("tar", "unix-archive", PROFILE),
                                                ("deb", "debian", DEB_PROFILE),
                                                ("rpm", "rpm", RPM_PROFILE))}
FULL_RPM_PROFILE = "linux-rpm-full-repro-cargo-build-closure-v1"
REPRO_PROFILES[FULL_RPM_PROFILE] = ("rpm-full", "rpm", RPM_PROFILE)
FULL_DEB_PROFILE = "linux-deb-full-repro-cargo-build-closure-v1"
REPRO_PROFILES[FULL_DEB_PROFILE] = ("deb-full", "debian", DEB_PROFILE)
FULL_TAR_PROFILE = "linux-tar-full-repro-cargo-build-closure-v1"
REPRO_PROFILES[FULL_TAR_PROFILE] = ("tar-full", "unix-archive", PROFILE)
ROOT = Path(__file__).resolve().parents[2]
REQUIRED_INPUTS = {
    "metadata", "units", "auxiliary", "archive_proof", "payload_result", "lock",
    "root_manifest", "environment", "tools", "tool_versions", "metadata_receipt",
    "build_full", "build_tiny", "build_daemon", "completions", "xtask_graph", "xtask_build",
}


class Error(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Error(message)


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def loads(data):
    return json.loads(data, object_pairs_hook=unique_pairs,
                      parse_constant=lambda value: (_ for _ in ()).throw(Error("nonfinite JSON")))


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"),
                       ensure_ascii=False, allow_nan=False) + "\n").encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_hash(path):
    path = Path(path)
    return layout.file_digest(path.parent, path.name)


def checked_file(reference, path=None):
    path = Path(path or reference["path"])
    require(path.is_file() and not path.is_symlink(), "not a regular pinned file: " + str(path))
    data = path.read_bytes()
    require(len(data) == reference["bytes"] and digest(data) == reference["sha256"],
            "input pin mismatch: " + str(path))
    return data


def shape(value, rule, definitions, where="SBOM"):
    if "ref" in rule:
        return shape(value, definitions[rule["ref"]], definitions, where)
    kind = rule["type"]
    if kind == "nullable" and value is None:
        return
    if kind == "nullable":
        return shape(value, rule["item"], definitions, where)
    types = {"object": dict, "array": list, "string": str, "integer": int, "boolean": bool}
    require(type(value) is types[kind], "schema type: " + where)
    if kind == "object":
        require(set(value) == set(rule["properties"]), "schema fields: " + where)
        for key, child in rule["properties"].items():
            shape(value[key], child, definitions, where + "." + key)
    elif kind == "array":
        for index, item in enumerate(value):
            shape(item, rule["item"], definitions, where + "[" + str(index) + "]")
    elif kind == "string" and "pattern" in rule:
        require(re.fullmatch(rule["pattern"], value) is not None, "schema pattern: " + where)
    elif kind == "integer":
        require(value >= rule.get("minimum", 0), "schema integer: " + where)
    if "enum" in rule:
        require(value in rule["enum"], "schema enum: " + where)


def tool_policy(descriptor_path, profile=PROFILE):
    descriptor = layout.load(descriptor_path)
    variant = (REPRO_PROFILES[profile][1] if profile in REPRO_PROFILES else
               {PROFILE: "unix-archive", RPM_PROFILE: "rpm", DEB_PROFILE: "debian"}[profile])
    layout.validate_variant(descriptor, variant)
    policy = descriptor["sbom"]["profiles"][profile]
    require(policy["format"] == "rmux-sbom-json" and policy["version"] == 1
            and policy["profile"] == profile and policy["subject_variant"] == variant
            and policy["target"] == "x86_64-unknown-linux-gnu", "unsupported SBOM policy")
    require(policy["relation_types"] == ["contains", "built_from", "instance_of", "depends_on"]
            and policy["reuse_key_version"] == 1
            and policy["component_scope"] == "archive-files-and-actual-contained-binary-cargo-build-closures",
            "unsupported closure profile")
    preserved = {k: v for k, v in descriptor.items() if k != "sbom"}
    require(digest(canonical(preserved)) == policy["preserved_layout_sha256"], "layout changed")
    pins = {}
    for name, expected_path in [("generator", "scripts/packaging/sbom.py"),
                                ("validator", "scripts/packaging/sbom.py"),
                                ("layout", "scripts/packaging/layout.py"),
                                ("schema", "scripts/packaging/sbom-schema-v1.json")]:
        ref = (dict(path=policy["schema_path"], sha256=policy["schema_sha256"])
               if name == "schema" else policy[name])
        require(ref["path"] == expected_path, "unexpected tool path: " + name)
        require(file_hash(ROOT / expected_path) == ref["sha256"], "tool pin mismatch: " + name)
        pins[name] = ref["sha256"]
    require(file_hash(Path(sys.executable).resolve()) == policy["interpreter"]["sha256"], "interpreter pin mismatch")
    require(file_hash(Path(toml_reader.__file__)) == "5b30a806a5e763926de4b12d4a4bab450162c9b56f136f9c3eb6032608bd18e2",
            "TOML reader pin mismatch")
    require(sys.version == policy["interpreter"]["version"], "interpreter version mismatch")
    pins.update(interpreter=policy["interpreter"]["sha256"], descriptor=file_hash(descriptor_path))
    schema = loads((ROOT / policy["schema_path"]).read_bytes())
    require(schema["format"] == "rmux-closed-shape" and schema["version"] == 1, "schema format")
    if profile == RPM_PROFILE:
        schema = {**schema, "root": schema["rpm_root"]}
    if profile == DEB_PROFILE:
        schema = {**schema, "root": schema["deb_root"]}
    if profile in REPRO_PROFILES:
        schema = {**schema, "root": schema["repro_roots"][profile]}
    return descriptor, policy, pins, schema


def read_inputs(path, expected_digest):
    data = Path(path).read_bytes()
    require(digest(data) == expected_digest, "input manifest digest mismatch")
    manifest = loads(data)
    require(manifest["schema"] == 1 and manifest["profile"] == PROFILE, "input profile mismatch")
    require(set(manifest["references"]) == REQUIRED_INPUTS, "input references differ")
    base = Path(path).resolve().parent
    inputs = {}
    for name, reference in manifest["references"].items():
        relative = PurePosixPath(reference["path"])
        require(len(relative.parts) == 1 and relative.name not in (".", ".."), "input path")
        data = checked_file(reference, base / relative.name)
        inputs[name] = data if name in ("lock", "tool_versions") else loads(data)
    return manifest, inputs


def archive_files(subject, manifest, descriptor, proof):
    require(Path(subject).name == manifest["subject"]["name"], "subject name mismatch")
    require(Path(subject).stat().st_size == manifest["subject"]["bytes"]
            and file_hash(subject) == manifest["subject"]["sha256"], "subject digest mismatch")
    prefix = proof["metadata"]["package_name"]
    expected = {place["path"]: place for _, place in layout.entries(descriptor, "unix-archive")}
    files, captured, seen = {}, {}, set()
    with tarfile.open(subject, "r:gz") as archive:
        for member in archive:
            parts = PurePosixPath(member.name).parts
            require(parts and parts[0] == prefix and not member.name.startswith("/")
                    and ".." not in parts and member.name not in seen, "unsafe/duplicate tar member")
            seen.add(member.name)
            relative = "/".join(parts[1:])
            if member.isdir():
                require(not relative or any(p.startswith(relative + "/") for p in expected),
                        "unexpected tar directory")
                continue
            require(member.isfile() and relative in expected and relative not in files,
                    "unexpected tar entry: " + relative)
            require(member.mode == int(expected[relative]["mode"], 8), "file mode mismatch")
            stream = archive.extractfile(member)
            h, total, content = hashlib.sha256(), 0, bytearray()
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                h.update(block)
                total += len(block)
                if relative == "share/rmux/artifact-metadata.json":
                    content.extend(block)
            require(total == member.size, "short tar content")
            files[relative] = dict(path=relative, bytes=total, sha256=h.hexdigest(), mode=member.mode)
            if content:
                captured["metadata"] = loads(content)
    require(set(files) == set(expected), "archive file set differs from descriptor")
    proven = {r["relative"]: r["stage"] for r in proof["files"]}
    require(set(proven) == set(files), "archive proof file set mismatch")
    for path, info in files.items():
        require(all(info[k] == proven[path][k] for k in ("bytes", "sha256")), "archive proof identity")
    metadata = captured["metadata"]
    require(metadata == proof["metadata"], "archive metadata differs from evidence")
    source = manifest["payload_source"]
    require(metadata["git_commit"] == source["commit"]
            and metadata["payload_provenance"]["source"] == source["commit"]
            and metadata["payload_provenance"]["tree"] == source["tree"], "payload source mismatch")
    require(metadata["target"] == manifest["target"]
            and metadata["configuration"] == manifest["configuration"], "target/profile mismatch")
    require(file_hash(subject) == manifest["subject"]["sha256"], "subject changed during inspection")
    return files, metadata


def build_graph(manifest, data):
    """Rebuild links from original Cargo JSON and checked native short fingerprints."""
    metadata, source = data["metadata"], manifest["payload_source"]
    package_by_id = {p["id"]: p for p in metadata["packages"]}
    require(len(package_by_id) == len(metadata["packages"]), "duplicate metadata package")
    byname = defaultdict(list)
    for package in package_by_id.values():
        byname[package["name"]].append(package)
    raw, auxiliary, numbers, package_ids = {}, {}, {}, {}
    for observation in data["auxiliary"]["observations"]:
        path = observation["record"]["path"]
        require(path not in auxiliary, "duplicate auxiliary unit")
        auxiliary[path] = observation
    for item in data["units"]["fingerprint_records"]:
        path, record = item["file"]["path"], item["record"]
        require(path.startswith(manifest["historical_target_root"] + "/") and path not in raw,
                "unexpected/duplicate unit path")
        require(loads(checked_file(item["file"])) == record, "historical unit JSON differs")
        observed = auxiliary[path]
        require(observed["record"] == item["file"] and observed["matches_record"], "unit binding mismatch")
        short = observed["fingerprint"]
        require(short["path"] == str(Path(path).with_suffix("")), "short fingerprint path")
        value = checked_file(short).decode("ascii")
        require(value == short["value"] and re.fullmatch("[0-9a-f]{16}", value), "short fingerprint")
        number = int.from_bytes(bytes.fromhex(value), "little")
        require(number not in numbers, "ambiguous unit fingerprint")
        numbers[number] = path
        raw[path] = record
        depfile = observed.get("depfile", {})
        versions = []
        if depfile.get("exists"):
            require(depfile["path"].startswith(manifest["historical_target_root"] + "/"), "depfile path")
            content = checked_file(depfile).decode()
            versions = sorted(set(re.findall(r"/registry/src/[^/ ]+/([^/ :]+)", content)))
            require(versions == depfile["registry_package_versions"], "depfile interpretation mismatch")
        owner = Path(path).parent.name.rsplit("-", 1)[0]
        candidates = byname[owner]
        matched = [p for p in candidates if p["name"] + "-" + p["version"] in versions]
        if len(matched) == 1:
            package_ids[path] = matched[0]["id"]
        elif len(candidates) == 1 and candidates[0]["source"] is None:
            package_ids[path] = candidates[0]["id"]
    require(set(raw) == set(auxiliary), "auxiliary unit set mismatch")
    edges = []
    for path, record in raw.items():
        for dependency in record["deps"]:
            require(len(dependency) == 4 and dependency[3] in numbers, "missing dependency relation")
            edges.append((path, numbers[dependency[3]], dependency[1]))
    for path in set(raw) - set(package_ids):
        targets = [b for a, b, name in edges if a == path and name == "build_script_build"]
        options = {package_ids[b] for b in targets if b in package_ids}
        require(len(options) == 1, "ambiguous package identity: " + path)
        identity = options.pop()
        require(package_by_id[identity]["name"] == Path(path).parent.name.rsplit("-", 1)[0],
                "build-script package identity differs")
        package_ids[path] = identity
    source_manifest = data["root_manifest"]
    require(source_manifest["commit"] == source["commit"] and source_manifest["tree"] == source["tree"],
            "source manifest mismatch")
    source_files = {e["path"]: e for e in source_manifest["entries"]}
    require(digest(data["lock"]) == source_files["Cargo.lock"]["sha256"], "lockfile/source mismatch")
    locked = {(p["name"], p["version"], p.get("source")): p for p in toml_reader.loads(data["lock"].decode())["package"]}
    packages, stable_package = [], {}
    for identity in sorted(set(package_ids.values())):
        p = package_by_id[identity]
        key = (p["name"], p["version"], p["source"])
        require(key in locked, "package missing from lockfile")
        origin = p["source"] or ("workspace:" + source["commit"])
        identifier = "pkg:" + digest(canonical([p["name"], p["version"], origin]))
        manifest_sha = None
        if p["source"] is None:
            relative = str(PurePosixPath(p["manifest_path"]).relative_to(manifest["historical_source_root"]))
            ref = source_files[relative]
            checked_file(ref, p["manifest_path"])
            manifest_sha = ref["sha256"]
        packages.append(dict(id=identifier, name=p["name"], version=p["version"], source=origin,
                             license=p["license"], checksum=locked[key].get("checksum"),
                             manifest_sha256=manifest_sha))
        stable_package[identity] = identifier
    units, unit_id = [], {}
    for path, record in raw.items():
        # Run-build fingerprints omit features. Bind them to their unique compiled
        # build script; treating the empty sentinel as [] would lose features.
        feature_record = record
        if record["features"] == "":
            require(Path(path).name == "run-build-script-build-script-build.json",
                    "empty features on a compiled unit")
            scripts = [b for a, b, name in edges if a == path and name == "build_script_build"]
            require(len(scripts) == 1 and raw[scripts[0]]["features"] != ""
                    and package_ids[scripts[0]] == package_ids[path], "ambiguous build-script features")
            feature_record = raw[scripts[0]]
        features = loads(feature_record["features"])
        require(isinstance(features, list) and all(type(f) is str for f in features)
                and len(features) == len(set(features)), "unit features")
        short = auxiliary[path]["fingerprint"]["value"]
        kind = Path(path).name[:-5]
        unit = dict(package=stable_package[package_ids[path]], kind=kind,
                    context=("target" if Path(path).relative_to(manifest["historical_target_root"]).parts[0]
                             == manifest["target"] else "host"),
                    target=record["target"], profile=record["profile"], features=sorted(features),
                    rustflags=record["rustflags"], compile_kind=record["compile_kind"], fingerprint=short,
                    historical_record_sha256=auxiliary[path]["record"]["sha256"])
        unit["id"] = "unit:" + digest(canonical(unit))
        unit_id[path] = unit["id"]
        units.append(unit)
    require(len({u["id"] for u in units}) == len(units), "duplicate canonical unit")
    return packages, units, edges, raw, unit_id, package_by_id


def expected_features(package, argv):
    selected = set() if "--no-default-features" in argv else {"default"}
    for i, arg in enumerate(argv[:-1]):
        if arg == "--features":
            selected.update(re.split(r"[, ]+", argv[i + 1]))
    pending = list(selected)
    while pending:
        feature = pending.pop()
        for child in package["features"].get(feature, []):
            if child in package["features"] and child not in selected:
                selected.add(child)
                pending.append(child)
    return sorted(selected)


def closure_fields(document):
    return {k: v for k, v in document.items() if k not in ("subject", "closure_sha256")}


def same_closure(left, right):
    return (left["closure_sha256"] == digest(canonical(closure_fields(left)))
            and right["closure_sha256"] == digest(canonical(closure_fields(right)))
            and canonical(closure_fields(left)) == canonical(closure_fields(right)))


def reconstruct(descriptor_path, inputs_path, inputs_sha256, subject):
    descriptor, policy, pins, schema = tool_policy(descriptor_path)
    manifest, data = read_inputs(inputs_path, inputs_sha256)
    require(manifest["target"] == policy["target"] and manifest["configuration"] == "release", "resolution context")
    files, metadata = archive_files(subject, manifest, descriptor, data["archive_proof"])
    packages, units, edges, raw, unit_ids, package_by_id = build_graph(manifest, data)
    payload = data["payload_result"]
    require(payload["O1"]["source_commit"] == manifest["payload_source"]["commit"]
            and payload["O1"]["source_tree"] == manifest["payload_source"]["tree"], "build source mismatch")
    receipt = data["metadata_receipt"]
    require(receipt["direct_status"] == 0 and receipt["stdout"]["sha256"]
            == manifest["references"]["metadata"]["sha256"], "metadata receipt mismatch")
    require(receipt["argv"][receipt["argv"].index("--filter-platform") + 1] == manifest["target"], "metadata target")
    root_package = package_by_id[data["metadata"]["resolve"]["root"]]
    roles = layout.payload_binary_roles(descriptor, "unix-archive")
    roots, contexts, runtime, visited = [], [], [], set()
    adjacency = defaultdict(list)
    for a, b, _ in edges:
        adjacency[a].append(b)
    stages = {b["stage"]: b for b in payload["O1"]["builds"]}
    payloads = {p["name"]: p for p in payload["O2"]["payload"]}
    for role, stage, name in [("helper", "full", "rmux-full"), ("cli", "tiny", "rmux"), ("daemon", "daemon", "rmux-daemon")]:
        build, native = stages[stage], data["build_" + stage]
        require(build["argv"] == native["argv"] and native["direct_status"] == 0
                and native["supervisor_status"] == 0 and build["SSH_direct_status"] == 0, "build receipt mismatch")
        argv = build["argv"]
        require(argv[argv.index("--target") + 1] == manifest["target"]
                and all(a in argv for a in ("--release", "--locked", "--offline")), "build recipe context")
        binary = argv[argv.index("--bin") + 1]
        features = expected_features(root_package, argv)
        matches = [path for path, rec in raw.items() if Path(path).name == "bin-" + binary + ".json"
                   and sorted(loads(rec["features"])) == features and rec["compile_kind"] != 0]
        require(len(matches) == 1, "ambiguous binary root/features")
        root = matches[0]
        pending, seen = [root], set()
        while pending:
            current = pending.pop()
            if current not in seen:
                seen.add(current)
                pending.extend(adjacency[current])
        visited.update(seen)
        info, original = files[roles[role]], payloads[name]
        require(all(info[k] == original["local"][k] for k in ("bytes", "sha256")), "payload identity mismatch")
        roots.append(dict(role=role, path=roles[role], sha256=info["sha256"], bytes=info["bytes"],
                          unit=unit_ids[root], reachable_units=len(seen)))
        contexts.append(dict(role=role, target=manifest["target"], configuration="release", features=features,
                             lock_sha256=manifest["references"]["lock"]["sha256"], rustflags=raw[root]["rustflags"]))
        runtime.append(dict(role=role, needed=original["needed"], glibc_max=".".join(map(str, original["glibc_max"]))))
    require(visited == set(raw), "unit outside actual binary closures")
    relations = [dict(kind="depends_on", source=unit_ids[a], target=unit_ids[b], name=n) for a, b, n in edges]
    require(len({canonical(r) for r in relations}) == len(relations), "duplicate dependency relation")
    relations += [dict(kind="instance_of", source=u["id"], target=u["package"], name="") for u in units]
    relations += [dict(kind="contains", source="subject", target="file:" + path, name="") for path in files]
    relations += [dict(kind="built_from", source="file:" + r["path"], target=r["unit"], name="") for r in roots]
    evidence = [dict(name=name, sha256=ref["sha256"], bytes=ref["bytes"])
                for name, ref in manifest["references"].items()]
    document = dict(format=policy["format"], version=policy["version"], profile=PROFILE,
                    subject=manifest["subject"], payload_source=manifest["payload_source"],
                    resolution_contexts=sorted(contexts, key=lambda r: r["role"]),
                    files=sorted(files.values(), key=lambda f: f["path"]),
                    packages=sorted(packages, key=lambda p: p["id"]),
                    build_units=sorted(units, key=lambda u: u["id"]),
                    binary_roots=sorted(roots, key=lambda r: r["role"]),
                    relationships=sorted(relations, key=canonical), recipe_evidence=sorted(evidence, key=lambda r: r["name"]),
                    external_runtime_requirements=sorted(runtime, key=lambda r: r["role"]), tool_pins=pins)
    document["closure_sha256"] = digest(canonical(closure_fields(document)))
    shape(document, schema["root"], schema["definitions"])
    return document, schema


def rpm_headers(raw):
    """Bound the RPM v3 lead and both headers; interpret only required file tags."""
    require(raw[:4] == b"\xed\xab\xee\xdb" and raw[4:6] == b"\x03\x00", "RPM lead")
    def header(offset):
        require(raw[offset:offset + 8] == b"\x8e\xad\xe8\x01\0\0\0\0", "RPM header magic")
        count, length = struct.unpack_from(">II", raw, offset + 8)
        store = offset + 16 + 16 * count
        end = store + length
        require(count <= 4096 and end <= len(raw), "RPM header bounds")
        tags = {}
        for i in range(count):
            tag, kind, start, size = struct.unpack_from(">IIII", raw, offset + 16 + i * 16)
            require(tag not in tags and start <= length, "RPM duplicate/outside tag")
            tags[tag] = (kind, start, size)
        return end, raw[store:end], tags
    signature_end, _, _ = header(96)
    main = (signature_end + 7) // 8 * 8
    payload, store, tags = header(main)
    def value(tag, kind):
        actual, start, count = tags[tag]
        require(actual == kind and count <= 4096, "RPM tag type/count")
        if kind in (3, 4):
            width = {3: 2, 4: 4}[kind]
            require(start + width * count <= len(store), "RPM integer bounds")
            return [int.from_bytes(store[start + i * width:start + (i + 1) * width], "big")
                    for i in range(count)]
        parts = store[start:].split(b"\0", count)
        require(len(parts) > count, "RPM string bounds")
        return [part.decode("utf-8") for part in parts[:count]]
    identity = dict(zip(("name", "version", "release", "arch"),
                        (value(tag, 6)[0] for tag in (1000, 1001, 1002, 1022))))
    names, dirs, indices = value(1117, 8), value(1118, 8), value(1116, 4)
    sizes, modes, hashes = value(1028, 4), value(1030, 3), value(1035, 8)
    links, users, groups = value(1036, 8), value(1039, 8), value(1040, 8)
    require(value(5011, 4) == [8], "RPM file digest algorithm must be SHA256")
    require(all(len(v) == len(names) for v in (indices, sizes, modes, hashes, links, users, groups)),
            "RPM file arrays differ")
    files = {}
    for i, name in enumerate(names):
        require(indices[i] < len(dirs), "RPM directory index")
        path = dirs[indices[i]] + name
        require(path.startswith("/") and ".." not in PurePosixPath(path).parts
                and str(PurePosixPath(path)) == path, "unsafe RPM header path")
        path = path[1:]
        require(path not in files and stat.S_ISREG(modes[i]) and not links[i]
                and users[i] == groups[i] == "root", "RPM file type/link/owner")
        files[path] = dict(path=path, bytes=sizes[i], sha256=hashes[i], mode=stat.S_IMODE(modes[i]))
    sections = [dict(name=name, offset=start, bytes=end - start, sha256=digest(raw[start:end]))
                for name, start, end in (("lead", 0, 96), ("signature_header", 96, signature_end),
                                        ("main_header", main, payload), ("payload", payload, len(raw)))]
    return identity, files, sections


def rpm_files(subject, reader, max_total=None):
    """Stream native libarchive RPM/gzip/cpio data; never create archive paths."""
    require(file_hash(reader["path"]) == reader["sha256"], "RPM reader pin mismatch")
    native = ctypes.CDLL(reader["path"])
    pointer, integer, string = ctypes.c_void_p, ctypes.c_int, ctypes.c_char_p
    functions = {"archive_read_new": ([], pointer), "archive_version_string": ([], string),
                 "archive_read_open_filename": ([pointer, string, ctypes.c_size_t], integer),
                 "archive_read_next_header": ([pointer, ctypes.POINTER(pointer)], integer),
                 "archive_read_data": ([pointer, pointer, ctypes.c_size_t], ctypes.c_ssize_t),
                 "archive_entry_size": ([pointer], ctypes.c_int64),
                 "archive_entry_mode": ([pointer], ctypes.c_uint),
                 "archive_entry_nlink": ([pointer], ctypes.c_uint)}
    for name in ("archive_read_free", "archive_read_support_filter_rpm",
                 "archive_read_support_filter_gzip", "archive_read_support_format_cpio"):
        functions[name] = ([pointer], integer)
    for name in ("archive_entry_pathname", "archive_entry_symlink", "archive_entry_hardlink"):
        functions[name] = ([pointer], string)
    for name, (args, result) in functions.items():
        function = getattr(native, name)
        function.argtypes, function.restype = args, result
    require(native.archive_version_string().decode() == reader["version"], "RPM reader version")
    archive = native.archive_read_new()
    require(archive is not None, "RPM reader allocation")
    files, metadata, buffer, entry = {}, None, ctypes.create_string_buffer(1024 * 1024), pointer()
    expanded = 0
    try:
        for name in ("archive_read_support_filter_rpm", "archive_read_support_filter_gzip",
                     "archive_read_support_format_cpio"):
            require(getattr(native, name)(archive) == 0, "RPM reader support")
        require(native.archive_read_open_filename(archive, str(subject).encode(), 65536) == 0,
                "RPM reader open")
        while True:
            status = native.archive_read_next_header(archive, ctypes.byref(entry))
            if status == 1:
                break
            require(status == 0 and len(files) < 256, "RPM reader header")
            raw_path = native.archive_entry_pathname(entry).decode()
            path = raw_path.removeprefix("./")
            require(raw_path == "./" + path and not path.startswith("/")
                    and ".." not in PurePosixPath(path).parts and str(PurePosixPath(path)) == path
                    and path not in files, "unsafe/duplicate RPM payload path")
            mode, size = native.archive_entry_mode(entry), native.archive_entry_size(entry)
            require(stat.S_ISREG(mode) and native.archive_entry_nlink(entry) == 1
                    and not native.archive_entry_symlink(entry) and not native.archive_entry_hardlink(entry)
                    and 0 <= size <= 64 * 1024 * 1024, "RPM payload type/link/size")
            expanded += size
            require(max_total is None or expanded <= max_total, "RPM expansion bound")
            h, total, content = hashlib.sha256(), 0, bytearray()
            while True:
                count = native.archive_read_data(archive, buffer, len(buffer))
                require(count >= 0, "RPM reader data")
                if count == 0:
                    break
                block = buffer.raw[:count]
                h.update(block)
                total += count
                require(total <= size, "RPM payload size exceeded")
                if path == "usr/share/rmux/artifact-metadata.json":
                    require(total <= 1024 * 1024, "RPM metadata size")
                    content.extend(block)
            require(total == size, "RPM payload short file")
            files[path] = dict(path=path, bytes=size, sha256=h.hexdigest(), mode=stat.S_IMODE(mode))
            if content:
                metadata = loads(content)
    finally:
        native.archive_read_free(archive)
    return files, metadata


def cargo_projection(document):
    """Binary build closure only; whole-document same_closure remains stricter."""
    result = {key: document[key] for key in ("payload_source", "resolution_contexts", "packages",
                                           "build_units", "external_runtime_requirements")}
    result["binary_roots"] = [{k: v for k, v in root.items() if k != "path"}
                              for root in document["binary_roots"]]
    result["relationships"] = [r for r in document["relationships"]
                               if r["kind"] in ("depends_on", "instance_of")]
    result["recipe_evidence"] = [r for r in document["recipe_evidence"] if r["name"] in REQUIRED_INPUTS]
    return result


def reconstruct_rpm(descriptor_path, inputs_path, inputs_sha256, subject):
    descriptor, policy, pins, schema = tool_policy(descriptor_path, RPM_PROFILE)
    raw_manifest = Path(inputs_path).read_bytes()
    require(digest(raw_manifest) == inputs_sha256, "input manifest digest mismatch")
    manifest, base = loads(raw_manifest), Path(inputs_path).resolve().parent
    require(manifest["schema"] == 1 and manifest["profile"] == RPM_PROFILE, "RPM input profile")
    require(set(manifest["references"]) == {"rpm_result", "rpm_install", "rpm_signed", "tar_sbom", "tar_review"},
            "RPM input references")
    data = {}
    for name, ref in manifest["references"].items():
        require(PurePosixPath(ref["path"]).name == ref["path"], "RPM reference path")
        data[name] = loads(checked_file(ref, base / ref["path"]))
    require(Path(subject).name == manifest["subject"]["name"], "RPM subject name")
    raw = checked_file(manifest["subject"], subject)
    candidate = data["rpm_result"]["candidate"]
    require(all(manifest["subject"][key] == candidate[key] for key in ("bytes", "sha256"))
            and data["rpm_install"]["candidate_sha256"] == candidate["sha256"]
            and data["rpm_signed"]["candidate_after"]["sha256"] == candidate["sha256"], "RPM receipts subject")
    identity, header_files, sections = rpm_headers(raw)
    require(identity == dict(name="rmux", version="0.10.0", release="1", arch="x86_64"), "RPM identity")
    for part in sections:
        if part["name"] in ("main_header", "payload"):
            require(part["sha256"] == candidate[part["name"] + "_sha256"], "RPM section mismatch")
    files, metadata = rpm_files(subject, policy["native_reader"])
    expected = {place["path"]: int(place["mode"], 8) for _, place in layout.entries(descriptor, "rpm")}
    require(set(files) == set(expected) and files == header_files, "RPM file set/header mismatch")
    proven = {r["path"].removeprefix("/"): r for r in data["rpm_install"]["files"]}
    require(set(proven) == set(files), "RPM historical file set mismatch")
    for path, item in files.items():
        require(item["mode"] == expected[path] == int(proven[path]["mode"], 8)
                and all(item[k] == proven[path][k] for k in ("bytes", "sha256")), "RPM historical file identity")
    parent, review = data["tar_sbom"], data["tar_review"]
    require(review["verdict"] == "ACCEPT_BOUNDED_SBOM"
            and review["sidecar"]["sha256"] == manifest["references"]["tar_sbom"]["sha256"]
            and review["closure_sha256"] == parent["closure_sha256"], "UNPROVEN parent acceptance")
    require(parent["closure_sha256"] == digest(canonical(closure_fields(parent))), "UNPROVEN parent closure")
    tar_ref = manifest["tar_inputs"]
    require(tar_ref["path"] == "tar-inputs/INPUTS.json", "tar input path")
    checked_file(tar_ref, base / tar_ref["path"])
    checked_file(manifest["tar_subject"])
    current, _ = reconstruct(descriptor_path, base / tar_ref["path"], tar_ref["sha256"],
                             manifest["tar_subject"]["path"])
    projection = lambda d: {k: v for k, v in d.items() if k not in ("tool_pins", "closure_sha256")}
    require(projection(parent) == projection(current), "UNPROVEN reconstructed tar closure")
    require(metadata["git_commit"] == parent["payload_source"]["commit"] == candidate["source_sha"]
            and parent["payload_source"]["tree"] == candidate["source_tree"]
            and metadata["target"] == policy["target"] and metadata["configuration"] == "release"
            and metadata["release_artifact"] is False, "RPM source/target/configuration")
    result, roles, equality = copy.deepcopy(parent), layout.payload_binary_roles(descriptor, "rpm"), []
    for root in result["binary_roots"]:
        item = files[roles[root["role"]]]
        prefix = {"cli": "", "helper": "helper_", "daemon": "daemon_"}[root["role"]]
        require(all(item[k] == root[k] == metadata[prefix + "binary_" + k]
                    for k in ("bytes", "sha256")), "UNPROVEN RPM binary differs")
        equality.append(dict(role=root["role"], tar_path=root["path"], rpm_path=item["path"],
                             bytes=item["bytes"], sha256=item["sha256"]))
        root["path"] = item["path"]
    result.update(profile=RPM_PROFILE, subject=manifest["subject"], files=sorted(files.values(), key=lambda f: f["path"]),
                  tool_pins=pins)
    result["relationships"] = [r for r in result["relationships"] if r["kind"] in ("depends_on", "instance_of")]
    result["relationships"] += [dict(kind="contains", source="subject", target="file:" + path, name="") for path in files]
    result["relationships"] += [dict(kind="built_from", source="file:" + r["path"], target=r["unit"], name="")
                                for r in result["binary_roots"]]
    result["relationships"].sort(key=canonical)
    result["recipe_evidence"] += [dict(name=name, bytes=ref["bytes"], sha256=ref["sha256"])
                                  for name, ref in manifest["references"].items()]
    result["recipe_evidence"].sort(key=lambda r: r["name"])
    require(cargo_projection(result) == cargo_projection(parent), "UNPROVEN Cargo closure differs")
    result["rpm_binding"] = dict(identity=identity, sections=sections, regular_single_link_files_only=True,
                                 native_reader_sha256=policy["native_reader"]["sha256"],
                                 native_reader_version=policy["native_reader"]["version"])
    result["reuse_evidence"] = dict(rule="identical-binaries-and-complete-cargo-closure-v1",
                                    parent_sbom_sha256=manifest["references"]["tar_sbom"]["sha256"],
                                    parent_subject_sha256=parent["subject"]["sha256"],
                                    parent_closure_sha256=parent["closure_sha256"],
                                    review_sha256=manifest["references"]["tar_review"]["sha256"],
                                    cargo_closure_sha256=digest(canonical(cargo_projection(parent))), binaries=equality)
    result["closure_sha256"] = digest(canonical(closure_fields(result)))
    shape(result, schema["root"], schema["definitions"])
    require(file_hash(subject) == manifest["subject"]["sha256"], "RPM changed during reading")
    return result, schema



def deb_ar(raw):
    """Read the acquired Debian ar layout, retaining each header and member hash."""
    require(len(raw) <= 64 * 1024 * 1024 and raw[:8] == b"!<arch>\n", "Debian ar magic/size")
    offset, contents, members = 8, {}, []
    for expected in ("debian-binary", "control.tar.zst", "data.tar.zst"):
        header = raw[offset:offset + 60]
        require(len(header) == 60 and header[-2:] == b"`\n", "Debian ar header")
        name = header[:16].decode("ascii").rstrip()
        require(name == expected and int(header[28:34]) == int(header[34:40]) == 0
                and int(header[40:48], 8) == 0o100644, "Debian ar name/owner/mode")
        size, start = int(header[48:58]), offset + 60
        end = start + size
        require(0 <= size <= 64 * 1024 * 1024 and end <= len(raw), "Debian ar member bounds")
        content = raw[start:end]
        require(size % 2 == 0 or raw[end:end + 1] == b"\n", "Debian ar padding")
        contents[name] = content
        members.append(dict(name=name, header_offset=offset, header_sha256=digest(header),
                            offset=start, bytes=size, sha256=digest(content)))
        offset = end + size % 2
    require(offset == len(raw) and contents["debian-binary"] == b"2.0\n", "Debian ar members/version")
    return contents, members


def deb_tar(raw, reader, expected_mtime=None):
    """Read zstd/tar with the pinned native reader, without filesystem extraction."""
    require(file_hash(reader["path"]) == reader["sha256"], "Debian reader pin mismatch")
    native = ctypes.CDLL(reader["path"])
    pointer, integer, string = ctypes.c_void_p, ctypes.c_int, ctypes.c_char_p
    functions = {"archive_read_new": ([], pointer), "archive_version_string": ([], string),
                 "archive_read_open_memory": ([pointer, pointer, ctypes.c_size_t], integer),
                 "archive_read_next_header": ([pointer, ctypes.POINTER(pointer)], integer),
                 "archive_read_data": ([pointer, pointer, ctypes.c_size_t], ctypes.c_ssize_t),
                 "archive_entry_size": ([pointer], ctypes.c_int64),
                 "archive_entry_mode": ([pointer], ctypes.c_uint),
                 "archive_entry_uid": ([pointer], ctypes.c_int64),
                 "archive_entry_gid": ([pointer], ctypes.c_int64),
                 "archive_entry_mtime": ([pointer], ctypes.c_int64),
                 "archive_entry_mtime_nsec": ([pointer], ctypes.c_long)}
    for name in ("archive_read_free", "archive_read_support_filter_zstd", "archive_read_support_format_tar"):
        functions[name] = ([pointer], integer)
    for name in ("archive_entry_pathname", "archive_entry_symlink", "archive_entry_hardlink"):
        functions[name] = ([pointer], string)
    for name, (args, result) in functions.items():
        function = getattr(native, name)
        function.argtypes, function.restype = args, result
    require(native.archive_version_string().decode() == reader["version"], "Debian reader version")
    archive = native.archive_read_new()
    require(archive is not None, "Debian reader allocation")
    source, buffer, entry = ctypes.create_string_buffer(raw), ctypes.create_string_buffer(1024 * 1024), pointer()
    files, directories, captured, seen, expanded = {}, [], {}, set(), 0
    try:
        require(native.archive_read_support_filter_zstd(archive) == 0
                and native.archive_read_support_format_tar(archive) == 0
                and native.archive_read_open_memory(archive, source, len(raw)) == 0, "Debian reader open")
        while True:
            status = native.archive_read_next_header(archive, ctypes.byref(entry))
            if status == 1:
                break
            require(status == 0 and len(seen) < 256, "Debian reader header/count")
            name = native.archive_entry_pathname(entry).decode()
            mode, size = native.archive_entry_mode(entry), native.archive_entry_size(entry)
            path = name.removeprefix("./").removesuffix("/") or "."
            expected = "./" if path == "." else "./" + path + ("/" if stat.S_ISDIR(mode) else "")
            require(name == expected and not path.startswith("/") and ".." not in PurePosixPath(path).parts
                    and str(PurePosixPath(path)) == path and path not in seen, "unsafe/duplicate Debian path")
            require(not native.archive_entry_symlink(entry) and not native.archive_entry_hardlink(entry)
                    and native.archive_entry_uid(entry) == native.archive_entry_gid(entry) == 0,
                    "Debian link/owner")
            if expected_mtime is not None:
                require(native.archive_entry_mtime(entry) == expected_mtime
                        and native.archive_entry_mtime_nsec(entry) == 0, "Debian epoch differs")
            seen.add(path)
            if stat.S_ISDIR(mode):
                require(stat.S_IMODE(mode) == 0o755 and size == 0, "Debian directory mode/size")
                directories.append(dict(path=path, mode=0o755))
                continue
            require(stat.S_ISREG(mode) and path != "." and 0 <= size <= 64 * 1024 * 1024,
                    "Debian entry type/size")
            expanded += size
            require(expanded <= 64 * 1024 * 1024, "Debian expansion bound")
            h, total, content = hashlib.sha256(), 0, bytearray()
            capture = path in ("control", "usr/share/rmux/artifact-metadata.json")
            require(not capture or size <= 1024 * 1024, "Debian metadata/control bound")
            while True:
                count = native.archive_read_data(archive, buffer, len(buffer))
                require(count >= 0, "Debian reader data")
                if count == 0:
                    break
                block = buffer.raw[:count]
                h.update(block)
                total += count
                require(total <= size, "Debian entry exceeds size")
                if capture:
                    content.extend(block)
            require(total == size, "Debian short entry")
            files[path] = dict(path=path, mode=stat.S_IMODE(mode), bytes=size, sha256=h.hexdigest())
            if capture:
                captured[path] = bytes(content)
    finally:
        native.archive_read_free(archive)
    return files, sorted(directories, key=lambda d: d["path"]), captured


def deb_control(raw):
    fields, key = {}, None
    for line in raw.decode("utf-8").splitlines():
        require(line != "", "Debian control multiple/empty stanza")
        if line.startswith((" ", "\t")):
            require(key is not None, "Debian control orphan continuation")
            fields[key] += "\n" + line
        else:
            key, separator, value = line.partition(":")
            require(separator and re.fullmatch(r"[A-Za-z][A-Za-z0-9-]*", key) and key not in fields,
                    "Debian control field/duplicate")
            fields[key] = value.strip()
    identity = {k.lower(): fields[k] for k in ("Package", "Version", "Architecture")}
    require(identity == dict(package="rmux", version="0.10.0", architecture="amd64"), "Debian control identity")
    return identity, [dict(field=k, value=v) for k, v in sorted(fields.items())]


def reconstruct_deb(descriptor_path, inputs_path, inputs_sha256, subject):
    descriptor, policy, pins, schema = tool_policy(descriptor_path, DEB_PROFILE)
    raw_manifest = Path(inputs_path).read_bytes()
    require(digest(raw_manifest) == inputs_sha256, "input manifest digest mismatch")
    manifest, base = loads(raw_manifest), Path(inputs_path).resolve().parent
    require(manifest["schema"] == 1 and manifest["profile"] == DEB_PROFILE, "Debian input profile")
    require(set(manifest["references"]) == {"deb_result", "deb_acceptance", "deb_history", "deb_inventory",
                                           "tar_sbom", "tar_review"}, "Debian input references")
    data = {}
    for name, ref in manifest["references"].items():
        require(PurePosixPath(ref["path"]).name == ref["path"], "Debian reference path")
        data[name] = loads(checked_file(ref, base / ref["path"]))
    require(Path(subject).name == manifest["subject"]["name"] and Path(subject).stat().st_size <= 64 * 1024 * 1024,
            "Debian subject name/size")
    raw = checked_file(manifest["subject"], subject)
    original, acceptance, history = data["deb_result"], data["deb_acceptance"], data["deb_history"]
    acquired, inventory = acceptance["obligations"]["O2"], data["deb_inventory"]
    require(acquired["decision"] == "ACCEPT_REUSED_EXACT_IDENTITY" and inventory["passed"] is True
            and history["state"] == "BOUNDED_DEBIAN_MILESTONE_COMPLETE_WITH_HISTORICAL_EXCEPTIONS"
            and history["bounded_technical_acceptance"]["sha256"] == manifest["references"]["deb_acceptance"]["sha256"]
            and original["O2"]["actual_archive_delta"]["sha256"] == manifest["references"]["deb_inventory"]["sha256"],
            "UNPROVEN Debian acquisition")
    for ref in (acquired, original["O2"]["archive_native"], inventory["archive"]):
        require(all(ref[k] == manifest["subject"][k] for k in ("bytes", "sha256")), "Debian receipt subject")
    contents, members = deb_ar(raw)
    control, control_dirs, control_data = deb_tar(contents["control.tar.zst"], policy["native_reader"])
    files, directories, captured = deb_tar(contents["data.tar.zst"], policy["native_reader"])
    require(set(control) == {"control"} and control_dirs == [dict(path=".", mode=0o755)]
            and all(control["control"][k] == inventory["control"][k] for k in ("bytes", "sha256"))
            and control["control"]["mode"] == int(inventory["control"]["mode"], 8) == 0o644,
            "Debian acquired control differs")
    identity, fields = deb_control(control_data["control"])
    expected = {place["path"]: int(place["mode"], 8) for _, place in layout.entries(descriptor, "debian")}
    require(set(files) == set(expected) == set(inventory["files"]), "Debian file set differs")
    required_dirs = {str(parent) for path in expected for parent in PurePosixPath(path).parents}
    require({d["path"] for d in directories} == required_dirs == set(inventory["directories"]) | {"."},
            "Debian directory set differs")
    for path, item in files.items():
        proven = inventory["files"][path]
        require(item["mode"] == expected[path] == int(proven["mode"], 8)
                and all(item[k] == proven[k] for k in ("bytes", "sha256")), "Debian historical file identity")
    metadata = loads(captured["usr/share/rmux/artifact-metadata.json"])
    parent, review = data["tar_sbom"], data["tar_review"]
    require(review["verdict"] == "ACCEPT_BOUNDED_SBOM"
            and review["sidecar"]["sha256"] == manifest["references"]["tar_sbom"]["sha256"]
            and review["closure_sha256"] == parent["closure_sha256"], "UNPROVEN parent acceptance")
    require(parent["closure_sha256"] == digest(canonical(closure_fields(parent))), "UNPROVEN parent closure")
    tar_ref = manifest["tar_inputs"]
    require(tar_ref["path"] == "tar-inputs/INPUTS.json", "tar input path")
    checked_file(tar_ref, base / tar_ref["path"])
    checked_file(manifest["tar_subject"])
    current, _ = reconstruct(descriptor_path, base / tar_ref["path"], tar_ref["sha256"], manifest["tar_subject"]["path"])
    projection = lambda d: {k: v for k, v in d.items() if k not in ("tool_pins", "closure_sha256")}
    require(projection(parent) == projection(current), "UNPROVEN reconstructed tar closure")
    require(metadata == inventory["metadata"] and metadata["package_name"] == "rmux_0.10.0_amd64"
            and metadata["artifact_kind"] == "debian-package-binary" and metadata["archive_format"] == "deb"
            and metadata["git_commit"] == parent["payload_source"]["commit"] == acquired["source"]
            and parent["payload_source"]["tree"] == acquired["tree"]
            and metadata["target"] == policy["target"] and metadata["configuration"] == "release"
            and metadata["release_artifact"] is False, "Debian metadata/source/context")
    result, roles, equality = copy.deepcopy(parent), layout.payload_binary_roles(descriptor, "debian"), []
    for root in result["binary_roots"]:
        item = files[roles[root["role"]]]
        prefix = {"cli": "", "helper": "helper_", "daemon": "daemon_"}[root["role"]]
        require(all(item[k] == root[k] == metadata[prefix + "binary_" + k] for k in ("bytes", "sha256")),
                "UNPROVEN Debian binary differs")
        equality.append(dict(role=root["role"], tar_path=root["path"], deb_path=item["path"],
                             bytes=item["bytes"], sha256=item["sha256"]))
        root["path"] = item["path"]
    result.update(profile=DEB_PROFILE, subject=manifest["subject"], files=sorted(files.values(), key=lambda f: f["path"]),
                  tool_pins=pins)
    result["relationships"] = [r for r in result["relationships"] if r["kind"] in ("depends_on", "instance_of")]
    result["relationships"] += [dict(kind="contains", source="subject", target="file:" + path, name="") for path in files]
    result["relationships"] += [dict(kind="built_from", source="file:" + r["path"], target=r["unit"], name="")
                                for r in result["binary_roots"]]
    result["relationships"].sort(key=canonical)
    result["recipe_evidence"] += [dict(name=name, bytes=ref["bytes"], sha256=ref["sha256"])
                                  for name, ref in manifest["references"].items()]
    result["recipe_evidence"].sort(key=lambda r: r["name"])
    require(cargo_projection(result) == cargo_projection(parent), "UNPROVEN Cargo closure differs")
    result["deb_binding"] = dict(identity=identity, package_name=metadata["package_name"], ar_members=members,
                                 control_files=sorted(control.values(), key=lambda f: f["path"]),
                                 control_directories=control_dirs, data_directories=directories, control_fields=fields,
                                 all_entries_root_owned=True, links_allowed=False,
                                 native_reader_sha256=policy["native_reader"]["sha256"],
                                 native_reader_version=policy["native_reader"]["version"])
    result["reuse_evidence"] = dict(rule="identical-binaries-and-complete-cargo-closure-v1",
                                    parent_sbom_sha256=manifest["references"]["tar_sbom"]["sha256"],
                                    parent_subject_sha256=parent["subject"]["sha256"],
                                    parent_closure_sha256=parent["closure_sha256"],
                                    review_sha256=manifest["references"]["tar_review"]["sha256"],
                                    cargo_closure_sha256=digest(canonical(cargo_projection(parent))), binaries=equality)
    result["closure_sha256"] = digest(canonical(closure_fields(result)))
    shape(result, schema["root"], schema["definitions"])
    require(file_hash(subject) == manifest["subject"]["sha256"], "Debian changed during reading")
    return result, schema



def repro_reference(ref):
    """Read bounded JSON/text evidence; archive data uses the streaming readers."""
    require(type(ref["bytes"]) is int and 0 <= ref["bytes"] <= 16 * 1024 * 1024,
            "reproduction evidence bound")
    require(Path(ref["path"]).stat().st_size == ref["bytes"], "reproduction evidence size")
    return checked_file(ref)


def repro_archive(ref):
    path = Path(ref["path"])
    require(path.is_file() and not path.is_symlink() and 0 < ref["bytes"] <= 64 * 1024 * 1024
            and path.stat().st_size == ref["bytes"] and file_hash(path) == ref["sha256"],
            "reproduction archive pin mismatch")


def repro_packaging(manifest, policy, kind):
    """Derive the binding from the pinned original pair receipt and prior freeze."""
    reference = manifest["packaging_receipt"]
    require(reference["sha256"] == policy["packaging_receipt_sha256"], "foreign packaging receipt")
    receipt = loads(repro_reference(reference))
    require(receipt["status"] == "PASS", "packaging receipt did not pass")
    freeze_ref = receipt["frozen" if kind == "tar" else "freeze"]
    freeze = loads(repro_reference(freeze_ref))
    pair = freeze["pair"] if kind == "rpm-full" else receipt["pair"]
    require(freeze["at"] < receipt["start"] and pair == freeze["pair"],
            "packaging freeze chronology/pair")
    contract = freeze["contract"]
    repro_reference(contract)
    rows = receipt["assemblies" if kind == "tar" else "runs"]
    require(len(rows) == 2 and all(row["rc"] == 0 for row in rows)
            and receipt["archive_byte_identical" if kind in ("tar", "rpm-full", "deb-full", "tar-full") else "byte_identical"] is True,
            "packaging A/B outcome")
    require(all(row["environment"]["SOURCE_DATE_EPOCH"] == "1790347419" for row in rows),
            "packaging epoch differs")
    archives = [row["archive_pin" if kind == "deb" else "archive"] for row in rows]
    require(manifest["archives"] == archives and archives[0]["path"] != archives[1]["path"]
            and all(archives[0][k] == archives[1][k] for k in ("bytes", "sha256")),
            "foreign A/B archive binding")
    for ref in archives:
        repro_archive(ref)
    with Path(archives[0]["path"]).open("rb") as left, Path(archives[1]["path"]).open("rb") as right:
        while True:
            block = left.read(1024 * 1024)
            require(block == right.read(1024 * 1024), "A/B bytes differ")
            if not block:
                break
    source = dict(commit=freeze["candidate" if kind == "tar" else "head"], tree=freeze["tree"])
    script = freeze["product_source" if kind == "tar" else "script"]
    repro_reference(script)
    tools = (loads(repro_reference(freeze["tool_pins"])) if kind in ("rpm-full", "deb-full", "tar-full") else
             receipt["tools"] if kind == "tar" else freeze["tools"])
    tools = list(tools.values()) if isinstance(tools, dict) else tools
    tools = sorted([{k: tool[k] for k in ("path", "bytes", "sha256")} for tool in tools],
                   key=lambda tool: tool["path"])
    # These are historical assembly tool digests, not a claim of tool execution now.
    require(tools and len({t["path"] for t in tools}) == len(tools), "packaging tool set")
    return dict(receipt=reference, freeze=freeze_ref, contract=contract, archives=archives,
                packager_source=source, packager_script=script, historical_tools=tools,
                source_date_epoch=1790347419, pair=pair,
                review_scope=policy["review_scope"], signature_status="unsigned",
                provenance_kind="local-historical-build-and-fixed-assembly-not-ci-or-signed")


def repro_tar_files(subject, descriptor, full=None):
    expected = {place["path"]: int(place["mode"], 8)
                for _, place in layout.entries(descriptor, "unix-archive")}
    prefix, files, seen, expanded = "rmux-0.10.0-linux-x86_64", {}, set(), 0
    if full is not None:
        with Path(subject).open("rb") as stream:
            full["gzip_header_hex"] = stream.read(10).hex()
        require(full["gzip_header_hex"] == "1f8b0800000000000003", "full TAR gzip header differs")
        full.update(members=[], captured={})
    with tarfile.open(subject, "r|gz", tarinfo=FullTarInfo if full is not None else tarfile.TarInfo) as archive:
        for member in archive:
            name = member.name
            parts = PurePosixPath(name).parts
            require(len(seen) < 256 and name not in seen and not name.startswith("/")
                    and ".." not in parts and parts and parts[0] == prefix
                    and str(PurePosixPath(name)) == name, "unsafe/duplicate reproduction tar entry")
            seen.add(name)
            relative = "/".join(parts[1:])
            if full is not None:
                full["members"].append(full_tar_header(member, relative))
            if member.isdir():
                require(not relative or any(p.startswith(relative + "/") for p in expected),
                        "unexpected reproduction tar directory")
                continue
            require(member.isfile() and relative in expected and member.mode == expected[relative]
                    and 0 <= member.size <= 64 * 1024 * 1024, "reproduction tar file/type/size")
            expanded += member.size
            require(expanded <= 64 * 1024 * 1024, "reproduction tar expansion bound")
            stream, h, total = archive.extractfile(member), hashlib.sha256(), 0
            capture = full is not None and relative in ("share/rmux/artifact-metadata.json", "share/rmux/package-layout.tsv", "SHA256SUMS.txt")
            require(not capture or member.size <= 1024 * 1024, "full TAR metadata bound")
            content = bytearray()
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                h.update(block)
                total += len(block)
                require(total <= member.size, "reproduction tar content bound")
                if capture:
                    content.extend(block)
            require(total == member.size, "short reproduction tar entry")
            files[relative] = dict(path=relative, mode=member.mode, bytes=total, sha256=h.hexdigest())
            if capture:
                full["captured"][relative] = bytes(content)
    if full is not None:
        directories = {str(p) for name in expected for p in PurePosixPath(name).parents}
        require(len(directories) == 18 and [r["path"] for r in full["members"]] == sorted(set(expected) | directories),
                "full TAR member order/set differs")
    return files


def repro_container(subject, kind, descriptor, policy, parent):
    """Re-read the new container; no historical header or signature is inherited."""
    if kind == "tar":
        return repro_tar_files(subject, descriptor), {}
    if kind == "tar-full":
        container = {}
        files = repro_tar_files(subject, descriptor, container)
        captured = container.pop("captured")
        return files, dict(tar_binding=container, full_captured=captured)
    raw = Path(subject).read_bytes()  # Already bounded and pinned to <=64MiB.
    reader = policy["native_reader"]
    if kind in ("rpm", "rpm-full"):
        identity, header_files, sections = rpm_headers(raw)
        files, metadata = rpm_files(subject, reader, max_total=64 * 1024 * 1024)
        require(files == header_files, "RPM header/payload differs")
        count = struct.unpack_from(">I", raw, 104)[0]
        signature_tags = {struct.unpack_from(">I", raw, 112 + 16 * i)[0] for i in range(count)}
        require(signature_tags == {62, 269, 273, 1000, 1004, 1007, 1008},
                "reproduction RPM must be unsigned")
        binding = dict(identity=identity, sections=sections, regular_single_link_files_only=True,
                       native_reader_sha256=reader["sha256"], native_reader_version=reader["version"])
        return files, dict(rpm_binding=binding, **({"full_metadata": metadata} if kind == "rpm-full" else {}))
    contents, members = deb_ar(raw)
    epoch = 1790347419 if kind == "deb-full" else None
    control, control_dirs, captured = deb_tar(contents["control.tar.zst"], reader, epoch)
    files, directories, data_captured = deb_tar(contents["data.tar.zst"], reader, epoch)
    identity, fields = deb_control(captured["control"])
    binding = dict(identity=identity, package_name=parent["deb_binding"]["package_name"],
                   ar_members=members, control_files=sorted(control.values(), key=lambda f: f["path"]),
                   control_directories=control_dirs, data_directories=directories, control_fields=fields,
                   all_entries_root_owned=True, links_allowed=False,
                   native_reader_sha256=reader["sha256"], native_reader_version=reader["version"])
    if kind == "deb-full":
        require(all(int(raw[m["header_offset"] + 16:m["header_offset"] + 28]) == epoch for m in members),
                "full Debian ar epoch differs")
        require(all(contents[n].startswith(bytes.fromhex("28b52ffd"))
                    for n in ("control.tar.zst", "data.tar.zst")), "full Debian compression differs")
        return files, dict(deb_binding=binding, full_metadata=data_captured["usr/share/rmux/artifact-metadata.json"],
                           full_control=captured["control"])
    require({k: v for k, v in binding.items() if k != "ar_members"}
            == {k: v for k, v in parent["deb_binding"].items() if k != "ar_members"},
            "reproduction Debian control/directories differ")
    return files, dict(deb_binding=binding)


def repro_file_closure(files, parent, descriptor, variant):
    expected = {place["path"]: int(place["mode"], 8) for _, place in layout.entries(descriptor, variant)}
    require(set(files) == set(expected), "reproduction file set differs")
    require(files == {f["path"]: f for f in parent["files"]}, "reproduction payload differs")
    require(all(f["mode"] == expected[path] for path, f in files.items()), "reproduction file modes")
    roles = layout.payload_binary_roles(descriptor, variant)
    require({r["role"] for r in parent["binary_roots"]} == set(roles), "reproduction binary role set")
    for root in parent["binary_roots"]:
        require(root["path"] == roles[root["role"]]
                and all(files[root["path"]][k] == root[k] for k in ("bytes", "sha256")),
                "reproduction binary closure differs")


def full_rpm_file_closure(files, metadata, parent, descriptor, freeze, binding, policy):
    """Compare actual payload to the prior full-script oracle, then reuse Cargo only."""
    expected = {f["path"]: {k: f[k] for k in ("path", "bytes", "sha256", "mode")}
                for f in freeze["expected_files"]}
    modes = {place["path"]: int(place["mode"], 8) for _, place in layout.entries(descriptor, "rpm")}
    require(len(expected) == len(freeze["expected_files"]) == 13 and set(files) == set(modes)
            and files == expected and all(f["mode"] == modes[p] for p, f in files.items()),
            "full RPM file inventory differs")
    require(binding["packager_source"] == policy["packager_source"]
            and metadata["git_commit"] == binding["packager_source"]["commit"]
            and metadata["git_dirty"] is False, "full RPM packager source differs")
    provenance = metadata["payload_provenance"]
    require(dict(commit=provenance["source"], tree=provenance["tree"]) == parent["payload_source"]
            == policy["payload_source"], "full RPM payload source differs")
    require(metadata["target"] == metadata["package_target"] == policy["target"]
            and metadata["configuration"] == provenance["configuration"] == "release"
            and metadata["release_artifact"] is False and metadata["skip_build"] is True
            and metadata["reuse_release_binary"] is False, "full RPM build context differs")
    require(metadata == freeze["expected_metadata"], "full RPM metadata differs")
    metadata_file = files["usr/share/rmux/artifact-metadata.json"]
    require(all(metadata_file[k] == freeze["expected_metadata_bytes"][k] == policy["metadata"][k]
                for k in ("bytes", "sha256")), "full RPM metadata bytes differ")
    roles = layout.payload_binary_roles(descriptor, "rpm")
    roots = {r["role"]: r for r in parent["binary_roots"]}
    require(set(roots) == set(roles) and len(parent["binary_roots"]) == 3,
            "full RPM binary role set")
    declared = {f["role"]: f for f in provenance["files"]}
    require(len(provenance["files"]) == 3 and set(declared) == set(roots) and provenance["schema"] == 1,
            "full RPM declaration role set")
    for role, root in roots.items():
        item = files[roles[role]]
        prefix = {"cli": "", "helper": "helper_", "daemon": "daemon_"}[role]
        require(root["path"] == item["path"] == declared[role]["path"]
                and metadata[prefix + "binary_path"] == "/" + item["path"]
                and item["sha256"] == declared[role]["sha256"]
                and all(item[k] == root[k] == metadata[prefix + "binary_" + k] for k in ("bytes", "sha256")),
                "full RPM binary Cargo closure differs")
    require({r["role"] for r in parent["resolution_contexts"]} == set(roles), "full RPM Cargo roles")
    for context in parent["resolution_contexts"]:
        require(context["target"] == policy["target"] and context["configuration"] == "release"
                and context["rustflags"] == []
                and context["features"] == (["default", "tiny-cli", "web"] if context["role"] == "cli"
                                            else ["default", "web"]), "full RPM Cargo context differs")
    require(digest(canonical(cargo_projection(parent))) == policy["cargo_closure_sha256"],
            "full RPM historical Cargo projection differs")


def full_rpm_evidence(files, metadata, parent, descriptor, binding, policy):
    receipt = loads(repro_reference(binding["receipt"]))
    freeze = loads(repro_reference(binding["freeze"]))
    full_rpm_file_closure(files, metadata, parent, descriptor, freeze, binding, policy)
    require(freeze["job_id"] == "L1-015" and freeze["pair"] == 2 and freeze["clean"] is True
            and freeze["epoch"] == binding["source_date_epoch"] == 1790347419,
            "full RPM freeze context")
    prospective = repro_reference(freeze["expected_metadata_bytes"])
    require(loads(prospective) == metadata, "full RPM prospective metadata differs")
    declaration = loads(repro_reference(freeze["declaration"]))
    provenance = metadata["payload_provenance"]
    require(declaration == {"source": provenance["source"], "tree": provenance["tree"],
                            "configuration": provenance["configuration"],
                            "files": [{k: f[k] for k in ("role", "sha256")} for f in provenance["files"]]},
            "full RPM declaration differs")
    for item in freeze["expected_files"]:
        require(all(item[k] == item["source"][k] for k in ("bytes", "sha256")),
                "full RPM source inventory differs")
        repro_archive(item["source"])
    captures = []
    require([r["label"] for r in receipt["runs"]] == ["A", "B"], "full RPM pair labels")
    for row in receipt["runs"]:
        capture = row["capture"]
        require(repro_reference(capture["metadata"]) == prospective, "full RPM captured metadata differs")
        repro_reference(capture["spec"])
        repro_reference(capture["sources"])
        call = loads(repro_reference(capture["rpm_call"]))
        require(call["rc"] == 0 and row["start"] <= call["start"] < call["end"] <= row["end"]
                and call["source_date_epoch"] == "1790347419", "full RPM captured execution differs")
        require({k: call["real_executable"][k] for k in ("path", "bytes", "sha256")}
                in binding["historical_tools"], "full RPM historical executable differs")
        require(all(call["metadata_before"][k] == capture["metadata"][k]
                    and call["spec_before"][k] == capture["spec"][k] for k in ("bytes", "sha256")),
                "full RPM capture identity differs")
        require(binding["packager_script"]["path"] in row["argv"]
                and "--skip-build" in row["argv"] and "--payload-provenance" in row["argv"],
                "full RPM entry point differs")
        captures.append(dict(label=row["label"], **capture))
    return dict(metadata={k: files["usr/share/rmux/artifact-metadata.json"][k]
                          for k in ("path", "bytes", "sha256")},
                prospective_metadata=freeze["expected_metadata_bytes"], captures=captures,
                declaration=freeze["declaration"], historical_tools_receipt=freeze["tool_pins"],
                source_files=[dict(path=f["path"], source=f["source"]) for f in freeze["expected_files"]],
                generated_at_utc=metadata["generated_at_utc"], release_artifact=False, skip_build=True)


def full_deb_file_closure(files, metadata, control, parent, descriptor, freeze, binding, policy, container):
    """Bind the full Debian payload/control to its prospective oracle and Cargo roots."""
    expected = {f["path"]: {k: f[k] for k in ("path", "bytes", "sha256", "mode")}
                for f in freeze["expected_files"]}
    modes = {place["path"]: int(place["mode"], 8) for _, place in layout.entries(descriptor, "debian")}
    require(len(expected) == len(freeze["expected_files"]) == 14 and set(files) == set(modes)
            and files == expected and all(f["mode"] == modes[p] for p, f in files.items()),
            "full Debian file inventory differs")
    require(binding["packager_source"] == policy["packager_source"]
            and metadata["git_commit"] == binding["packager_source"]["commit"]
            and metadata["git_dirty"] is False, "full Debian packager source differs")
    provenance = metadata["payload_provenance"]
    require(dict(commit=provenance["source"], tree=provenance["tree"]) == parent["payload_source"]
            == policy["payload_source"], "full Debian payload source differs")
    require(metadata["target"] == metadata["package_target"] == policy["target"]
            and metadata["configuration"] == provenance["configuration"] == "release"
            and metadata["release_artifact"] is False and metadata["skip_build"] is True
            and metadata["reuse_release_binary"] is False, "full Debian build context differs")
    require(metadata == freeze["expected_metadata"] and metadata["generated_at_utc"] == policy["generated_at_utc"],
            "full Debian metadata differs")
    require(all(files["usr/share/rmux/artifact-metadata.json"][k]
                == freeze["expected_metadata_bytes"][k] == policy["metadata"][k]
                for k in ("bytes", "sha256")), "full Debian metadata bytes differ")
    require(len(control) == freeze["expected_control"]["bytes"] == policy["control"]["bytes"]
            and digest(control) == freeze["expected_control"]["sha256"] == policy["control"]["sha256"],
            "full Debian control differs")
    require(container["control_files"] == [dict(path="control", mode=0o644, bytes=len(control), sha256=digest(control))]
            and container["control_directories"] == [dict(path=".", mode=0o755)]
            and container["data_directories"] == [dict(path=p, mode=0o755) for p in freeze["expected_directories"]],
            "full Debian control/directory inventory differs")
    require(all(files["usr/share/doc/rmux/copyright"][k] == freeze["expected_copyright"][k]
                for k in ("bytes", "sha256")), "full Debian copyright differs")
    roles = layout.payload_binary_roles(descriptor, "debian")
    roots = {r["role"]: r for r in parent["binary_roots"]}
    declared = {f["role"]: f for f in provenance["files"]}
    require(set(roots) == set(roles) == set(declared) and len(parent["binary_roots"]) == 3
            and len(provenance["files"]) == 3 and provenance["schema"] == 1, "full Debian binary roles")
    for role, root in roots.items():
        item = files[roles[role]]
        prefix = {"cli": "", "helper": "helper_", "daemon": "daemon_"}[role]
        require(root["path"] == item["path"] == declared[role]["path"]
                and metadata[prefix + "binary_path"] == "/" + item["path"]
                and item["sha256"] == declared[role]["sha256"]
                and all(item[k] == root[k] == metadata[prefix + "binary_" + k] for k in ("bytes", "sha256")),
                "full Debian binary Cargo closure differs")
    require(len(parent["resolution_contexts"]) == 3
            and {r["role"] for r in parent["resolution_contexts"]} == set(roles), "full Debian Cargo roles")
    for context in parent["resolution_contexts"]:
        require(context["target"] == policy["target"] and context["configuration"] == "release"
                and context["rustflags"] == []
                and context["features"] == (["default", "tiny-cli", "web"] if context["role"] == "cli"
                                            else ["default", "web"]), "full Debian Cargo context differs")
    require(digest(canonical(cargo_projection(parent))) == policy["cargo_closure_sha256"],
            "full Debian historical Cargo projection differs")


def full_deb_observation(row, entry, call, tools):
    """A receipt without both real successful observations cannot confer provenance."""
    require(row.get("rc") == entry.get("rc") == call.get("rc") == 0,
            "full Debian missing successful observation")
    require(entry["argv"] == row["argv"]
            and row["submitted_at"] <= entry["start"] <= call["start"] < call["end"] <= entry["end"] <= row["end"],
            "full Debian observation command/chronology differs")
    require(call["archive_pin"] == row["archive"] and call["archive"] == row["output"] == row["archive"]["path"]
            and call["actual_argv"] == [call["real_executable"]["path"], "--root-owner-group", "--build",
                                       call["stage"], call["archive"]], "full Debian observed archive differs")
    require(call["real_executable"] in tools, "full Debian packaging tool differs")
    require(all(call["environment"][key] == row["environment"][key] == value for key, value in
                [("SOURCE_DATE_EPOCH", "1790347419"), ("DPKG_DEB_THREADS_MAX", "2"),
                 ("DPKG_DEB_COMPRESSOR_TYPE", "zstd"), ("DPKG_DEB_COMPRESSOR_LEVEL", "19")]),
            "full Debian packaging environment differs")


def full_deb_evidence(files, raw_metadata, control, parent, descriptor, binding, policy, container):
    receipt = loads(repro_reference(binding["receipt"]))
    freeze = loads(repro_reference(binding["freeze"]))
    metadata = loads(raw_metadata)
    full_deb_file_closure(files, metadata, control, parent, descriptor, freeze, binding, policy, container)
    require(receipt["job_id"] == freeze["job_id"] == "L1-017" and receipt["pair"] == freeze["pair"] == 1
            and receipt["assemblies"] == 2 and freeze["clean"] is True and freeze["epoch"] == 1790347419,
            "full Debian freeze context")
    require(repro_reference(freeze["expected_metadata_bytes"]) == raw_metadata
            and repro_reference(freeze["expected_control"]) == control, "full Debian prospective bytes differ")
    repro_reference(freeze["expected_copyright"])
    declaration = loads(repro_reference(freeze["declaration"]))
    provenance = metadata["payload_provenance"]
    require(declaration == dict(source=provenance["source"], tree=provenance["tree"], configuration="release",
                                files=[dict(role=f["role"], sha256=f["sha256"], bytes=files[f["path"]]["bytes"])
                                       for f in provenance["files"]]), "full Debian declaration differs")
    for item in freeze["expected_files"]:
        repro_archive(item["source"])
        if item["transform"] == "copy":
            require(all(item[k] == item["source"][k] for k in ("bytes", "sha256")), "full Debian source differs")
        else:
            require(item["transform"] == "gzip" and item["path"] == "usr/share/man/man1/rmux.1.gz"
                    and files[item["path"]] == next(f for f in parent["files"] if f["path"] == item["path"]),
                    "full Debian transformed source differs")
    observations, scripts = [], []
    require([r["label"] for r in receipt["runs"]] == ["A", "B"], "full Debian pair labels")
    names = {"ENTRY-CALL.json", "DPKG-CALL.json", "STAGE.json", "artifact-metadata.json", "control"}
    for row, planned in zip(receipt["runs"], freeze["runs"]):
        capture = row["captures"]
        require(set(capture) == names and row["argv"] == planned["argv"] and row["output"] == planned["output"],
                "full Debian missing capture or frozen command")
        loaded = {name: repro_reference(ref) for name, ref in capture.items()}
        entry, call = loads(loaded["ENTRY-CALL.json"]), loads(loaded["DPKG-CALL.json"])
        full_deb_observation(row, entry, call, binding["historical_tools"])
        require(loaded["artifact-metadata.json"] == raw_metadata and loaded["control"] == control,
                "full Debian captured bytes differ")
        require(call["metadata_before"] == capture["artifact-metadata.json"]
                and call["control_before"] == capture["control"] and call["stage_inventory"] == capture["STAGE.json"],
                "full Debian capture identity differs")
        stage = loads(loaded["STAGE.json"])
        stage_files = {f["path"]: {k: f[k] for k in ("path", "mode", "bytes", "sha256")}
                       for f in stage if f["type"] == "file"}
        require(stage_files == dict(files, **{"DEBIAN/control": dict(path="DEBIAN/control", mode=0o644,
                    bytes=len(control), sha256=digest(control))}), "full Debian captured stage differs")
        require(all(f["mtime_ns"] == 1790347419 * 1000000000 for f in stage), "full Debian stage epoch differs")
        inventory = loads(repro_reference(row["inventory"]))
        require(inventory["archive"] == row["archive"] and
                [{k: m[k] for k in ("name", "bytes", "sha256")} for m in inventory["ar"]]
                == [{k: m[k] for k in ("name", "bytes", "sha256")} for m in container["ar_members"]],
                "full Debian captured container differs")
        script = dict(binding["packager_script"], path=str(Path(planned["view"]) / "scripts/package-debian.sh"))
        repro_reference(script)
        require(script["path"] in entry["argv"] and "--skip-build" in entry["argv"]
                and "--payload-provenance" in entry["argv"], "full Debian entry point differs")
        scripts.append(script)
        observations.append(dict(label=row["label"], entry=capture["ENTRY-CALL.json"], dpkg=capture["DPKG-CALL.json"],
                                 stage=capture["STAGE.json"], metadata=capture["artifact-metadata.json"],
                                 control=capture["control"], inventory=row["inventory"]))
    ref = lambda item: {k: item[k] for k in ("path", "bytes", "sha256")}
    return dict(metadata=ref(files["usr/share/rmux/artifact-metadata.json"]),
                control=dict(path="control", bytes=len(control), sha256=digest(control)),
                copyright=ref(files["usr/share/doc/rmux/copyright"]),
                prospective_metadata=freeze["expected_metadata_bytes"], prospective_control=freeze["expected_control"],
                prospective_copyright=freeze["expected_copyright"], declaration=freeze["declaration"],
                historical_tools_receipt=freeze["tool_pins"], entry_scripts=scripts, observations=observations,
                source_files=[{k: f[k] for k in ("path", "source", "transform")} for f in freeze["expected_files"]],
                generated_at_utc=metadata["generated_at_utc"], release_artifact=False, skip_build=True,
                reuse_release_binary=False)


class FullTarInfo(tarfile.TarInfo):
    """Retain the actual header while the acquired reader streams each member."""
    @classmethod
    def _frombuf(cls, buf, encoding, errors, *, dircheck=True):
        # The pinned Python reader uses this hook for both streamed and direct headers.
        member = super()._frombuf(buf, encoding, errors, dircheck=dircheck)
        member.header_sha256 = digest(buf)
        member.header_magic = buf[257:265].hex()
        return member


def full_tar_header(member, relative):
    require(member.isfile() or member.isdir(), "full TAR member type differs")
    require(member.uid == member.gid == 0 and member.uname == member.gname == ""
            and not member.linkname, "full TAR owner/link differs")
    require(member.mtime == 1790347419 and not member.pax_headers, "full TAR epoch/extensions differ")
    require(member.header_magic == b"ustar\x0000".hex(), "full TAR header format differs")
    if member.isdir():
        require(member.mode == 0o755 and member.size == 0, "full TAR directory mode/size differs")
    return dict(path=relative or ".", kind="file" if member.isfile() else "directory",
                mode=member.mode, bytes=member.size, uid=member.uid, gid=member.gid,
                uname=member.uname, gname=member.gname, mtime=member.mtime,
                header_sha256=member.header_sha256, header_magic=member.header_magic)


def full_tar_file_closure(files, metadata, parent, descriptor, freeze, binding, policy):
    expected = {f["path"]: {k: f[k] for k in ("path", "bytes", "sha256", "mode")} for f in freeze["files"]}
    modes = {place["path"]: int(place["mode"], 8) for _, place in layout.entries(descriptor, "unix-archive")}
    require(len(expected) == len(freeze["files"]) == 16 and set(files) == set(modes)
            and files == expected and all(f["mode"] == modes[p] for p, f in files.items()), "full TAR file inventory differs")
    require(binding["packager_source"] == policy["packager_source"]
            and metadata["git_commit"] == binding["packager_source"]["commit"]
            and metadata["git_dirty"] is False, "full TAR packager source differs")
    provenance = metadata["payload_provenance"]
    require(dict(commit=provenance["source"], tree=provenance["tree"]) == parent["payload_source"]
            == policy["payload_source"], "full TAR payload source differs")
    require(metadata["target"] == metadata["package_target"] == policy["target"]
            and metadata["configuration"] == provenance["configuration"] == "release"
            and metadata["release_artifact"] is False and metadata["skip_build"] is True,
            "full TAR build context differs")
    roles = layout.payload_binary_roles(descriptor, "unix-archive")
    roots = {r["role"]: r for r in parent["binary_roots"]}
    declared = {f["role"]: f for f in provenance["files"]}
    require(set(roots) == set(roles) == set(declared) and len(parent["binary_roots"]) == 3
            and len(provenance["files"]) == 3 and provenance["schema"] == 1, "full TAR binary roles differ")
    require(metadata == freeze["expected_metadata"] and metadata["generated_at_utc"] == policy["generated_at_utc"],
            "full TAR metadata differs")
    require(all(files["share/rmux/artifact-metadata.json"][k] == freeze["expected_metadata_bytes"][k]
                == policy["metadata"][k] for k in ("bytes", "sha256")), "full TAR metadata bytes differ")
    for role, root in roots.items():
        item = files[roles[role]]
        prefix = {"cli": "", "helper": "helper_", "daemon": "daemon_"}[role]
        require(root["path"] == item["path"] == declared[role]["path"] == metadata[prefix + "binary_path"]
                and item["sha256"] == declared[role]["sha256"]
                and all(item[k] == root[k] == metadata[prefix + "binary_" + k] for k in ("bytes", "sha256")),
                "full TAR binary Cargo closure differs")
    require(len(parent["resolution_contexts"]) == 3
            and {r["role"] for r in parent["resolution_contexts"]} == set(roles), "full TAR Cargo roles differ")
    for context in parent["resolution_contexts"]:
        require(context["target"] == policy["target"] and context["configuration"] == "release"
                and context["rustflags"] == []
                and context["features"] == (["default", "tiny-cli", "web"] if context["role"] == "cli"
                                            else ["default", "web"]), "full TAR Cargo context differs")
    require(digest(canonical(cargo_projection(parent))) == policy["cargo_closure_sha256"],
            "full TAR historical Cargo projection differs")


def full_tar_observation(row, entry, tar, gzip, tools):
    require(row.get("rc") == entry.get("rc") == tar.get("rc") == gzip.get("rc") == 0,
            "full TAR missing successful observation")
    require(entry["argv"] == row["argv"] and row["submitted_at"] <= entry["start"] <= tar["start"]
            < tar["end"] <= gzip["start"] < gzip["end"] <= entry["end"] <= row["end"],
            "full TAR observation command/chronology differs")
    require(tar["real_executable"] in tools and gzip["real_executable"] in tools,
            "full TAR packaging tool differs")
    archive = Path(row["archive"]["path"])
    temporary = str(archive) + ".tmp.tar"
    require(row["output"] == str(archive) and tar["temporary_tar"] == temporary
            and tar["stage"] == str(archive.parent / "rmux-0.10.0-linux-x86_64")
            and tar["actual_argv"] == [tar["real_executable"]["path"], "--sort=name", "--format=ustar",
                 "--owner=0", "--group=0", "--numeric-owner", "-cf", temporary, "-C", str(archive.parent),
                 "rmux-0.10.0-linux-x86_64"]
            and gzip["actual_argv"] == [gzip["real_executable"]["path"], "-n", "-c", temporary]
            and tar["temporary_tar_after"] == gzip["temporary_tar_before"], "full TAR observed archive differs")
    require(all(call["environment"]["SOURCE_DATE_EPOCH"] == row["environment"]["SOURCE_DATE_EPOCH"]
                == "1790347419" for call in (tar, gzip)), "full TAR packaging epoch differs")


def full_tar_evidence(files, captured, container, parent, descriptor, binding, policy):
    receipt = loads(repro_reference(binding["receipt"]))
    freeze = loads(repro_reference(binding["freeze"]))
    metadata = loads(captured["share/rmux/artifact-metadata.json"])
    full_tar_file_closure(files, metadata, parent, descriptor, freeze, binding, policy)
    require(receipt["job_id"] == freeze["job_id"] == "L1-019" and receipt["pair"] == freeze["pair"] == 1
            and receipt["assemblies"] == 2 and freeze["clean"] is True
            and freeze["source_date_epoch"] == 1790347419, "full TAR freeze context differs")
    generated = [("metadata", "share/rmux/artifact-metadata.json", "expected_metadata_bytes"),
                 ("manifest", "share/rmux/package-layout.tsv", "expected_manifest"),
                 ("checksums", "SHA256SUMS.txt", "expected_checksums")]
    for _, path, key in generated:
        require(captured[path] == repro_reference(freeze[key]), "full TAR prospective bytes differ")
    expected_checksums = "".join(files[p]["sha256"] + "  " + p + "\n"
                                 for p in sorted(files) if p != "SHA256SUMS.txt").encode()
    require(captured["SHA256SUMS.txt"] == expected_checksums, "full TAR checksums differ")
    declaration = loads(repro_reference(freeze["declaration"]))
    provenance = metadata["payload_provenance"]
    require(declaration == dict(source=provenance["source"], tree=provenance["tree"], configuration="release",
                                files=[dict(role=f["role"], sha256=f["sha256"], bytes=files[f["path"]]["bytes"])
                                       for f in provenance["files"]]), "full TAR declaration differs")
    for item in freeze["files"]:
        repro_archive(item["source"])
        require(all(item[k] == item["source"][k] for k in ("bytes", "sha256")), "full TAR source file differs")
    require([r["label"] for r in receipt["runs"]] == ["A", "B"], "full TAR pair labels differ")
    names = {"ENTRY-CALL.json", "TAR-CALL.json", "GZIP-CALL.json", "STAGE.json",
             "artifact-metadata.json", "package-layout.tsv", "SHA256SUMS.txt"}
    observations, scripts = [], []
    for row, planned in zip(receipt["runs"], freeze["runs"]):
        capture = row["captures"]
        require(set(capture) == names and row["argv"] == planned["argv"] and row["output"] == planned["output"],
                "full TAR capture/frozen command differs")
        loaded = {name: repro_reference(ref) for name, ref in capture.items()}
        entry, tar, gzip = [loads(loaded[n]) for n in ("ENTRY-CALL.json", "TAR-CALL.json", "GZIP-CALL.json")]
        full_tar_observation(row, entry, tar, gzip, binding["historical_tools"])
        for name, path, _ in generated:
            basename = PurePosixPath(path).name
            require(loaded[basename] == captured[path] and tar[name + "_before"] == capture[basename],
                    "full TAR captured bytes differ")
        require(tar["stage_inventory"] == capture["STAGE.json"], "full TAR stage capture differs")
        stage = loads(loaded["STAGE.json"])
        require({f["path"]: {k: f[k] for k in ("path", "mode", "bytes", "sha256")}
                 for f in stage if f["type"] == "file"} == files, "full TAR captured stage files differ")
        require(all(f["mtime_ns"] == 1790347419 * 1000000000 for f in stage), "full TAR stage epoch differs")
        inventory = loads(repro_reference(row["inventory"]))
        require(all(inventory[k] == row["archive"][k] for k in ("path", "bytes", "sha256"))
                and inventory["gzip_header_hex"] == container["gzip_header_hex"], "full TAR captured archive differs")
        rows = [dict(path=f["name"].removeprefix(freeze["root_name"] + "/")
                     if f["name"] != freeze["root_name"] else ".",
                     **{k: f[k] for k in ("kind", "mode", "bytes", "uid", "gid", "uname", "gname", "mtime", "header_sha256")},
                     header_magic=f["magic_hex"]) for f in inventory["entries"]]
        require(rows == container["members"], "full TAR captured headers differ")
        script = dict(binding["packager_script"], path=str(Path(planned["view"]) / "scripts/package-unix.sh"))
        repro_reference(script)
        require(script["path"] in entry["argv"] and "--skip-build" in entry["argv"]
                and "--payload-provenance" in entry["argv"], "full TAR entry point differs")
        scripts.append(script)
        observations.append(dict(label=row["label"], entry=capture["ENTRY-CALL.json"], tar=capture["TAR-CALL.json"],
             gzip=capture["GZIP-CALL.json"], stage=capture["STAGE.json"], metadata=capture["artifact-metadata.json"],
             manifest=capture["package-layout.tsv"], checksums=capture["SHA256SUMS.txt"], inventory=row["inventory"]))
    ref = lambda item: {k: item[k] for k in ("path", "bytes", "sha256")}
    return dict(**{name: ref(files[path]) for name, path, _ in generated},
                **{"prospective_" + name: freeze[key] for name, _, key in generated},
                declaration=freeze["declaration"], historical_tools_receipt=freeze["tool_pins"],
                entry_scripts=scripts, observations=observations,
                source_files=[{k: f[k] for k in ("path", "source")} for f in freeze["files"]],
                generated_at_utc=metadata["generated_at_utc"], release_artifact=False, skip_build=True)


def reconstruct_repro(descriptor_path, inputs_path, inputs_sha256, subject):
    raw = Path(inputs_path).read_bytes()
    require(len(raw) <= 1024 * 1024 and digest(raw) == inputs_sha256, "input manifest digest mismatch")
    manifest = loads(raw)
    profile = manifest["profile"]
    require(manifest["schema"] == 1 and profile in REPRO_PROFILES, "reproduction input profile")
    kind, variant, old_profile = REPRO_PROFILES[profile]
    descriptor, policy, pins, schema = tool_policy(descriptor_path, profile)
    parent_ref = manifest["parent_sbom"]
    require(parent_ref["sha256"] == policy["parent_sbom_sha256"], "unproven reproduction parent")
    parent = loads(repro_reference(parent_ref))
    require(parent["profile"] == old_profile
            and parent["closure_sha256"] == digest(canonical(closure_fields(parent))),
            "reproduction parent closure")
    historical = manifest["historical_inputs"]
    repro_reference(historical)
    operation = {PROFILE: reconstruct, RPM_PROFILE: reconstruct_rpm, DEB_PROFILE: reconstruct_deb}[old_profile]
    current, _ = operation(descriptor_path, historical["path"], historical["sha256"],
                           manifest["historical_subject"])
    project = lambda d: {k: v for k, v in d.items() if k not in ("tool_pins", "closure_sha256")}
    require(project(parent) == project(current), "unproven historical Cargo closure")
    binding = repro_packaging(manifest, policy, kind)
    require(str(Path(subject)) in [r["path"] for r in binding["archives"]], "subject outside A/B pair")
    require(manifest["subject"] == dict(name=policy["subject_name"],
                                       bytes=binding["archives"][0]["bytes"],
                                       sha256=binding["archives"][0]["sha256"]), "reproduction subject differs")
    files, container = repro_container(subject, kind, descriptor, policy, parent)
    if kind == "rpm-full":
        metadata = container.pop("full_metadata")
        container["full_rpm_binding"] = full_rpm_evidence(files, metadata, parent, descriptor, binding, policy)
    elif kind == "deb-full":
        metadata, control = container.pop("full_metadata"), container.pop("full_control")
        container["full_deb_binding"] = full_deb_evidence(
            files, metadata, control, parent, descriptor, binding, policy, container["deb_binding"])
    elif kind == "tar-full":
        captured = container.pop("full_captured")
        container["full_tar_binding"] = full_tar_evidence(
            files, captured, container["tar_binding"], parent, descriptor, binding, policy)
    else:
        repro_file_closure(files, parent, descriptor, variant)
    core = loads((ROOT / policy["schema_path"]).read_bytes())["root"]["properties"]
    result = {k: copy.deepcopy(parent[k]) for k in core}
    result.update(profile=profile, subject=manifest["subject"], tool_pins=pins,
                  files=sorted(files.values(), key=lambda f: f["path"]), **container)
    result["recipe_evidence"] = [r for r in result["recipe_evidence"] if r["name"] in REQUIRED_INPUTS]
    for name, ref in [("repro_parent", parent_ref), ("packaging_receipt", binding["receipt"]),
                      ("packaging_freeze", binding["freeze"]), ("packaging_contract", binding["contract"])]:
        result["recipe_evidence"].append(dict(name=name, bytes=ref["bytes"], sha256=ref["sha256"]))
    result["recipe_evidence"].sort(key=lambda r: r["name"])
    binding.update(parent_sbom=parent_ref, parent_subject=parent["subject"],
                   parent_closure_sha256=parent["closure_sha256"],
                   cargo_closure_sha256=digest(canonical(cargo_projection(parent))),
                   historical_profile=old_profile, reuse_scope="complete-cargo-closure-and-identical-files-only")
    if kind in ("rpm-full", "deb-full", "tar-full"):
        binding["reuse_scope"] = "complete-cargo-closure-after-identical-binaries-only"
        binding["provenance_kind"] = "local-historical-build-and-full-packaging-not-ci-or-signed"
    result["repro_binding"] = binding
    require(cargo_projection(result) == cargo_projection(parent), "reproduction Cargo closure differs")
    result["closure_sha256"] = digest(canonical(closure_fields(result)))
    shape(result, schema["root"], schema["definitions"])
    for ref in binding["archives"]:
        repro_archive(ref)
    return result, schema


def validate_document(document, expected, schema):
    shape(document, schema["root"], schema["definitions"])
    require(document["closure_sha256"] == digest(canonical(closure_fields(document))), "closure digest mismatch")
    require(canonical(document) == canonical(expected), "SBOM differs from reconstructed pinned closure/subject")


def write_exclusive(path, data):
    path = Path(path)
    require(not path.exists(), "output already exists")
    temporary = path.with_name(path.name + ".partial")
    with temporary.open("xb") as stream:
        stream.write(data)
        stream.flush()
    # Hard-link publication is atomic and refuses replacing an existing result.
    import os
    os.link(temporary, path)
    temporary.unlink()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("generate", "validate"))
    parser.add_argument("--descriptor", required=True)
    parser.add_argument("--profile", default=PROFILE, choices=(PROFILE, RPM_PROFILE, DEB_PROFILE, *REPRO_PROFILES))
    parser.add_argument("--inputs", required=True)
    parser.add_argument("--inputs-sha256", required=True)
    parser.add_argument("--subject", required=True)
    parser.add_argument("--output")
    parser.add_argument("--sbom")
    args = parser.parse_args(argv)
    try:
        operation = (reconstruct_repro if args.profile in REPRO_PROFILES else
                     {PROFILE: reconstruct, RPM_PROFILE: reconstruct_rpm, DEB_PROFILE: reconstruct_deb}[args.profile])
        expected, schema = operation(args.descriptor, args.inputs, args.inputs_sha256, args.subject)
        require(expected["profile"] == args.profile, "requested profile differs from input")
        if args.operation == "generate":
            require(args.output is not None and args.sbom is None, "generate requires --output only")
            write_exclusive(args.output, canonical(expected))
            output = args.output
        else:
            require(args.sbom is not None and args.output is None, "validate requires --sbom only")
            content = Path(args.sbom).read_bytes()
            document = loads(content)
            require(content == canonical(document), "SBOM is not canonical JSON")
            validate_document(document, expected, schema)
            output = args.sbom
        print(json.dumps(dict(status="PASS", operation=args.operation, subject=expected["subject"],
                              sbom_sha256=file_hash(output), closure_sha256=expected["closure_sha256"],
                              files=len(expected["files"]), roots=len(expected["binary_roots"]),
                              packages=len(expected["packages"]), units=len(expected["build_units"]),
                              dependency_edges=sum(r["kind"] == "depends_on" for r in expected["relationships"])), sort_keys=True))
        return 0
    except (Error, layout.LayoutError, OSError, ValueError, KeyError, TypeError, IndexError, struct.error, tarfile.TarError) as error:
        print("sbom: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
