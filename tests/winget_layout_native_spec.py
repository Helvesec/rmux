#!/usr/bin/env python3
"""Exercise generated WinGet manifests with native Windows PowerShell validators.

Pass --powershell once for each absolute interpreter path, --bash for Git Bash,
and a new --work-dir for retained fixtures and JSON evidence. No package is
downloaded, installed, or executed. Checksums are deliberately synthetic.
An optional --validator path allows the same oracle to check a parent revision.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


VERSION = "0.10.0"
PACKAGE = f"rmux-{VERSION}-windows-x86_64"
CANONICAL_PATH = f"{PACKAGE}\\rmux.exe"
CHANGED_PATH = f"{PACKAGE}\\bin\\rmux-next.exe"
CHANGED_ALIAS = "rmux-next"
CHECKSUM = "a" * 64
CASES = (
    ("canonical", False, False, None, None),
    ("changed-path", True, False, None, None),
    ("changed-alias", False, True, None, None),
    ("changed-path-and-alias", True, True, None, None),
    ("stale-path", True, False, ("RelativeFilePath", CANONICAL_PATH), "unexpected RelativeFilePath"),
    ("stale-alias", False, True, ("PortableCommandAlias", "rmux"), "unexpected PortableCommandAlias"),
    ("wrong-checksum", False, False, ("InstallerSha256", "b" * 64), "InstallerSha256 mismatch"),
    ("wrong-schema", False, False, ("schema", "1.9.0"), "unexpected yaml-language-server schema"),
    ("wrong-version", False, False, ("PackageVersion", "0.10.1"), "unexpected PackageVersion"),
    ("wrong-url", False, False, ("InstallerUrl", "https://example.invalid/rmux.zip"), "unexpected InstallerUrl"),
)


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def pin(path: Path) -> dict:
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def replace_field(body: str, key: str, value: str) -> str:
    pattern = re.compile(r"^(\s*(?:-\s*)?" + re.escape(key) + r":\s*).+$", re.MULTILINE)
    body, count = pattern.subn(lambda match: match[1] + value, body)
    if count != 1:
        raise ValueError(f"expected one {key}, found {count}")
    return body


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--powershell", type=Path, action="append", required=True)
    parser.add_argument("--bash", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--validator", type=Path)
    options = parser.parse_args()
    if os.name != "nt":
        parser.error("this regression requires native Windows")
    for executable in [*options.powershell, options.bash]:
        if not executable.is_absolute() or not executable.is_file():
            parser.error(f"expected an existing absolute executable: {executable}")

    source = Path(__file__).resolve().parents[1]
    validator = options.validator or source / "scripts/validate-winget-manifest.ps1"
    if not validator.is_file():
        parser.error(f"validator not found: {validator}")
    work = options.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=False)
    report = {"start": now(), "source": str(source), "validator": pin(validator),
              "test": pin(Path(__file__).resolve()), "synthetic_checksums": True,
              "commands": [], "cases": []}

    def save() -> None:
        (work / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    def run(argv: list[str], environment: dict[str, str]) -> dict:
        record = {"argv": argv, "start": now()}
        try:
            child = subprocess.run(argv, capture_output=True, stdin=subprocess.DEVNULL,
                                   env=environment, timeout=45)
            record.update(exit=child.returncode,
                          stdout=child.stdout.decode("utf-8", errors="replace"),
                          stderr=child.stderr.decode("utf-8", errors="replace"))
        except subprocess.TimeoutExpired:
            record.update(exit=None, stdout="", stderr="child exceeded 45 seconds")
        record["end"] = now()
        report["commands"].append(record)
        save()
        return record

    environment = os.environ.copy()
    environment["RMUX_PYTHON"] = Path(sys.executable).as_posix()
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PYTHONUTF8"] = "1"
    copies = ["scripts/generate-winget-manifest.sh", "scripts/packaging/layout",
              "scripts/packaging/layout.py", "scripts/packaging/layout-tool.ps1",
              "scripts/packaging/layout_dispatch.py", "scripts/packaging/layout_profiles.py",
              "scripts/packaging/layout_rpm.py",
              "scripts/packaging/rmux-package-layout.json"]
    for name, changed_path, changed_alias, corruption, expected_error in CASES:
        fixture = work / name
        for relative in copies:
            target = fixture / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source / relative, target)
        copied_validator = fixture / "scripts/validate-winget-manifest.ps1"
        shutil.copyfile(validator, copied_validator)
        descriptor = fixture / "scripts/packaging/rmux-package-layout.json"
        if changed_path or changed_alias:
            layout = json.loads(descriptor.read_text(encoding="utf-8"))
            if changed_path:
                cli = next(entry for entry in layout["entries"] if entry["id"] == "cli")
                cli["variants"]["windows-archive"]["path"] = "bin/rmux-next.exe"
            if changed_alias:
                layout["downstream"]["winget"]["portable_command_alias"] = CHANGED_ALIAS
            descriptor.write_text(json.dumps(layout, indent=2) + "\n", encoding="utf-8")
        checksums = fixture / "SHA256SUMS"
        checksums.write_text(f"{CHECKSUM}  {PACKAGE}.zip\n", encoding="utf-8")
        manifest = fixture / "manifest/Helvesec.RMUX.yaml"
        generation = run([str(options.bash), (fixture / copies[0]).as_posix(),
                          "--version", VERSION, "--checksums", checksums.as_posix(),
                          "--release-date", "2026-09-26", "--output", manifest.as_posix()], environment)
        if generation["exit"] != 0:
            raise RuntimeError(f"manifest generation failed: {name}: {generation}")
        installer = manifest.with_name("Helvesec.RMUX.installer.yaml")
        body = installer.read_text(encoding="utf-8")
        expected_path = CHANGED_PATH if changed_path else CANONICAL_PATH
        expected_alias = CHANGED_ALIAS if changed_alias else "rmux"
        if (f"RelativeFilePath: {expected_path}\n" not in body or
                f"PortableCommandAlias: {expected_alias}\n" not in body):
            raise RuntimeError(f"generator did not project fixture values: {name}")
        if corruption:
            key, value = corruption
            if key == "schema":
                body = body.replace("winget-manifest.installer.1.10.0.schema.json",
                                    f"winget-manifest.installer.{value}.schema.json")
            else:
                body = replace_field(body, key, value)
            installer.write_text(body, encoding="utf-8")
        inputs = [pin(path) for path in [descriptor, copied_validator, checksums,
                                        *sorted(manifest.parent.glob("*.yaml"))]]
        for powershell in options.powershell:
            outcome = run([str(powershell), "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                           "-File", str(copied_validator), "-Manifest", str(manifest),
                           "-Version", VERSION, "-Checksums", str(checksums)], environment)
            output = outcome["stdout"] + outcome["stderr"]
            if expected_error:
                passed = (outcome["exit"] is not None and outcome["exit"] != 0 and expected_error in output)
            else:
                passed = outcome["exit"] == 0 and f"WinGet manifest OK: Helvesec.RMUX {VERSION} multi-file" in output
            report["cases"].append({"case": name, "powershell": str(powershell),
                                    "expected_error": expected_error, "exit": outcome["exit"],
                                    "passed": passed, "inputs": inputs,
                                    "command_index": len(report["commands"]) - 1})
            print(f"{'PASS' if passed else 'FAIL'} {name}: {powershell}", flush=True)
            save()
    report["end"] = now()
    report["passed"] = all(case["passed"] for case in report["cases"])
    save()
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
