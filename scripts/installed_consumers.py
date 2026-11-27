#!/usr/bin/env python3
"""Linux author gate: frozen external Cargo consumers on exact Rust 1.88.0.

Usage: python3 scripts/installed_consumers.py --sha FULL_SHA --tree FULL_TREE
  --tools /absolute/rust-1.88.0/bin --cache /completed/registry/cache/INDEX
  --output /absolute/absent/Projects/proof

Requires Python >=3.11. Offline; no publication, source patches, shared cache
writes or installed daemon discovery. Output must be outside this workspace.
The directory source is bounded to exact locked archives and Cargo-produced
RMUX archives. Packaging skips Cargo's automatic verify; the mandatory frozen
external builds below supply the consumer proof instead. This is neither an
OS sandbox, a complete SemVer gate nor release/native fleet qualification.
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent / "release"))

from installed_consumer_packages import (
    file_hash, install_locked, package_facades, verify_install,
)
from installed_consumer_checks import freeze, project, build_cell, controls
from public_api_policy import cells, require_complete
from installed_consumer_diagnostics import expected_diagnostic, match_diagnostic


class Runner:
    target = "x86_64-unknown-linux-gnu"

    def __init__(self, args):
        self.source = Path(__file__).resolve().parents[1]
        self.output = args.output.resolve()
        if not args.output.is_absolute() or self.output.is_relative_to(self.source):
            raise ValueError("output must be absolute and outside the source workspace")
        if not args.tools.is_absolute() or not args.cache.is_absolute():
            raise ValueError("tools and archive cache must be absolute paths")
        self.output.mkdir(mode=0o700, exist_ok=False)
        for name in ("logs", "home", "cargo-home", "bin", "tmp"):
            (self.output / name).mkdir(mode=0o700)
        self.vendor = self.output / "vendor"
        self.cargo = args.tools / "cargo"
        self.rustc = args.tools / "rustc"
        self.env = {
            "PATH": f"{args.tools}:/usr/bin:/bin", "HOME": str(self.output / "home"),
            "CARGO_HOME": str(self.output / "cargo-home"), "RUSTC": str(self.rustc),
            "RUSTDOC": str(args.tools / "rustdoc"), "CARGO_BUILD_JOBS": "2",
            "CARGO_NET_OFFLINE": "true", "CARGO_PROFILE_DEV_DEBUG": "0",
            "TMPDIR": str(self.output / "tmp"), "LANG": "C.UTF-8", "TERM": "xterm-256color",
            "SHELL": "/bin/sh",
        }
        self.receipt = {"schema": 1, "status": "incomplete", "source_sha": args.sha,
            "source_tree": args.tree, "source": str(self.source), "target": self.target,
            "environment": self.env, "commands": [], "cells": [], "controls": [], "runtime": [],
            "pending": ["macOS native", "physical Windows native", "final composition/release artifacts"]}
        self.save()

    @staticmethod
    def label(cell):
        return cell.id.replace("/", "-").replace("+", "-")

    def save(self):
        target = self.output / "receipt.json"
        temporary = target.with_suffix(".tmp")
        temporary.write_text(json.dumps(self.receipt, indent=2, sort_keys=True) + "\n")
        temporary.replace(target)

    def run(self, label, command, cwd=None, reject=None, diagnostic=None, timeout=1200,
            diagnostic_source=None, diagnostic_package=None):
        command = list(map(str, command))
        cwd = cwd or self.output
        log = self.output / "logs" / (f"{len(self.receipt['commands']):03d}-" + label + ".log")
        errors = log.with_suffix(".stderr.log")
        record = {"id": label, "command": command, "cwd": str(cwd), "log": str(log),
                  "stderr_log": str(errors), "status": "incomplete", "expected_rejection": reject}
        expected = (expected_diagnostic(diagnostic, reject, diagnostic_source, diagnostic_package)
                    if reject is not None else None)
        record["expected_diagnostic"] = expected
        self.receipt["commands"].append(record)
        self.save()
        print(f"{label}: {shlex.join(command)}", flush=True)
        start = time.monotonic()
        with log.open("w") as output, errors.open("w") as stderr:
            child = subprocess.Popen(command, cwd=cwd, env=self.env, stdout=output,
                                     stderr=stderr, start_new_session=True)
            try:
                code = child.wait(timeout=timeout)
            except BaseException:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                record.update(status="incomplete", elapsed_seconds=round(time.monotonic() - start, 3))
                self.save()
                raise
        output = log.read_text(errors="replace")
        stderr = errors.read_text(errors="replace")
        diagnostic_output = output + stderr
        record.update(returncode=code, elapsed_seconds=round(time.monotonic() - start, 3))
        matched = match_diagnostic(output, stderr, cwd, expected) if expected and code > 0 else None
        record["matched_diagnostic"] = matched
        good = (code == 0) if expected is None else matched is not None
        record["status"] = "pass" if good else "fail"
        self.save()
        print(f"{label}: exit={code} {record['status']} ({record['elapsed_seconds']}s)", flush=True)
        if not good:
            raise ValueError(f"{label}: unexpected result; see {log}\n{diagnostic_output[-2500:]}")
        return output

    def identity(self):
        sha = self.run("source-sha", ["/usr/bin/git", "rev-parse", "HEAD"], self.source).strip()
        tree = self.run("source-tree", ["/usr/bin/git", "rev-parse", "HEAD^{tree}"], self.source).strip()
        status = self.run("source-status", ["/usr/bin/git", "status", "--porcelain", "--untracked-files=all"], self.source)
        if sha != self.receipt["source_sha"] or tree != self.receipt["source_tree"] or status.strip():
            raise ValueError("source must be the exact clean SHA/tree")
        return file_hash(self.source / "Cargo.lock")


def daemon(runner):
    runner.run("daemon-build", [runner.cargo, "build", "--frozen", "-j", "2", "--bin", "rmux",
        "--target", runner.target, "--target-dir", runner.output / "daemon-target"], runner.source)
    source = runner.output / "daemon-target" / runner.target / "debug/rmux"
    binary = runner.output / "bin/rmux"
    shutil.copyfile(source, binary)
    binary.chmod(0o700)
    if file_hash(source) != file_hash(binary):
        raise ValueError("daemon copy differs from candidate build")
    version = runner.run("daemon-version", [binary, "--version"]).strip()
    runner.receipt["daemon"] = {"binary": str(binary), "sha256": file_hash(binary),
        "version": version, "source_sha": runner.receipt["source_sha"], "features": ["default", "web"]}
    runner.save()
    return binary


def runtime(runner, daemon_binary, binary, index):
    # A short private endpoint avoids sockaddr_un limits under long run paths.
    endpoint_root = runner.output / f"s{index}"
    endpoint_root.mkdir(mode=0o700)
    socket = endpoint_root / "s"
    if len(os.fsencode(socket)) >= 104:
        raise ValueError("use a shorter external output path for the private Unix socket")
    record = {"consumer": str(binary), "socket": str(socket), "status": "incomplete"}
    runner.receipt["runtime"].append(record)
    command = [str(daemon_binary), "--__internal-daemon", str(socket)]
    record["command"] = command
    runner.save()
    with (runner.output / "logs" / f"daemon-{index}.log").open("w") as log:
        child = subprocess.Popen(command, env=runner.env, cwd=runner.output,
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        record["pid"] = child.pid
        try:
            deadline = time.monotonic() + 10
            while not socket.exists():
                if child.poll() is not None or time.monotonic() >= deadline:
                    raise ValueError("owned daemon failed to become ready")
                time.sleep(0.025)
            executable = Path(f"/proc/{child.pid}/exe")
            if executable.resolve() != daemon_binary or file_hash(executable) != file_hash(daemon_binary):
                raise ValueError("running daemon identity differs from candidate artifact")
            record["executable_sha256"] = file_hash(executable)
            # Keep explicit SDK session removal and explicit daemon shutdown
            # independently observable; the product default exits on last kill.
            runner.run(f"daemon-config-{index}", [daemon_binary, "-S", socket,
                "set-option", "-s", "exit-empty", "off"], timeout=10)
            runner.run(f"runtime-{index}", [binary, socket, daemon_binary], timeout=45)
            code = child.wait(timeout=10)
            record["daemon_exit"] = code
            if code or socket.exists():
                raise ValueError("owned daemon cleanup did not complete")
            record["status"] = "pass"
        except BaseException as error:
            record.update(status="fail", error=str(error))
            raise
        finally:
            if child.poll() is None:
                try:
                    runner.run(f"cleanup-{index}", [daemon_binary, "-S", socket, "kill-server"], timeout=10)
                    child.wait(timeout=10)
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait()
                        record["forced_daemon_cleanup"] = True
            runner.save()


def execute(runner, args):
    if sys.platform != "linux":
        raise ValueError("this bounded native gate requires Linux")
    lock = runner.identity()
    runner.receipt["lock_sha256"] = lock
    for name, tool in (("rustc", runner.rustc), ("cargo", runner.cargo)):
        value = runner.run(name + "-version", [tool, "-Vv"])
        if "release: 1.88.0\n" not in value or f"host: {runner.target}\n" not in value:
            raise ValueError(f"{name} must be native exact 1.88.0")
        runner.receipt[name] = {"path": str(tool.resolve()), "version": value, "sha256": file_hash(tool)}
    frozen = freeze(runner)
    install_locked(runner, args.cache)
    (runner.output / "cargo-home/config.toml").write_text(
        '[source.crates-io]\nreplace-with = "installed"\n[source.installed]\ndirectory = '
        + json.dumps(str(runner.vendor)) + "\n")
    metadata = json.loads(runner.run("source-metadata", [runner.cargo, "metadata", "--frozen",
                         "--no-deps", "--format-version", "1"], runner.source))
    runner.source_packages = {p["name"]: p for p in metadata["packages"]}
    required = cells(metadata, runner.target)
    package_facades(runner, metadata)
    projects = {name: project(runner, frozen, name) for name in ("rmux-sdk", "ratatui-rmux")}
    binaries = {}
    for cell in required:
        binaries[cell.id] = build_cell(runner, frozen, projects[cell.package], cell)
    require_complete(required, [c["id"] for c in runner.receipt["cells"]])
    baseline_locks = {name: file_hash(path / "Cargo.lock") for name, path in projects.items()}
    controls(runner, projects)
    if baseline_locks != {name: file_hash(path / "Cargo.lock") for name, path in projects.items()}:
        raise ValueError("control modified a baseline lock")
    for relative, digest in runner.receipt["fixtures"]["files"].items():
        if file_hash(frozen / relative) != digest:
            raise ValueError("control modified a frozen witness")
    verify_install(runner.vendor)
    for package in runner.receipt["packages"]:
        if file_hash(Path(package["archive"])) != package["sha256"]:
            raise ValueError("baseline archive changed")
    runner.receipt["packaging_status"] = "pass"
    runner.save()
    daemon_binary = daemon(runner)
    failures = []
    for index, name in enumerate(("rmux-sdk/no-default", "rmux-sdk/all-features", "ratatui-rmux/default")):
        try:
            runtime(runner, daemon_binary, binaries[name], index)
        except Exception as error:
            failures.append(f"{name}: {error}")
    if runner.identity() != lock:
        raise ValueError("candidate lock changed")
    runner.receipt["lock_sha256"] = lock
    if failures:
        raise ValueError("\n".join(failures))
    runner.receipt["status"] = "pass"
    runner.save()


def job_object_cells():
    from installed_job_objects import job_object_cells as cells
    return cells()


def main():
    if sys.argv[1:2] == ["--windows-job-objects"]:
        if len(sys.argv) != 3:
            raise ValueError("--windows-job-objects requires one absolute plan JSON path")
        from installed_job_objects import collect
        collect(Path(sys.argv[2]))
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("sha", "tree"):
        parser.add_argument("--" + option, required=True)
    for option in ("tools", "cache", "output"):
        parser.add_argument("--" + option, type=Path, required=True)
    args = parser.parse_args()
    runner = Runner(args)
    try:
        execute(runner, args)
    except (Exception, KeyboardInterrupt) as error:
        runner.receipt.update(status="fail", error=str(error))
        runner.save()
        print(str(error), file=sys.stderr)
        return 1
    print(f"installed consumers: PASS; {runner.output / 'receipt.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
