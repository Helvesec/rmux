"""Qualification commands selected from the candidate, before any receipt exists."""
from itertools import combinations
import json
from pathlib import Path
import subprocess

from soak_profile import soak_profile


CELLS = [{"id": name, "native_os": name} for name in ("linux", "macos", "windows11")]

# The #[ignore]d Darwin PTY allocation salvos contend on the kernel-global
# allocator and inject errno -6 into every other process opening PTYs on the
# host, so --include-ignored must never replay them inside a qualification.
# They run alone on a quiet host (see crates/rmux-pty/src/backend/macos_tests.rs).
HOST_DISRUPTIVE_TESTS = ["--skip", "backend::macos::tests::"]


def default_manifest(root, source_sha, qualification_id="Q0", *, final_artifact_manifest=None):
    final = None
    if qualification_id in {"Q1", "Q2"}:
        from qualification_final_smokes import bind_inputs
        final = bind_inputs(final_artifact_manifest, source_sha)
    elif final_artifact_manifest is not None:
        raise ValueError("final artifact inputs require Q1 or Q2")
    raw = subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps",
                                   "--format-version", "1"], cwd=root, text=True)
    metadata = json.loads(raw)
    members = set(metadata["workspace_members"])
    packages = sorted((p for p in metadata["packages"] if p["id"] in members),
                      key=lambda p: p["name"])
    if len(packages) != len(members) or not packages:
        raise ValueError("workspace membership is incomplete")
    scenarios = []

    def add(name, command, tests=False, native_os=None):
        scenarios.append({"id": name, "command": command, "tests": tests,
                          "native_os": native_os or [c["native_os"] for c in CELLS]})

    add("fmt", ["cargo", "fmt", "--all", "--", "--check"])
    add("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--locked",
                   "--", "-D", "warnings"])
    add("hygiene", ["bash", "scripts/check-worktree-hygiene.sh"])
    add("release-docs", ["python", "scripts/release_docs_allowlist.py"])
    add("regressions", ["python", "scripts/regression_registry.py", "--platform", "{platform}"])
    for package in packages:
        features = sorted(set(package["features"]) - {"default"})
        selections = [("default", []), ("all", ["--all-features"])]
        for count in range(len(features) + 1):
            for subset in combinations(features, count):
                selections.append(("subset:" + (",".join(subset) or "none"),
                                   ["--no-default-features"] +
                                   (["--features", ",".join(subset)] if subset else [])))
        for name, flags in selections:
            add("powerset/" + package["name"] + "/" + name,
                ["cargo", "check", "-p", package["name"], "--all-targets", "--locked", *flags])
    add("workspace", ["cargo", "test", "--workspace", "--all-targets", "--locked",
                      "--", "--include-ignored", *HOST_DISRUPTIVE_TESTS,
                      "--show-output", "--test-threads=1"], True)
    add("doctests", ["cargo", "test", "--workspace", "--doc", "--locked",
                     "--", "--include-ignored", "--show-output", "--test-threads=1"], True)
    add("scoreboard", ["cargo", "test", "--locked", "--test", "tmux_compat_surface_matrix",
                       "--", "--include-ignored", "--show-output", "--test-threads=1"], True, ["linux", "macos"])
    add("scoreboard/windows11", ["cargo", "test", "--locked", "--test", "windows_terminal_matrix",
                                  "--", "--include-ignored", "--show-output", "--test-threads=1"],
        True, ["windows11"])
    profile = soak_profile("brief" if qualification_id == "Q0" else "long")
    add("soak/v1", ["python", "scripts/soak.py", "--mode", profile["mode"],
                    "--duration", str(profile["duration_seconds"])])
    scenarios[-1]["profile"] = profile
    manifest = {"schema": 1, "source_sha": source_sha, "qualification_id": qualification_id,
                "scenarios": scenarios, "cells": CELLS}
    if final is not None:
        from qualification_final_smokes import scenario
        scenarios.append(scenario())
        # Schema 1 remains the historical gates/soak accounting contract.
        manifest.update(schema=2, final_artifacts=final)
    return manifest


def applicable(scenario, cell):
    return cell["native_os"] in scenario.get("native_os", [c["native_os"] for c in CELLS])


def expected_matrix(manifest):
    return [{"cell_id": c["id"], "scenario_id": s["id"]}
            for c in manifest["cells"] for s in manifest["scenarios"] if applicable(s, c)]


def launch_records(target, before):
    directory = Path(target) / "identity-launches"
    paths = set(directory.glob("*.json")) - before
    return [json.loads(path.read_text()) for path in sorted(paths)]
