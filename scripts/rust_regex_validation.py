"""Validate with the daemon's Rust regex engine, never a Python approximation."""
from functools import lru_cache
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess

from toml_reader import loads as read_toml
from web_compatibility_model import require

ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / "scripts/rust-regex-validator"


def verify_engine(lock: str) -> None:
    """Reject engine drift, including the syntax parser's transitive versions."""
    daemon = read_toml(lock)["package"]
    validator = read_toml((TOOL / "Cargo.lock").read_text())["package"]
    server = next(package for package in daemon if package["name"] == "rmux-server")
    dependency = [item.split() for item in server["dependencies"] if item.split()[0] == "regex"]
    require(len(dependency) == 1, "daemon Rust regex dependency absent or ambiguous")
    selected = [package for package in daemon if package["name"] == "regex"
                and (len(dependency[0]) == 1 or package["version"] == dependency[0][1])]
    require(selected == [package for package in validator if package["name"] == "regex"],
            "Rust regex validator engine differs from daemon")
    for package in validator:
        if package["name"] == "rmux-regex-validator":
            continue
        require(package in daemon,
                f"Rust regex validator dependency differs from daemon: {package['name']}")


# A caller's build context (a candidate identity target and compiler wrapper,
# or the OUT_DIR Cargo exports to a running test) belongs to another lock
# workspace and must not select or wrap this standalone tool's build.
INHERITED_BUILD_CONTEXT = ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "RUSTC_WRAPPER",
                           "CARGO_BUILD_RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                           "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "OUT_DIR")


def private_target() -> Path:
    """One target per checked-out commit and OS, so another commit's image is never reused."""
    head = subprocess.run(["git", "rev-parse", "--verify", "HEAD^{commit}"], cwd=ROOT,
                          capture_output=True, text=True)
    require(head.returncode == 0, f"Rust regex validator commit unresolved: {head.stderr.strip()}")
    return ROOT / ".rmux-audit/rust-regex-target" / head.stdout.strip() / platform.system().lower()


@lru_cache(maxsize=1)
def validator_binary() -> Path:
    """Cargo supplies the actual executable path and checks source freshness."""
    cargo = shutil.which("cargo")
    require(cargo is not None, "Rust regex validation requires Cargo (offline cached dependencies)")
    target = private_target()
    command = [cargo, "build", "--manifest-path", str(TOOL / "Cargo.toml"),
               "--locked", "--offline", "--message-format=json", "--target-dir", str(target)]
    environment = {key: value for key, value in os.environ.items()
                   if key not in INHERITED_BUILD_CONTEXT}
    result = subprocess.run(command, cwd=ROOT, env=environment, capture_output=True, text=True)
    require(result.returncode == 0, f"Rust regex validator build failed: {result.stderr.strip()}")
    executables = []
    for line in result.stdout.splitlines():
        record = json.loads(line)
        if (record.get("reason") == "compiler-artifact"
                and record.get("target", {}).get("name") == "rmux-regex-validator"
                and record.get("manifest_path") == str(TOOL / "Cargo.toml")
                and record.get("executable")):
            executables.append(Path(record["executable"]))
    require(len(executables) == 1 and executables[0].is_file()
            and executables[0].resolve().is_relative_to(target.resolve()),
            "Rust regex validator executable absent or ambiguous")
    return executables[0]


def validate_regex(pattern: str, field: str, name: str) -> None:
    # stdin preserves NULs, literal newlines and all Rust syntax without shell or
    # command-line pattern splitting. Regex::new also enforces UTF-8 and size limits.
    result = subprocess.run([str(validator_binary())], input=pattern.encode("utf-8"),
                            capture_output=True)
    if result.returncode == 1:
        raise ValueError(f"invalid provider preset {field}: {name}: "
                         + result.stderr.decode("utf-8", errors="replace").strip())
    require(result.returncode == 0 and not result.stdout and not result.stderr,
            f"Rust regex validator failed for {name} {field}: exit {result.returncode}")
