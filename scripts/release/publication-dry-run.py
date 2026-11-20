#!/usr/bin/env python3
"""Rehearse every publishable workspace crate without publication authority."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
from typing import Any

from release_tag_policy import TAG_PATTERN

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from toml_reader import loads as read_toml


def load_script(name: str) -> Any:
    path = Path(__file__).with_name(f"{name}.py")
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


canonical = load_script("canonical-crate-set")
publisher = load_script("publish-crate-set")
GIB = 1024 ** 3
MIN_FREE_BYTES = 35 * GIB
MAX_TARGET_BYTES = 6 * GIB


def isolated_environment(target: Path, cargo: Path) -> dict[str, str]:
    """Allow only OS execution variables; never inherit credentials or wrappers."""
    environment = {
        key: os.environ[key]
        for key in ("PATH", "SYSTEMROOT", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT")
        if key in os.environ
    }
    home = target / "home"
    temporary = target / "tmp"
    environment.update({
        "HOME": str(home), "USERPROFILE": str(home),
        "CARGO_HOME": str(target / "cargo-home"),
        "CARGO_TARGET_DIR": str(target / "cargo-target"),
        "RUSTC": str(cargo.with_name("rustc.exe" if os.name == "nt" else "rustc")),
        "RUSTDOC": str(cargo.with_name("rustdoc.exe" if os.name == "nt" else "rustdoc")),
        "CARGO_BUILD_JOBS": "2", "CARGO_PROFILE_DEV_DEBUG": "0",
        "CARGO_INCREMENTAL": "0", "CARGO_TERM_COLOR": "never",
        "TMPDIR": str(temporary), "TEMP": str(temporary), "TMP": str(temporary),
        "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0",
        "GIT_CONFIG_COUNT": "3", "GIT_CONFIG_KEY_0": "credential.helper",
        "GIT_CONFIG_VALUE_0": "", "GIT_CONFIG_KEY_1": "core.sshCommand",
        "GIT_CONFIG_VALUE_1": "false", "GIT_CONFIG_KEY_2": "core.hooksPath",
        "GIT_CONFIG_VALUE_2": os.devnull,
    })
    return environment


def check_space(target: Path) -> None:
    if shutil.disk_usage(target).free < MIN_FREE_BYTES:
        raise ValueError("publication rehearsal stopped: less than 35 GiB free")
    size = sum(path.stat().st_size for path in target.rglob("*") if path.is_file())
    if size > MAX_TARGET_BYTES:
        raise ValueError("publication rehearsal stopped: target exceeds 6 GiB")


def write_report(target: Path, report: dict[str, Any]) -> None:
    destination = target / "publication-dry-run.json"
    temporary = target / "publication-dry-run.json.tmp"
    temporary.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(destination)


def task_alias_only(path: Path) -> bool:
    """Allow one explicit alias table followed by its one inert assignment."""
    try:
        lines = path.read_text(encoding="utf-8").split("\n")
        for index, line in enumerate(lines):
            statement = line.lstrip(" \t")
            if not statement or statement.startswith("#"):
                continue
            # Parse the header and body separately: the subset reader can reopen
            # inline tables, so equality of the complete document is insufficient.
            return (
                statement.startswith("[")
                and read_toml(line) == {"alias": {}}
                and read_toml("\n".join(lines[index + 1:])) == {
                    "xtask": "run --locked -p xtask --",
                }
            )
        return False
    except (OSError, ValueError):
        return False


def reject_cargo_configuration(source: Path) -> None:
    """Permit the source's inert task alias; Cargo ancestors remain unconfigured."""
    for directory in (source, *source.parents):
        cargo_dir = directory / ".cargo"
        try:
            mode = cargo_dir.lstat().st_mode
        except FileNotFoundError:
            continue
        if not stat.S_ISDIR(mode):
            raise ValueError(f"publication rehearsal refuses Cargo configuration: {cargo_dir}")
        for name in ("config", "config.toml"):
            path = cargo_dir / name
            try:
                mode = path.lstat().st_mode
            except FileNotFoundError:
                continue
            if directory == source and stat.S_ISREG(mode) and task_alias_only(path):
                continue
            raise ValueError(f"publication rehearsal refuses Cargo configuration: {path}")


def execute(args: argparse.Namespace) -> dict[str, Any]:
    if publisher.SOURCE_SHA.fullmatch(args.source_sha) is None:
        raise ValueError("source SHA must be lowercase hexadecimal")
    release = TAG_PATTERN.fullmatch(args.release_ref)
    if release is None or args.version != release.group("version").partition("-rc.")[0]:
        raise ValueError("release ref and package version disagree")
    if not args.cargo.is_absolute():
        raise ValueError("cargo must be an absolute toolchain executable path")
    target = args.target_dir.absolute()
    environment = isolated_environment(target, args.cargo)
    source = publisher.validate_source_checkout(
        args.source_dir, args.source_sha, environment=environment,
    )
    reject_cargo_configuration(source)
    publisher.prepare_target_dir(target)
    target = target.resolve(strict=True)
    environment = isolated_environment(target, args.cargo)
    for key in ("HOME", "CARGO_HOME", "TMPDIR"):
        Path(environment[key]).mkdir()
    (target / "logs").mkdir()
    report = {
        "schema_version": 1, "mode": "dry-run", "source_git_sha": args.source_sha,
        "release_ref": args.release_ref, "version": args.version,
        "source_dir": str(source), "cargo": str(args.cargo),
        "status": "running", "publish_order": [], "workspace_dependencies": {},
        "results": [],
    }
    write_report(target, report)
    try:
        check_space(target)
        metadata = canonical.cargo_metadata(
            source_dir=source, cargo=str(args.cargo), environment=environment,
        )
        packages = canonical.publishable_packages(metadata, args.version)
        order, dependencies = canonical.dependency_order(packages)
        report.update(publish_order=order, workspace_dependencies=dependencies)
        write_report(target, report)
        for name in order:
            check_space(target)
            reject_cargo_configuration(source)
            command = [str(args.cargo), *publisher.publish_arguments(name, dry_run=True)]
            print(f"rehearsing {name}: cargo publish --dry-run --locked", flush=True)
            completed = subprocess.run(
                command, cwd=source, env=environment, check=False,
                capture_output=True, text=True,
            )
            log = f"logs/{name}.log"
            (target / log).write_text(completed.stdout + completed.stderr, encoding="utf-8")
            report["results"].append({
                "name": name, "command": command, "exit_code": completed.returncode,
                "log": log,
            })
            write_report(target, report)
            check_space(target)
        # Reuse the same source validator to detect mutations made during Cargo verification.
        publisher.validate_source_checkout(source, args.source_sha, environment=environment)
        report["status"] = "failed" if any(r["exit_code"] for r in report["results"]) else "success"
    except (OSError, ValueError, KeyboardInterrupt) as error:
        report.update(status="interrupted", error=str(error))
        write_report(target, report)
        raise
    write_report(target, report)
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--version", required=True, help="candidate package_version")
    parser.add_argument("--cargo", type=Path, required=True, help="absolute cargo next to rustc")
    parser.add_argument("--target-dir", type=Path, required=True, help="absent private rehearsal directory")
    return parser.parse_args()


if __name__ == "__main__":
    try:
        result = execute(parse_args())
        print(json.dumps(result, sort_keys=True))
        raise SystemExit(0 if result["status"] == "success" else 1)
    except (OSError, ValueError) as error:
        print(f"publication-dry-run: {error}", file=sys.stderr)
        raise SystemExit(1) from error
