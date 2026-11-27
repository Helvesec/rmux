"""Bind qualification accounting to the existing signed final-artifact smokes.

Recipe schema 2 adds final inputs and retained smoke output. The release input
and smoke result schemas remain unchanged. These checks prove accounting and
byte bindings, never the authenticity of a native execution.
"""
import hashlib
from pathlib import Path, PurePosixPath, PureWindowsPath
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent / "release"))
import final_artifact_smoke as smoke


SCENARIO = "final-artifact-smokes/v1"
PLATFORMS = {"linux": "linux-x86_64", "macos": "macos-aarch64",
             "windows11": "windows-x86_64"}
BINDING_FIELDS = ("source_sha", "run_id", "challenge_id", "execution_id")


def require(condition, message):
    if not condition:
        raise ValueError("final artifact " + message)


def file_hash(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def scenario():
    return {"id": SCENARIO, "command": ["python", "scripts/release/final_artifact_smoke.py",
            "run", "--manifest", "{final_manifest}", "--output", "{final_output}"],
            "tests": False, "native_os": list(PLATFORMS)}


def read_inputs(path, head):
    require(path is not None, "signed manifest required for Q1/Q2")
    path = smoke.canonical_file(path)
    signature = smoke.canonical_file(str(path) + ".sig")
    before = (file_hash(path), file_hash(signature))
    value, signer = smoke.manifest(path, head=head)
    for row in value["platforms"].values():
        require(all(row["scenarios"].values()), "native scenario pending")
        # All archives and drivers must accompany the signed input bundle.
        smoke.pinned(path.parent, row["archive"])
        for cell in row["scenarios"].values():
            smoke.pinned(path.parent, cell["driver"])
    require(before == (file_hash(path), file_hash(signature)), "inputs changed during verification")
    return value, {"manifest_sha256": before[0], "signature_sha256": before[1], "signer": signer}


def bind_inputs(path, head):
    return read_inputs(path, head)[1]


def require_current_recipe(manifest, synthetic):
    if not synthetic and manifest["qualification_id"] in {"Q1", "Q2"}:
        require(manifest["schema"] == 2, "Q1/Q2 require recipe schema 2; legacy accounting is incomplete")
    if manifest["schema"] == 2:
        require(not synthetic, "native smoke recipe cannot run as a synthetic fixture")


def validate_recipe(manifest):
    require(manifest["qualification_id"] in {"Q1", "Q2"}, "recipe qualification identity")
    require(manifest["cells"] == [{"id": name, "native_os": name} for name in PLATFORMS],
            "recipe requires all three native OS cells")
    rows = [s for s in manifest["scenarios"] if s["id"] == SCENARIO]
    require(rows == [scenario()], "recipe smoke scenario missing or changed")
    binding = manifest.get("final_artifacts", {})
    require(set(binding) == {"manifest_sha256", "signature_sha256", "signer"}, "input binding fields")
    for name in ("manifest_sha256", "signature_sha256"):
        require(isinstance(binding[name], str) and len(binding[name]) == 64 and
                all(c in "0123456789abcdef" for c in binding[name]), "invalid input hash")
    require(isinstance(binding["signer"], str) and binding["signer"], "missing signer")


def command_arguments(command, manifest_path, output):
    replacements = {"{final_manifest}": str(Path(manifest_path).absolute()),
                    "{final_output}": str(Path(output).absolute())}
    return [replacements.get(arg, arg) for arg in command]


def output_files(output):
    require(output.is_dir() and output.resolve() == output, "redirected output directory")
    files = {}
    for path in sorted(output.rglob("*")):
        require(not path.is_symlink(), "redirected output entry")
        if path.is_file():
            files[path.relative_to(output).as_posix()] = file_hash(path)
        else:
            require(path.is_dir(), "nonregular output entry")
    return files


def inspect_output(manifest, receipt, output, manifest_path):
    """Check a copied native output without executing its binaries or drivers."""
    value, binding = read_inputs(manifest_path, manifest["source_sha"])
    require(binding == manifest["final_artifacts"], "signed input binding changed")
    key = PLATFORMS[receipt["native_os"]]
    row = value["platforms"][key]
    result = smoke.read_json_object(smoke.canonical_file(output / "result.json"), "final smoke result")
    require(type(result["schema"]) is int and result["schema"] == 1 and
            result["subject"] == "final-artifact" and result["status"] == "PASS" and
            result["product_qualification"] is False, "result is not final native accounting")
    require(result["source_sha"] == manifest["source_sha"] and result["platform"] == key and
            result["manifest_sha256"] == binding["manifest_sha256"] and
            result["signer"] == binding["signer"] and result["archive"] == row["archive"],
            "result input identity differs")
    require(set(result["cells"]) == set(smoke.roster(key)), "missing or unexpected final smoke cell")
    smoke.canonical_file(output / "installed.log")
    files = smoke.archive_files(Path(manifest_path).absolute().parent, row)
    actual = smoke.check_package(output / row["package_root"], files, row, manifest["source_sha"], key)
    binaries = result["binaries"]
    require(set(binaries) == set(actual), "binary roster differs")
    path_type = PureWindowsPath if key == "windows-x86_64" else PurePosixPath
    roots = set()
    for name, record in binaries.items():
        relative = Path(actual[name]["path"]).relative_to(output / row["package_root"])
        original = path_type(record["path"])
        require(original.is_absolute() and ".." not in original.parts and
                original.parts[-len(relative.parts):] == relative.parts and
                record["commit"] == actual[name]["commit"] and
                record["sha256"] == actual[name]["sha256"], "binary path/hash/commit differs")
        roots.add(original.parents[len(relative.parts) - 1])
    require(len(roots) == 1, "mixed extracted packages")
    original_package = roots.pop()
    require(original_package.name == row["package_root"], "extracted package name differs")
    original_output = original_package.parent
    for name, cell in result["cells"].items():
        smoke.canonical_file(output / (name + ".log"))
        selected = row["scenarios"][name]
        require(cell["status"] == "PASS" and cell["driver"] == selected["driver"] and
                cell["tool"] == selected["tool"], "cell driver/tool/status differs")
        proof_path = smoke.canonical_file(output / (name + ".json"))
        require(file_hash(proof_path) == cell["evidence_sha256"], "cell evidence bytes differ")
        proof = smoke.read_json_object(proof_path, "native scenario receipt")
        require(proof.get("scenario") == name and proof.get("status") == "PASS" and
                proof.get("subject") == "final-artifact" and
                proof.get("source_sha") == manifest["source_sha"] and proof.get("binaries") == binaries,
                "cell evidence identity differs")
        # Invocation paths remain those of the native host, including on a
        # collector with another OS. The bundle location comes from its argv.
        native_manifest = path_type(receipt["argv"][4])
        replacements = {"{tool}": str(path_type(selected["tool"]["path"])),
                        "{driver}": str(native_manifest.parent / selected["driver"]["path"]),
                        "{rmux}": binaries["cli"]["path"],
                        "{receipt}": str(original_output / (name + ".json"))}
        require(cell["argv"] == [replacements.get(a, a) for a in selected["argv"]], "cell argv differs")
    require(str(original_output) == receipt["argv"][6], "result came from another execution directory")
    return len(result["cells"])


def seal_output(manifest, receipt, output, manifest_path):
    files = output_files(output)
    count = inspect_output(manifest, receipt, output, manifest_path)
    require(files == output_files(output), "output changed during verification")
    return {"schema": 1, "qualification_id": manifest["qualification_id"],
            **{name: receipt[name] for name in BINDING_FIELDS},
            "inputs": manifest["final_artifacts"], "cells": count, "files": files}


def check_output(manifest, receipt, output, manifest_path):
    require(receipt.get("final_artifacts") == seal_output(manifest, receipt, output, manifest_path),
            "retained execution binding differs (including qualification/challenge)")
    return receipt["final_artifacts"]["cells"]
