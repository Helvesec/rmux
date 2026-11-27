"""Frozen external projects, compiler witnesses and archive mutation controls."""

import io
import json
from pathlib import Path
import shutil
import tarfile

from installed_consumer_packages import REGISTRY, check_lock, file_hash, read_toml, unpack
from public_api_consumers import compiler_inputs
from public_api_policy import FACADES

FIXTURE = "tests/fixtures/installed-consumers"
MIGRATION = "crates/rmux-sdk/tests/fixtures/sdk-migration/src"


def freeze(runner):
    manifest = json.loads((runner.source / FIXTURE / "frozen.json").read_text())
    root = runner.output / "frozen"
    root.mkdir()
    for relative, digest in manifest["files"].items():
        source = runner.source / relative
        if file_hash(source) != digest:
            raise ValueError(f"frozen witness changed: {relative}")
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
    runner.receipt["fixtures"] = manifest
    runner.save()
    return root


def project(runner, frozen, facade):
    root = runner.output / ("sdk-consumer" if facade == "rmux-sdk" else "ratatui-consumer")
    src = root / "src"
    src.mkdir(parents=True)
    pins = {p["name"]: p["version"] for p in read_toml(runner.source / "Cargo.lock")["package"]}
    lines = ['[package]', f'name = "{root.name}"', 'version = "0.0.0"',
             'edition = "2021"', 'publish = false', '[workspace]', '[dependencies]']
    lines += [f'rmux-sdk = {{ version = "={pins["rmux-sdk"]}", default-features = false }}',
              f'tokio = {{ version = "={pins["tokio"]}", features = ["macros", "rt", "time"] }}']
    if facade == "rmux-sdk":
        for path in (frozen / MIGRATION).glob("*.rs"):
            shutil.copyfile(path, src / path.name)
        lines += [f'serde = {{ version = "={pins["serde"]}", features = ["derive"] }}',
                  f'regex = {{ version = "={pins["regex"]}", optional = true }}',
                  f'crossterm = {{ version = "={pins["crossterm"]}", default-features = false, features = ["events", "windows"], optional = true }}',
                  '[lib]', 'name = "sdk_migration_consumer"', '[features]',
                  'default = ["rmux-sdk/default", "web"]',
                  'web = ["rmux-sdk/web"]',
                  'regex = ["rmux-sdk/regex", "dep:regex"]',
                  'crossterm = ["rmux-sdk/crossterm", "dep:crossterm"]']
        main = "sdk.rs"
    else:
        lines += [f'ratatui-rmux = "={pins["ratatui-rmux"]}"',
                  f'ratatui-core = {{ version = "={pins["ratatui-core"]}", default-features = false }}']
        main = "ratatui.rs"
    (root / "Cargo.toml").write_text("\n".join(lines) + "\n")
    shutil.copyfile(frozen / FIXTURE / main, src / "main.rs")
    shutil.copyfile(frozen / FIXTURE / "scenario.rs", src / "scenario.rs")
    shutil.copyfile(runner.source / "Cargo.lock", root / "Cargo.lock")
    runner.run(root.name + "-resolve", [runner.cargo, "metadata", "--offline", "--format-version", "1"], root)
    check_lock(root / "Cargo.lock", runner.source / "Cargo.lock", runner.receipt["packages"], root.name)
    return root


def validate_resolution(runner, project, cell):
    metadata = json.loads(runner.run("resolve-" + runner.label(cell),
        [runner.cargo, "metadata", "--frozen", "--format-version", "1", *cell.flags], project))
    for package in metadata["packages"]:
        if package["name"] == project.name:
            continue
        if package["source"] != REGISTRY or not Path(package["manifest_path"]).is_relative_to(runner.vendor):
            raise ValueError(f"consumer escaped archive source: {package['id']}")
        for dep in package["dependencies"]:
            if dep.get("path"):
                raise ValueError("installed manifest contains a path override")
        original = runner.source_packages.get(package["name"])
        if original:
            keys = ("name", "req", "kind", "rename", "optional", "uses_default_features", "features", "target")
            def signature(item):
                return json.dumps({key: item[key] for key in keys}, sort_keys=True)
            if (package["features"] != original["features"] or
                    sorted(map(signature, package["dependencies"])) !=
                    sorted(map(signature, original["dependencies"]))):
                raise ValueError(f"packaging changed dependency metadata: {package['name']}")
    if metadata["workspace_root"] != str(project):
        raise ValueError("consumer is not an independent workspace")


def build_cell(runner, frozen, project, cell):
    label = runner.label(cell)
    validate_resolution(runner, project, cell)
    output = runner.run("build-" + label, [runner.cargo, "build", "--frozen", "-j", "2",
        "--target", runner.target, "--target-dir", runner.output / "consumer-target",
        "--message-format=json", *cell.flags], project)
    artifacts = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    sdk = next(a for a in artifacts if a.get("reason") == "compiler-artifact" and a["target"]["name"] == "rmux_sdk")
    if cell.package == "ratatui-rmux" or cell.mode == "default":
        expected = {"default", *FACADES["rmux-sdk"]["default"]}
    elif cell.mode == "all-features":
        expected = set(FACADES["rmux-sdk"])
    else:
        expected = set(cell.features)
    if set(sdk["features"]) != expected:
        raise ValueError(f"wrong installed SDK features: {sdk['features']} != {expected}")
    common = [runner.rustc, "--edition=2021", "--target", runner.target, "--emit=metadata"]
    names = [("rmux_sdk", "rmux_sdk")]
    if cell.package == "rmux-sdk":
        for boundary in ("identity-boundary", "semantic-conversion"):
            source = frozen / f"crates/rmux-sdk/tests/fixtures/{boundary}/supported.rs"
            runner.run(label + "-" + boundary, [*common, source, "--test",
                *compiler_inputs(output, names, ".rlib"), "-o", runner.output / (label + boundary + ".rmeta")])
        names += [(name, name) for name in ("tokio", "serde", "regex", "crossterm")
                  if name in {"tokio", "serde"} | expected]
        source = frozen / "tests/fixtures/public-api-gate/sdk-dependencies.rs"
        flags = ["--crate-type=lib"] + [arg for feature in expected for arg in ("--cfg", f'feature="{feature}"')]
    else:
        names += [("ratatui_rmux", "ratatui_rmux"), ("ratatui_core", "ratatui_core")]
        source = frozen / "tests/fixtures/public-api-gate/ratatui-consumer.rs"
        flags = []
    runner.run(label + "-public-dependencies", [*common, source, *flags,
        *compiler_inputs(output, names, ".rlib"), "-o", runner.output / (label + ".rmeta")])
    if "web" not in expected:
        absent = frozen / FIXTURE / "web-absent.rs"
        runner.run(label + "-web-absent", [*common, absent, "--error-format=json",
            *compiler_inputs(output, [("rmux_sdk", "rmux_sdk")], ".rlib"),
            "-o", runner.output / (label + "-absent.rmeta")],
            reject="rmux_sdk::WebShareHandle", diagnostic="E0432", diagnostic_source=absent)
    executable = next(a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact"
                      and a.get("executable") and a["target"]["name"] == project.name)
    binary = runner.output / "bin" / label
    shutil.copyfile(executable, binary)
    binary.chmod(0o700)
    runner.receipt["cells"].append({"id": cell.id, "sdk_features": sorted(expected),
        "binary": str(binary), "sha256": file_hash(binary), "lock_sha256": file_hash(project / "Cargo.lock")})
    runner.save()
    return binary


def controls(runner, projects):
    """Mutate only copied archives; fixture bytes and baseline locks stay fixed."""
    cases = (
        ("rmux-sdk", "removed-export", "src/lib.rs", b"pub use types::{", b"pub(crate) use types::{", "SessionName", "E0603", "src/lib.rs"),
        ("rmux-sdk", "missing-file", "src/snapshot/cell.rs", None, None, "cell", "E0583", "src/snapshot.rs"),
        ("ratatui-rmux", "removed-export", "src/lib.rs", b"pub use widget::PaneWidget;", b"", "ratatui_rmux::PaneWidget", "E0432", "src/main.rs"),
        ("ratatui-rmux", "missing-file", "src/widget.rs", None, None, "widget", "E0583", "src/lib.rs"),
    )
    for facade, mutation, member_path, marker, replacement, rejected, diagnostic, diagnostic_file in cases:
        package = next(p for p in runner.receipt["packages"] if p["name"] == facade)
        original = Path(package["archive"])
        project = projects[facade]
        label = facade + "-" + mutation
        root = runner.output / label
        root.mkdir()
        archive = root / original.name
        changed = 0
        with tarfile.open(original, "r:gz") as source, tarfile.open(archive, "w:gz") as target:
            for member in source.getmembers():
                data = source.extractfile(member).read()
                if member.name == f"{facade}-{package['version']}/{member_path}":
                    changed += 1
                    if marker is None:
                        continue
                    if data.count(marker) != 1:
                        raise ValueError("removed-export control location changed")
                    data = data.replace(marker, replacement)
                member.size = len(data)
                target.addfile(member, io.BytesIO(data))
        if changed != 1:
            raise ValueError("control must change exactly one archive member")
        vendor = root / "vendor"
        vendor.mkdir()
        for path in runner.vendor.iterdir():
            if path.name != f"{package['name']}-{package['version']}":
                (vendor / path.name).symlink_to(path, target_is_directory=True)
        unpack(archive, vendor, package["name"], package["version"])
        consumer = root / project.name
        shutil.copytree(project, consumer)
        # Only the mutant's copied lock records its new archive checksum.
        lock = consumer / "Cargo.lock"
        lock.write_text(lock.read_text().replace(package["sha256"], file_hash(archive)))
        config = ['--config', 'source.installed.directory=' + json.dumps(str(vendor))]
        name = consumer.name if mutation == "removed-export" else facade
        package_id = runner.run(label + "-package-id", [runner.cargo, "pkgid", "--frozen",
            "-p", name, *config], consumer).strip()
        source = consumer if mutation == "removed-export" else vendor / f"{facade}-{package['version']}"
        runner.run(label, [runner.cargo, "check", "--frozen", "-j", "2",
            "--target", runner.target, "--target-dir", root / "target",
            "--message-format=json", *config], consumer,
            reject=rejected, diagnostic=diagnostic, diagnostic_source=source / diagnostic_file,
            diagnostic_package=package_id)
        runner.receipt["controls"].append({"id": label, "archive": str(archive),
            "sha256": file_hash(archive), "baseline_archive_sha256": file_hash(original)})
        runner.save()
