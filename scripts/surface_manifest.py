#!/usr/bin/env python3
"""Export/check the global Linux source surface projection (Python >= 3.9).

--check validates the owning registries. --check --manifest FILE additionally
checks a retained export, including exact metadata and conformance references.
The export is generated data, never an independent authority or native proof.
"""
import argparse
import json
from pathlib import Path
import sys

from surface_sources import READERS, Sources
from surface_conformance import python_cases, rust_cases
from surface_syntax import require


ROOT = Path(__file__).resolve().parents[1]
FAMILIES = {reader.__name__ for reader in READERS}
CLASSES = {"stable", "post-1.0", "absent"}


def conformance(sources, reference):
    require(isinstance(reference, dict) and set(reference) == {"path", "case"},
            "invalid conformance reference")
    path, name = reference["path"], reference["case"]
    require(isinstance(path, str) and isinstance(name, str) and name,
            "invalid conformance case")
    source = sources.read(path)
    if path.endswith(".rs"):
        cases = rust_cases(sources.rust(path))
    elif path.endswith(".py"):
        cases = python_cases(source)
    else:
        raise ValueError("unsupported conformance source: " + path)
    require(cases.count(name) == 1, "orphan or ambiguous conformance case: " + path + "::" + name)


def validate(manifest, sources):
    require(isinstance(manifest, dict) and manifest.get("schema") == 1
            and manifest.get("platform") == "linux", "manifest schema/platform")
    entries = manifest.get("entries")
    require(isinstance(entries, list) and entries, "empty surface manifest")
    identities, families, cases = [], set(), set()
    for record in entries:
        require(isinstance(record, dict) and set(record) == {
            "family", "id", "classification", "sources", "metadata", "conformance"},
                "unclassified surface record")
        family, name = record["family"], record["id"]
        require(isinstance(family, str) and family in FAMILIES and isinstance(name, str) and name,
                "unclassified surface family/id")
        key = (family, name)
        require(key not in identities, "duplicate surface: " + family + ":" + name)
        identities.append(key)
        families.add(family)
        require(record["classification"] in CLASSES, "unclassified surface: " + family + ":" + name)
        paths = record["sources"]
        require(isinstance(paths, list) and paths and len(paths) == len(set(paths)),
                "orphan or duplicate source: " + name)
        for path in paths:
            require(isinstance(path, str), "invalid source path")
            # Binary fixture bytes are hashed by the wire reader.
            if path not in sources.inputs:
                sources.binary(path)
        references = record["conformance"]
        require(isinstance(references, list), "invalid conformance cases: " + name)
        require(record["classification"] != "stable" or references,
                "stable surface without conformance case: " + name)
        seen = set()
        for reference in references:
            serialized = canonical(reference)
            require(serialized not in seen, "duplicate conformance case: " + name)
            seen.add(serialized)
            if serialized not in cases:
                conformance(sources, reference)
                cases.add(serialized)
    require(families == FAMILIES, "orphan surface family")
    require(identities == sorted(identities), "surface order is not deterministic")


def canonical(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, indent=2, allow_nan=False) + "\n"


def project(root=ROOT):
    sources = Sources(root)
    for name in ("surface_manifest.py", "surface_sources.py", "surface_syntax.py",
                 "surface_conformance.py", "toml_reader.py"):
        sources.read("scripts/" + name)
    entries = []
    for reader in READERS:
        entries.extend(reader(sources))
    entries.sort(key=lambda e: (e["family"], e["id"]))
    manifest = dict(schema=1, platform="linux", scope="registered source contracts across cfg/features; not execution or release proof",
                    classification_policy={
                        "stable": "current registry declarations with named conformance cases; not a final 1.0 freeze",
                        "post-1.0": "crate API outside the existing facade policy; binary package layouts are separate",
                        "absent": "reserved wire identities and retired capability identities"},
                    entries=entries)
    validate(manifest, sources)
    manifest["inputs"] = dict(sorted(sources.inputs.items()))
    return manifest


def pairs(items):
    value = {}
    for key, item in items:
        require(key not in value, "duplicate JSON key: " + key)
        value[key] = item
    return value


def check(manifest, root=ROOT):
    sources = Sources(root)
    validate(manifest, sources)
    expected = project(root)
    observed = {(r["family"], r["id"]) for r in manifest["entries"]}
    required = {(r["family"], r["id"]) for r in expected["entries"]}
    require(observed == required, "orphan or missing surface in projection")
    require(manifest == expected, "stale surface metadata, classification, conformance or inputs")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--export", action="store_true")
    mode.add_argument("--check", action="store_true")
    parser.add_argument("--manifest", type=Path, help="retained export to check; never used as registry input")
    args = parser.parse_args(argv)
    if args.manifest and not args.check:
        parser.error("--manifest requires --check")
    try:
        if args.manifest:
            manifest = json.loads(args.manifest.read_text(encoding="utf-8"), object_pairs_hook=pairs)
            check(manifest)
        else:
            manifest = project()
        if args.export:
            print(canonical(manifest), end="")
        else:
            print(f"surface manifest: {len(manifest['entries'])} classified identities in eight families")
        return 0
    except (ValueError, OSError, KeyError, TypeError, StopIteration, IndexError) as error:
        print("surface manifest: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
