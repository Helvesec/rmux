#!/usr/bin/env python3
"""Collect RMUX benchmark JSON artifacts from local and remote machines."""

from __future__ import annotations

import argparse
import base64
import platform
import shlex
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
BENCH_DIR = REPO_ROOT / "target" / "benchmarks"


def run(cmd: list[str], *, dry_run: bool, timeout: int, cwd: Path = REPO_ROOT) -> None:
    print("+ " + shlex.join(cmd), flush=True)
    if dry_run:
        return
    subprocess.run(cmd, cwd=cwd, timeout=timeout, check=True)


def powershell_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def encoded_powershell(script: str) -> str:
    return base64.b64encode(script.encode("utf-16le")).decode("ascii")


def remote_windows_path(repo: str, suffix: str) -> str:
    return repo.rstrip("\\/") + "\\" + suffix


def scp_windows_path(repo: str, suffix: str) -> str:
    return repo.rstrip("\\/").replace("\\", "/") + "/" + suffix.replace("\\", "/")


def collect_local_unix(platform_id: str, iterations: int, skip_build: bool, dry_run: bool, timeout: int) -> None:
    out = f"target/benchmarks/{platform_id}.json"
    cmd = [
        "tools/bench/run-unix.sh",
        "--iterations",
        str(iterations),
        "--out",
        out,
    ]
    if skip_build:
        cmd.append("--skip-build")
    run(cmd, dry_run=dry_run, timeout=timeout)


def collect_windows(
    host: str,
    repo: str,
    iterations: int,
    skip_build: bool,
    dry_run: bool,
    timeout: int,
) -> None:
    out = remote_windows_path(repo, "target\\benchmarks\\windows.json")
    runner = remote_windows_path(repo, "tools\\bench\\run-windows.ps1")
    args = ["-Iterations", str(iterations), "-Out", powershell_literal(out)]
    if skip_build:
        args.append("-SkipBuild")
    script = "& " + powershell_literal(runner) + " " + " ".join(args)
    script += "\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n"
    run(
        ["ssh", host, "powershell", "-NoProfile", "-EncodedCommand", encoded_powershell(script)],
        dry_run=dry_run,
        timeout=timeout,
    )
    run(
        [
            "scp",
            f"{host}:{scp_windows_path(repo, 'target/benchmarks/windows.json')}",
            "target/benchmarks/windows.json",
        ],
        dry_run=dry_run,
        timeout=timeout,
    )


def collect_linux(
    host: str,
    repo: str,
    iterations: int,
    skip_build: bool,
    dry_run: bool,
    timeout: int,
) -> None:
    parts = [
        'PATH="$HOME/.cargo/bin:$PATH"',
        "tools/bench/run-unix.sh",
        "--iterations",
        str(iterations),
        "--out",
        "target/benchmarks/linux.json",
    ]
    if skip_build:
        parts.append("--skip-build")
    command = f"cd {shlex.quote(repo)} && " + " ".join(parts)
    run(["ssh", host, command], dry_run=dry_run, timeout=timeout)
    run(
        ["scp", f"{host}:{repo.rstrip('/')}/target/benchmarks/linux.json", "target/benchmarks/linux.json"],
        dry_run=dry_run,
        timeout=timeout,
    )


def render(output: Path, asset_dir: Path, dry_run: bool, timeout: int) -> None:
    inputs = sorted(str(path.relative_to(REPO_ROOT)) for path in BENCH_DIR.glob("*.json"))
    if not inputs:
        raise SystemExit("no benchmark JSON artifacts found in target/benchmarks")
    run(
        [
            sys.executable,
            "tools/bench/render.py",
            *inputs,
            "--output",
            str(output),
            "--asset-dir",
            str(asset_dir),
        ],
        dry_run=dry_run,
        timeout=timeout,
    )


def local_platform(value: str) -> str | None:
    if value != "auto":
        return None if value == "none" else value
    system = platform.system().lower()
    if system == "darwin":
        return "macos"
    if system == "linux":
        return "linux"
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--local-platform", choices=("auto", "macos", "linux", "none"), default="auto")
    parser.add_argument("--windows-host", help="SSH host for Windows collection")
    parser.add_argument("--windows-repo", help="Windows repo path, for example C:\\path\\to\\rmux")
    parser.add_argument("--linux-host", help="SSH host for Linux collection")
    parser.add_argument("--linux-repo", help="Linux repo path, for example /home/user/rmux")
    parser.add_argument("--unix-iterations", type=int, default=25)
    parser.add_argument("--windows-iterations", type=int, default=3)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--no-render", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("docs/benchmarks.md"))
    parser.add_argument("--asset-dir", type=Path, default=Path("docs/benchmarks"))
    parser.add_argument("--step-timeout-seconds", type=int, default=3600)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if (args.windows_host is None) != (args.windows_repo is None):
        parser.error("--windows-host and --windows-repo must be passed together")
    if (args.linux_host is None) != (args.linux_repo is None):
        parser.error("--linux-host and --linux-repo must be passed together")
    if args.unix_iterations < 1 or args.windows_iterations < 1:
        parser.error("iteration counts must be positive")

    BENCH_DIR.mkdir(parents=True, exist_ok=True)

    platform_id = local_platform(args.local_platform)
    if platform_id:
        collect_local_unix(platform_id, args.unix_iterations, args.skip_build, args.dry_run, args.step_timeout_seconds)
    if args.windows_host and args.windows_repo:
        collect_windows(
            args.windows_host,
            args.windows_repo,
            args.windows_iterations,
            args.skip_build,
            args.dry_run,
            args.step_timeout_seconds,
        )
    if args.linux_host and args.linux_repo:
        collect_linux(
            args.linux_host,
            args.linux_repo,
            args.unix_iterations,
            args.skip_build,
            args.dry_run,
            args.step_timeout_seconds,
        )
    if not args.no_render:
        render(args.output, args.asset_dir, args.dry_run, args.step_timeout_seconds)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
