#!/usr/bin/env python3
"""Focused native cfg regression. Same source/tool/output arguments as the gate."""

import json
from pathlib import Path
import shutil
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from public_api_gate import Runner, baseline_source, digest, parser, source_digest, tree_digest
from public_api_policy import Cell, cells
from public_api_consumers import check_consumer, migration_project, registry_packages
from public_api_gate_canaries import FIXTURES, consumer, demand, tiny


def reject(runner, action):
    count = len(runner.commands)
    try:
        action()
    except ValueError as error:
        demand("unsupported Cargo configuration" in str(error), str(error))
        demand(len(runner.commands) == count + 1, "missing rejection receipt")
        demand(runner.commands[-1]["exit"] is None, "rejection executed a tool")
    else:
        raise RuntimeError("configured invocation was accepted")


def configure(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def removal(runner):
    frozen = (FIXTURES / "baseline.rs").read_bytes()
    marker = b"pub fn combination_only"
    demand(frozen.count(marker) == 1, "expected exactly one combination_only export")
    mutant = frozen.replace(marker, b"fn combination_only", 1)
    manifest = '''[package]
name = "rmux-sdk"
version = "1.0.0"
edition = "2021"
[workspace]
[features]
default = ["web"]
web = []
regex = []
crossterm = []
'''
    roots = []
    for side, data in (("frozen", frozen), ("mutant", mutant)):
        root = runner.output / side
        (root / "src").mkdir(parents=True)
        (root / "src/lib.rs").write_bytes(data)
        (root / "Cargo.toml").write_text(manifest if side == "frozen" else
                                         manifest.replace('"1.0.0"', '"99.0.0"'))
        runner.run(side + "-lock", [runner.cargo, "generate-lockfile", "--offline"], root)
        roots.append(root)
    old_root, new_root = roots
    before = tree_digest(old_root)
    demand(digest(old_root / "src/lib.rs") == digest(FIXTURES / "baseline.rs"), "frozen bytes differ")
    cell = Cell("rmux-sdk", "no-default", ("regex", "web"))
    old = runner.document(old_root, cell, "clean-old")
    new = runner.document(new_root, cell, "clean-new")
    message = runner.compare(old, new, "clean-removal", expected=1)
    demand("function_missing" in message and runner.commands[-1]["exit"] == 100, message)
    runner.compare(old, new, "major-control", release_type="major")
    # Direct rustc witnesses retain the two discriminating feature selections.
    for index, features in ((6, ("regex", "web")), (7, ("crossterm", "regex", "web"))):
        a = tiny(runner, frozen.decode("utf-8"), f"combination-{index}-old", features=features)
        b = tiny(runner, mutant.decode("utf-8"), f"combination-{index}-new", "99.0.0", features)
        runner.compare(a[0], b[0], f"combination-{index}-semver", expected=int(index == 6))
        consumer(runner, a, f"combination-{index}-old-consumer")
        consumer(runner, b, f"combination-{index}-consumer", expected=int(index == 6),
                 diagnostic="E0603" if index == 6 else None)
    config = runner.output / ".cargo/config.toml"
    configure(config, '[build]\nrustdocflags = ["--cfg", \'feature="crossterm"\']\n')
    try:
        try:
            configured_old = runner.document(old_root, cell, "configured-old")
        except ValueError as error:
            demand("unsupported Cargo configuration" in str(error), str(error))
            demand(runner.commands[-1]["exit"] is None, "compiler ran during rejection")
        else:
            # On the uncorrected Runner retain the real false-green reproduction.
            configured_new = runner.document(new_root, cell, "configured-new")
            runner.compare(configured_old, configured_new, "configured-removal")
            raise RuntimeError("API-GATE-CFG-01: configured removal became green")
    finally:
        demand(tree_digest(old_root) == before, "frozen source changed")
        (runner.output / "frozen-digest.json").write_text(json.dumps({
            "before": before, "after": tree_digest(old_root),
            "fixture": digest(FIXTURES / "baseline.rs")}, indent=2))


def boundaries(runner):
    root = runner.output / "source"
    (root / "src").mkdir(parents=True)
    (root / "Cargo.toml").write_text('[package]\nname="cfg-boundary"\nversion="1.0.0"\nedition="2021"\n[workspace]\n')
    (root / "src/lib.rs").write_text("pub fn retained() {}\n")
    runner.run("lock", [runner.cargo, "generate-lockfile", "--offline"], root)
    config_texts = [
        '[build]\nrustflags=["--cfg", \'feature="web"\']\n',
        '[build]\nrustdocflags=["--cfg", \'feature="web"\']\n',
        '[target.\'cfg(unix)\']\nrustflags=["--cfg=hidden"]\n',
        '[target.x86_64-pc-windows-msvc]\nrustdocflags=["--cfg=hidden"]\n',
        '[env]\nRUSTFLAGS={value="--cfg=hidden",force=true}\n',
        '[target.x86_64-unknown-linux-gnu.fake]\nrustc-cfg=["hidden"]\n',
        'include=["extra.toml"]\n',
        '[source.crates-io]\nreplace-with="unreviewed"\n',
    ]
    home = Path(runner.env["CARGO_HOME"])
    # Each cwd is an actual sibling entry path; both file names fail closed.
    for i, text in enumerate(config_texts):
        directory = (root / ".cargo", runner.output / ".cargo", home)[i % 3]
        path = directory / ("config" if i % 2 else "config.toml")
        configure(path, text)
        try:
            reject(runner, lambda: runner.metadata(root, f"config-{i}-metadata"))
            reject(runner, lambda: runner.run(f"config-{i}-check", [runner.cargo, "check", "--frozen"], root))
            reject(runner, lambda: runner.run(f"config-{i}-build", [runner.cargo, "build", "--frozen"], root))
        finally:
            path.unlink()
    # A failed compiler cannot substitute a preexisting generated JSON.
    cell = Cell("cfg-boundary", "no-default")
    runner.document(root, cell, "valid")
    generated = runner.output / "failed-target" / runner.target / "doc/cfg_boundary.json"
    generated.parent.mkdir(parents=True)
    shutil.copyfile(runner.output / "valid-cfg-boundary-no-default.json", generated)
    (root / "src/lib.rs").write_text("not Rust;\n")
    try:
        runner.document(root, cell, "failed")
    except RuntimeError:
        demand(not generated.exists(), "failed compiler left reusable JSON")
    else:
        raise RuntimeError("compiler failure became green")
    try:
        runner.compare(runner.output / "absent.json", runner.output / "valid-cfg-boundary-no-default.json",
                       "missing-json")
    except RuntimeError:
        pass
    else:
        raise RuntimeError("missing rustdoc output became green")


def sdk(runner, source):
    baseline = baseline_source(runner, source)
    before = tree_digest(baseline)
    source_before = source_digest(source)
    demand(cells(runner.metadata(source, "candidate-metadata"), runner.target) ==
           cells(runner.metadata(baseline, "baseline-metadata"), runner.target), "cell drift")
    cell = Cell("rmux-sdk", "no-default")
    shapes = {}
    for side, root in (("baseline", baseline), ("current", source)):
        artifact = runner.document(root, cell, side)
        data = json.loads(artifact.read_text())
        names = {v["name"] for v in data["index"].values()}
        demand("WebShareBuilder" not in names, "no-default contains web exports")
        shapes[side] = {"index": len(data["index"]),
                        "root": len(data["index"][str(data["root"])]["inner"]["module"]["items"]),
                        "sha256": digest(artifact)}
    runner.compare(runner.output / "baseline-rmux-sdk-no-default.json",
                   runner.output / "current-rmux-sdk-no-default.json", "no-default-compare")
    # Reject frozen consumer lock resolution before Cargo can normalize anything.
    resolver = Runner(runner.output / "rejected-resolution", Path(runner.cargo).parent,
                      runner.env["CARGO_HOME"], runner.semver, runner.target)
    config = resolver.output / ".cargo/config.toml"
    configure(config, '[build]\nrustflags=["--cfg", \'feature="web"\']\n')
    reject(resolver, lambda: migration_project(resolver, baseline, source))
    copied = resolver.output / "sdk-consumer/Cargo.lock"
    demand(digest(copied) == digest(source / "Cargo.lock"), "rejected resolution changed lock")
    config.unlink()
    # Keep rejected inputs for evidence, then create the legitimate consumer.
    check_consumer(runner, baseline, source, cell)
    # Exercise the sibling Cargo build / .rlib downstream path once.
    check_consumer(runner, baseline, source, Cell("ratatui-rmux", "default"))
    artifacts = [json.loads(line) for line in (runner.output / "consumer-rmux-sdk-no-default.log").read_text().splitlines()
                 if line.startswith("{")]
    features = next(a["features"] for a in artifacts if a.get("reason") == "compiler-artifact"
                    and a["target"]["name"] == "rmux_sdk")
    demand(features == [], "no-default consumer feature drift")
    lock = runner.output / "sdk-consumer/Cargo.lock"
    lock_hash = digest(lock)
    # Baseline, candidate, consumer and output/direct-rustc entry paths all reject.
    for i, cwd in enumerate((baseline, source, runner.output / "sdk-consumer", runner.output)):
        config = cwd / ".cargo/config"
        configure(config, '[build]\nrustdocflags=["--cfg", \'feature="web"\']\n')
        try:
            if i < 2:
                reject(runner, lambda: runner.document(cwd, cell, f"configured-{i}"))
                if i == 1:
                    reject(runner, lambda: check_consumer(runner, baseline, source,
                                                        Cell("ratatui-rmux", "default"), "configured-"))
            elif i == 2:
                reject(runner, lambda: check_consumer(runner, baseline, source, cell, "configured-"))
            else:
                command = next(c["argv"] for c in runner.commands
                               if c["id"] == "consumer-rmux-sdk-no-default-identity-boundary")
                reject(runner, lambda: runner.run("configured-direct-rustc", command))
        finally:
            config.unlink()
            config.parent.rmdir()
    demand(digest(lock) == lock_hash, "frozen consumer lock changed")
    demand(tree_digest(baseline) == before and source_digest(source) == source_before, "source changed")
    return {"shapes": shapes, "features": features, "baseline_digest": before, "source_digest": source_before,
            "consumer_lock": lock_hash, "registry_pins": len(registry_packages(lock)),
            "pins_subset": registry_packages(lock) <= registry_packages(source / "Cargo.lock")}


if __name__ == "__main__":
    args = parser().parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    summary = {"result": "incomplete"}
    try:
        for name, action in (("removal", removal), ("boundaries", boundaries),
                             ("sdk", lambda r: sdk(r, args.source.resolve()))):
            runner = Runner(args.output / name, args.toolchain_bin, args.cargo_home,
                            args.semver_checks, args.target)
            if name == "removal":
                summary["tools"] = runner.preflight()
            summary[name] = action(runner)
        summary["result"] = "pass"
    finally:
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
