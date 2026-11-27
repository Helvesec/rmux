#!/usr/bin/env python3
"""Verify local copies of an RC using existing signed release evidence and SBOMs.

This read-only gate proves supplied bytes, not their network acquisition or the
native execution of their payloads. Missing or unsupported evidence is an error.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

from release_evidence import file_hash, publishable_assets, validate_candidate_reference, validate_file
from release_publish_security import verify_promotion_attestation
from release_sbom import local_main, verify_sboms
from strict_json import read_json_object


def publisher_module():
    spec = importlib.util.spec_from_file_location(
        "release_publisher", Path(__file__).with_name("publish-github-release.py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def mapped_paths(values):
    result = {}
    for value in values or []:
        name, separator, path = value.partition("=")
        if not separator or not name or name in result or Path(name).name != name or not path:
            raise ValueError("SBOM mapping must be unique NAME=PATH")
        result[name] = Path(path)
    return result


def verify_files(root: Path, records):
    if root.is_symlink() or not root.is_dir():
        raise ValueError("release assets must be a real directory")
    if {p.name for p in root.iterdir()} != {r["name"] for r in records}:
        raise ValueError("release asset names differ from signed envelope")
    for record in records:
        path = validate_file(root / record["name"], "release asset")
        if path.stat().st_size != record["size"] or file_hash(path) != record["sha256"]:
            raise ValueError("release asset bytes differ: " + record["name"])


def verify(args):
    if args.cutoff_output.exists() or args.cutoff_output.is_symlink():
        raise ValueError("refusing to overwrite cutoff evidence")
    publisher = publisher_module()
    manifest_path = validate_file(args.manifest, "candidate manifest")
    manifest = read_json_object(manifest_path, "candidate manifest")
    predicate_path = validate_file(args.predicate, "promotion predicate")
    predicate = read_json_object(predicate_path, "promotion predicate")
    envelope = read_json_object(validate_file(args.envelope, "promotion envelope"), "promotion envelope")
    publisher.validate_authorization(predicate, envelope, file_hash(predicate_path), simulation=False)
    reference = {k: v for k, v in predicate["candidate"].items()
                 if k not in {"manifest_created_at", "manifest_expires_at"}}
    validate_candidate_reference(reference, manifest_path, manifest)
    if any(predicate["candidate"]["manifest_" + k] != manifest[k]
           for k in ("created_at", "expires_at")):
        raise ValueError("manifest dates differ from signed predicate")
    if (predicate["source_git_sha"] != manifest["source_git_sha"]
            or predicate["release"]["ref"] != manifest["planned_release_ref"]
            or predicate["release_policy_sha256"] != manifest["release_policy"]["sha256"]):
        raise ValueError("manifest identity differs from signed predicate")
    records = publisher.expected_asset_records(predicate, envelope)
    verify_files(args.assets_dir, records)
    verify_files(args.redownloads_dir, records)
    for record in records:
        if (args.assets_dir / record["name"]).samefile(args.redownloads_dir / record["name"]):
            raise ValueError("download copies must be separate files")
    authorized = publishable_assets(manifest, args.assets_dir / "SHA256SUMS")
    if authorized != predicate["assets"]:
        raise ValueError("manifest names/roles differ from signed predicate")
    attested = {r["name"]: args.assets_dir / r["name"]
                for r in predicate["assets"] if r["role"] != "checksums"}
    verify_promotion_attestation(
        gh=args.gh, assets=attested, bundle=args.assets_dir / "SHA256SUMS.sigstore.json",
        predicate=predicate, source_git_sha=manifest["source_git_sha"],
        release_ref=manifest["planned_release_ref"],
    )
    def git(*arguments):
        return subprocess.run(["git", "-C", str(args.source_dir), *arguments],
                              check=True, capture_output=True, text=True).stdout.strip()
    if git("rev-parse", "HEAD") != manifest["source_git_sha"] or git("status", "--porcelain", "--untracked-files=no"):
        raise ValueError("SBOM source must be the clean exact candidate checkout")
    count = verify_sboms(authorized, args.assets_dir, mapped_paths(args.sbom),
                        mapped_paths(args.sbom_inputs), mapped_paths(args.sbom_descriptor),
                        args.source_dir, manifest["source_git_sha"], git("rev-parse", "HEAD^{tree}"), args.cargo)
    payloads = [args.redownloads_dir / r["name"] for r in authorized
                if r["role"] in {"archive", "debian", "rpm", "snap-amd64", "snap-arm64"}]
    cutoff = Path(__file__).resolve().parents[1] / "bench" / "cutoff_assets.py"
    result = subprocess.run([sys.executable, str(cutoff), *map(str, payloads),
                             "--output", str(args.cutoff_output)], check=False)
    if result.returncode != 0:
        raise ValueError("release instrumentation cutoff failed or container unsupported")
    verify_files(args.assets_dir, records)
    verify_files(args.redownloads_dir, records)
    return dict(status="VERIFIED_LOCAL_FILES", assets=len(records), sboms=count,
                source_git_sha=manifest["source_git_sha"],
                acquisition="not-proven-by-local-copy-checks", native_execution="not-performed")


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "predicate", "envelope", "assets-dir", "redownloads-dir",
                 "source-dir", "gh", "cargo", "cutoff-output"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("sbom", "sbom-inputs", "sbom-descriptor"):
        parser.add_argument("--" + name, action="append", metavar="NAME=PATH", required=True)
    return parser.parse_args()


if __name__ == "__main__":
    if sys.argv[1:2] == ["local-sboms"]:
        raise SystemExit(local_main(["verify-local", *sys.argv[2:]]))
    try:
        print(json.dumps(verify(parse_args()), sort_keys=True))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print("verify-release: " + str(error), file=sys.stderr)
        raise SystemExit(1) from error
