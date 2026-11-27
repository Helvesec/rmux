"""Frozen downstream compiler checks supplement cargo-semver-checks' lint coverage."""

import json
from pathlib import Path
import shutil

from toml_reader import loads
from public_api_policy import FACADES

FIXTURES = Path(__file__).resolve().parents[1] / "tests/fixtures/public-api-gate"


def compiler_inputs(output, names, extension):
    artifacts = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    command = []
    for name, alias in names:
        matches = [a for a in artifacts if a.get("reason") == "compiler-artifact"
                   and a["target"]["name"] == name]
        libraries = [Path(f) for a in matches for f in a["filenames"] if f.endswith(extension)]
        if len(libraries) != 1:
            raise ValueError(f"missing or ambiguous {name} artifact")
        command += ["--extern", f"{alias}={libraries[0]}"]
    # Explicit --target separates host proc macros from target libraries even on
    # a native build. Preserve both compiler-reported search directories.
    directories = sorted({str(Path(f).parent) for a in artifacts
                          if a.get("reason") == "compiler-artifact" for f in a["filenames"]})
    return command + [arg for directory in directories for arg in ("-L", f"dependency={directory}")]


def registry_packages(lock):
    return {(p["name"], p["version"], p["source"], p.get("checksum"))
            for p in loads(Path(lock).read_text())["package"] if "source" in p}


def migration_project(runner, baseline, source):
    project = runner.output / "sdk-consumer"
    fixture = baseline / "crates/rmux-sdk/tests/fixtures/sdk-migration"
    shutil.copytree(fixture, project)
    manifest = project / "Cargo.toml"
    original = manifest.read_text()
    marker = 'path = "../../.."'
    if original.count(marker) != 1:
        raise ValueError("frozen SDK consumer path needs adaptation")
    manifest.write_text(original.replace(marker, "path = " + json.dumps(str(source / "crates/rmux-sdk"))))
    # Start from candidate pins. Offline Cargo may only normalize workspace/path
    # packages for this standalone consumer, never choose new registry versions.
    shutil.copyfile(source / "Cargo.lock", project / "Cargo.lock")
    runner.run("consumer-resolve", [runner.cargo, "metadata", "--offline", "--format-version", "1",
                                    "--manifest-path", manifest], project)
    if not registry_packages(project / "Cargo.lock") <= registry_packages(source / "Cargo.lock"):
        raise ValueError("consumer resolution changed candidate registry pins")
    return project


def check_consumer(runner, baseline, source, cell, prefix=""):
    label = prefix + "consumer-" + cell.id.replace("/", "-")
    target = runner.output / (prefix + "consumer-target")
    if cell.package == "rmux-sdk":
        project = runner.output / "sdk-consumer"
        if not project.exists():
            project = migration_project(runner, baseline, source)
        flags = cell.flags
        expected = set(cell.features)
        if cell.mode in ("default", "all-features"):
            # The frozen migration manifest disables dependency defaults and
            # forwards web itself. Enable SDK's literal `default` cfg as well.
            flags = [*flags, "--features", "rmux-sdk/default"]
            expected = (set(FACADES["rmux-sdk"]) if cell.mode == "all-features"
                        else {"default", *FACADES["rmux-sdk"]["default"]})
        output = runner.run(label, [runner.cargo, "check", "--frozen", "--lib", "-j", "2",
                          "--manifest-path", project / "Cargo.toml", "--target", runner.target,
                          "--target-dir", target, *flags, "--message-format=json"], project)
        artifacts = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
        features = next(a["features"] for a in artifacts if a.get("reason") == "compiler-artifact"
                        and a["target"]["name"] == "rmux_sdk")
        if set(features) != expected:
            raise ValueError(f"consumer compiled wrong SDK features: {features}, expected {expected}")
        common = [runner.rustc, "--edition", "2021", "--target", runner.target, "--emit=metadata"]
        sdk = compiler_inputs(output, [("rmux_sdk", "rmux_sdk")], ".rmeta")
        for name in ("identity-boundary", "semantic-conversion"):
            frozen = baseline / "crates/rmux-sdk/tests/fixtures" / name / "supported.rs"
            runner.run(label + "-" + name, [*common, frozen, "--test", *sdk,
                       "-o", runner.output / (label + "-" + name + ".rmeta")])
        names = [(name, name) for name in ("rmux_sdk", "tokio", "serde")]
        names += [(name, name) for name in ("regex", "crossterm") if name in features]
        cfg = [arg for feature in features for arg in ("--cfg", f'feature="{feature}"')]
        runner.run(label + "-dependencies", [*common, FIXTURES / "sdk-dependencies.rs",
                   "--crate-type=lib", *compiler_inputs(output, names, ".rmeta"), *cfg,
                   "-o", runner.output / (label + "-dependencies.rmeta")])
        return
    output = runner.run(label + "-build", [runner.cargo, "build", "--frozen", "--lib", "-j", "2",
                        "--manifest-path", source / "Cargo.toml", "-p", cell.package,
                        "--target", runner.target, "--target-dir", target, *cell.flags,
                        "--message-format=json"], source)
    command = [runner.rustc, FIXTURES / "ratatui-consumer.rs",
               "--edition", "2021", "--target", runner.target, "--emit=metadata",
               "-o", runner.output / (label + ".rmeta")]
    command += compiler_inputs(output, [("ratatui_rmux", "ratatui_rmux"), ("rmux_sdk", "rmux_sdk"),
                                        ("ratatui_core", "ratatui_core")], ".rlib")
    runner.run(label, command)
