#!/usr/bin/env python3
"""Compiler-backed, version-independent no-break gate. See public-api-gate.md."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tarfile

from public_api_policy import (BASELINE_COMMIT, BASELINE_TREE, FROZEN_FIXTURES, RUST_VERSION,
                               SEMVER_VERSION, cells, require_complete)
from public_api_consumers import check_consumer


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def source_digest(root):
    """Hash every nonignored source input, including uncommitted new files."""
    files = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root
    ).decode().split("\0")
    return tree_digest(root, sorted(set(files) - {""}))


def tree_digest(root, files=None):
    root = Path(root)
    if files is None:
        files = sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.is_file())
    hashes = {name: digest(root / name) for name in files}
    return hashlib.sha256(json.dumps(hashes, sort_keys=True).encode()).hexdigest()


def require_unconfigured(cwd, cargo_home):
    """Fail closed on Cargo's cwd ancestry and explicit home, before any tool.

    Check both names, even the shadowed file: removing legacy `config` must not
    silently activate `config.toml`. No TOML keys (including includes, env,
    source replacement or build-script overrides) are supported by this gate.
    Member/manifest directories are not searched unless they are the cwd.
    """
    directories = [p / ".cargo" for p in (cwd, *cwd.parents)]
    directories.append(Path(cargo_home))
    for directory in dict.fromkeys(directories):
        for name in ("config", "config.toml"):
            path = directory / name
            try:
                path.lstat()
            except FileNotFoundError:
                continue
            raise ValueError(f"unsupported Cargo configuration: {path}; use a config-free "
                             "cwd/ancestry and private Cargo home (registry cache is supported); "
                             "keep required native tools in PATH and native SDK environment")


class Runner:
    def __init__(self, output, toolchain_bin, cargo_home, semver, target):
        self.output = Path(output).resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        toolchain_bin = Path(toolchain_bin).absolute()
        suffix = ".exe" if os.name == "nt" else ""
        self.cargo = str(toolchain_bin / ("cargo" + suffix))
        self.rustc = str(toolchain_bin / ("rustc" + suffix))
        self.rustdoc = str(toolchain_bin / ("rustdoc" + suffix))
        self.semver = str(Path(semver).absolute())
        self.target = target
        # Windows environment names are case-insensitive. Apply the same strict
        # contract on Unix, without removing native PATH/LIB/INCLUDE/SDK setup.
        def controlled(key):
            return key.upper() == "CARGO" or key.upper().startswith(("CARGO_", "RUST"))

        self.ambient_overrides = sorted(k for k, v in os.environ.items() if v and controlled(k)
                                        and k.upper() != "CARGO_HOME"
                                        and not k.upper().startswith("RUSTUP_"))
        self.env = {k: v for k, v in os.environ.items()
                    if not controlled(k) and k.upper() != "PATH"}
        native_path = next((v for k, v in os.environ.items() if k.upper() == "PATH"), "")
        self.env.update({
            "PATH": str(toolchain_bin) + os.pathsep + native_path,
            "CARGO_HOME": str(Path(cargo_home).resolve()),
            "CARGO_NET_OFFLINE": "true", "CARGO_BUILD_JOBS": "2",
            "CARGO_INCREMENTAL": "0", "RUSTC": self.rustc, "RUSTDOC": self.rustdoc,
            "CARGO_PROFILE_DEV_DEBUG_ASSERTIONS": "true",
            "CARGO_PROFILE_DEV_OVERFLOW_CHECKS": "true",
            # Only unlock rustdoc JSON on the pinned compiler; no product cfg flags.
            "RUSTC_BOOTSTRAP": "1",
        })
        self.commands = []

    def run(self, label, command, cwd=None, expected=0):
        command = [str(arg) for arg in command]
        cwd = Path(cwd or self.output).resolve()
        print(f"[{label}] {shlex.join(command)}", flush=True)
        try:
            if self.ambient_overrides:
                raise ValueError("unsupported ambient compiler configuration: "
                                 + ", ".join(self.ambient_overrides)
                                 + "; unset these overrides explicitly; native linker/cache "
                                 "settings must not be silently discarded")
            require_unconfigured(cwd, self.env["CARGO_HOME"])
        except (OSError, ValueError) as error:
            (self.output / f"{label}.log").write_text(f"not executed: {error}\n")
            self.commands.append({"id": label, "cwd": str(cwd), "argv": command,
                                  "exit": None, "rejected_configuration": str(error)})
            self.save_commands()
            raise
        result = subprocess.run(command, cwd=cwd, env=self.env,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        (self.output / f"{label}.log").write_text(result.stdout)
        self.commands.append({"id": label, "cwd": str(cwd),
                              "argv": command, "exit": result.returncode})
        self.save_commands()
        if (expected == 0 and result.returncode != 0) or (expected != 0 and result.returncode == 0):
            raise RuntimeError(f"{label}: unexpected exit {result.returncode}; "
                               f"see {self.output / (label + '.log')}")
        return result.stdout

    def save_commands(self):
        (self.output / "commands.json").write_text(json.dumps(self.commands, indent=2) + "\n")

    def preflight(self):
        fixtures = Path(__file__).resolve().parents[1] / "tests/fixtures/public-api-gate"
        for name, expected in FROZEN_FIXTURES.items():
            if digest(fixtures / name) != expected:
                raise ValueError(f"frozen consumer/canary changed: {name}")
        versions = {}
        for name in ("cargo", "rustc", "rustdoc", "semver"):
            binary = getattr(self, name)
            if not Path(binary).is_file():
                raise ValueError(f"missing installed tool: {binary}")
            value = self.run(name + "-version", [binary, "--version"])
            expected = SEMVER_VERSION if name == "semver" else RUST_VERSION
            if value.split()[1] != expected:
                raise ValueError(f"expected installed {name} {expected}, got {value.strip()}")
            versions[name] = {"path": binary, "version": value.strip(), "sha256": digest(binary)}
        verbose = self.run("rustc-verbose", [self.rustc, "-vV"])
        if f"host: {self.target}\n" not in verbose:
            raise ValueError("gate requires a native compiler matching the explicit target")
        return versions

    def metadata(self, root, label):
        return json.loads(self.run(label, [self.cargo, "metadata", "--no-deps",
                                           "--format-version", "1", "--frozen",
                                           "--manifest-path", Path(root) / "Cargo.toml"], root))

    def document(self, root, cell, side):
        label = f"{side}-{cell.id.replace('/', '-')}"
        target_dir = self.output / (side + "-target")
        generated = target_dir / self.target / "doc" / (cell.package.replace("-", "_") + ".json")
        # No stale JSON may stand in for a failed or omitted compiler invocation.
        generated.unlink(missing_ok=True)
        self.run(label, [self.cargo, "rustdoc", "--frozen", "--lib", "-j", "2",
                         "--manifest-path", Path(root) / "Cargo.toml", "-p", cell.package,
                         "--target", self.target, "--target-dir", target_dir, *cell.flags,
                         "--", "-Z", "unstable-options", "--output-format", "json"], root)
        artifact = self.output / (label + ".json")
        shutil.copyfile(generated, artifact)
        data = json.loads(artifact.read_text())
        root_item = data["index"][str(data["root"])]
        if (root_item["name"] != cell.package.replace("-", "_")
                or not root_item["inner"]["module"]["items"]
                or data["includes_private"] or data["target"]["triple"] != self.target):
            raise ValueError(f"wrong target, empty or private rustdoc surface: {label}")
        return artifact

    def compare(self, baseline, current, label, expected=0, release_type="minor"):
        # Explicit minor permits additions but enables every major-breaking lint,
        # even when Cargo/rustdoc claims that this is a major version release.
        # Feature switches are intentionally on the compiler invocation: the CLI
        # rejects them with --current-rustdoc. No heuristic or registry fetch runs.
        return self.run(label, [self.semver, "--baseline-rustdoc", baseline,
                               "--current-rustdoc", current, "--release-type", release_type,
                               "--target", self.target, "--color", "never"], expected=expected)


def baseline_source(runner, source, prefix=""):
    actual = runner.run(prefix + "baseline-tree", ["git", "rev-parse", BASELINE_COMMIT + "^{tree}"], source).strip()
    if actual != BASELINE_TREE:
        raise ValueError("baseline tree mismatch")
    archive = runner.output / "baseline.tar"
    runner.run(prefix + "baseline-archive", ["git", "archive", "--format=tar", "--output", archive,
                                    BASELINE_COMMIT], source)
    root = runner.output / "baseline"
    root.mkdir()
    with tarfile.open(archive) as tar:
        # The archive is from the exact repository-owned Git object above.
        tar.extractall(root)
    return root


def gate(args):
    source = args.source.resolve()
    runner = Runner(args.output, args.toolchain_bin, args.cargo_home, args.semver_checks, args.target)
    receipt = {"result": "incomplete", "target": args.target, "completed": []}
    try:
        receipt["tools"] = runner.preflight()
        receipt["source_commit"] = runner.run("source-commit", ["git", "rev-parse", "HEAD"], source).strip()
        receipt["source_digest"] = source_digest(source)
        metadata = runner.metadata(source, "current-metadata")
        selected = cells(metadata, args.target, args.cell)
        baseline = baseline_source(runner, source)
        receipt["baseline"] = {"commit": BASELINE_COMMIT, "tree": BASELINE_TREE,
                               "source_digest": tree_digest(baseline)}
        baseline_cells = cells(runner.metadata(baseline, "baseline-metadata"), args.target)
        if selected != baseline_cells:
            raise ValueError("baseline and candidate feature cells differ")
        receipt["required"] = [{"id": c.id, "cargo_flags": c.flags} for c in selected]
        for cell in selected:
            old = runner.document(baseline, cell, "baseline")
            new = runner.document(source, cell, "current")
            runner.compare(old, new, "compare-" + cell.id.replace("/", "-"))
            check_consumer(runner, baseline, source, cell)
            receipt["completed"].append({"id": cell.id, "baseline_sha256": digest(old),
                                          "current_sha256": digest(new)})
        require_complete(selected, [c["id"] for c in receipt["completed"]])
        if tree_digest(baseline) != receipt["baseline"]["source_digest"]:
            raise ValueError("baseline source changed during gate")
        if source_digest(source) != receipt["source_digest"]:
            raise ValueError("candidate source changed during gate")
        receipt["result"] = "pass"
        print(f"PASS: {len(selected)} native {args.target} cells; no aliases or skipped cells", flush=True)
    finally:
        (runner.output / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    result.add_argument("--output", required=True, type=Path, help="new, run-owned evidence directory")
    result.add_argument("--toolchain-bin", required=True, type=Path, help="installed 1.96.1 bin directory")
    result.add_argument("--cargo-home", required=True, type=Path, help="private populated offline Cargo home")
    result.add_argument("--semver-checks", required=True, type=Path, help="installed 0.49.0 executable")
    result.add_argument("--target", required=True)
    result.add_argument("--cell", action="append", help="optional exact enumeration of ALL required cell IDs")
    return result


if __name__ == "__main__":
    try:
        gate(parser().parse_args())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(f"public-api-gate: {error}")
