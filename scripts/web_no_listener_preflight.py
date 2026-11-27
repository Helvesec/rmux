#!/usr/bin/env python3
"""Qualify the exact CLI graph before the no-listener probe's runtime deadline."""
import json
from pathlib import Path
import shutil
import subprocess
import sys

import candidate_identity as identity


ROOT = Path(__file__).resolve().parents[1]
BUILD_ARGS = ["build", "--locked", "--offline", "-p", "rmux", "--bin", "rmux",
              "--features", "rmux-sdk/default", "--message-format=json"]


def qualify(binary, source, head, target):
    # Checking only Cargo's alias leaves the retained producer's first check
    # inside new-session's deadline when the CLI selects its hidden daemon.
    record = identity.verify_built(binary, source, head, target)
    built = json.loads(Path(record["build"]).read_text())
    producer = Path(built["producer"] if built["kind"] == "unix-launcher" else built["path"])
    guard = [sys.executable, str(ROOT / "scripts/candidate_identity.py"), "check"]
    for path in dict.fromkeys((binary, producer)):
        selected = subprocess.run([*guard, str(path)], cwd=ROOT, check=True,
                                  text=True, stdout=subprocess.PIPE).stdout.strip()
        subprocess.run([*guard, selected], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)


def main():
    identity.validate_context()
    source, head, target = identity.context()
    if sys.platform != "linux":
        raise RuntimeError("no-listener preflight requires Linux")
    cargo = shutil.which("cargo")
    if cargo is None:
        raise RuntimeError("missing protective tool: cargo")
    # A workspace/default build may select different features. Build the same
    # graph the unchanged probe will request, then qualify its actual artifact.
    built = subprocess.run([cargo, *BUILD_ARGS], cwd=ROOT, check=True,
                           text=True, stdout=subprocess.PIPE)
    binaries = []
    for line in built.stdout.splitlines():
        item = json.loads(line)
        if (item.get("reason") == "compiler-artifact" and item["target"]["name"] == "rmux"
                and item.get("executable")):
            binaries.append(Path(item["executable"]))
    if len(binaries) != 1:
        raise RuntimeError("CLI build did not identify a unique executable")
    qualify(binaries[0], source, head, target)
    print("[regression] web-no-listener: exact CLI identity prequalified", flush=True)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode if error.returncode >= 0 else 128 - error.returncode)
    except (OSError, ValueError, KeyError, RuntimeError, identity.IdentityError) as error:
        print(f"no-listener preflight: {error}", file=sys.stderr)
        sys.exit(2)
