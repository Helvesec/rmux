#!/usr/bin/env python3
"""Replay the finite closed-line regression registry from the native fast gates.

Receipt paths are historical locators, not runtime prerequisites or verdicts.
Use --list to inspect another OS; execution requires the native identity guard.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
DEFAULT = ROOT / "scripts/regression_registry.json"
PLATFORMS = {"linux", "macos", "windows"}


class RegistryError(ValueError):
    pass


def strings(value, label):
    if (not isinstance(value, list) or not value
            or any(not isinstance(item, str) or not item.strip() for item in value)
            or len(value) != len(set(value))):
        raise RegistryError(f"{label}: nonempty unique strings required")
    return value


def source_path(name, root):
    path = root / name
    if Path(name).is_absolute() or not path.resolve().is_relative_to(root.resolve()):
        raise RegistryError(f"source outside repository: {name}")
    if not path.is_file():
        raise RegistryError(f"missing protective source: {name}")
    return path


def cargo_test(command, argv):
    if (not isinstance(argv, list) or any(not isinstance(arg, str) or not arg for arg in argv)
            or argv[:2] != ["cargo", "test"] or "--locked" not in argv or "--" not in argv):
        raise RegistryError("locked Cargo test and libtest separator required")
    if any(Path(arg).name.startswith("gate-") or Path(arg).name == Path(__file__).name
           for arg in argv):
        raise RegistryError("recursive gate or registry command")
    if type(command["min_tests"]) is not int or command["min_tests"] < 1:
        raise RegistryError("positive protected test population required")
    strings(command["required_tests"], command["id"] + " required tests")


def documentary_links(value, receipts, root):
    # A grid marked “—” has review/test links, never an invented native cell.
    seen = set()
    for item in value.get("documentary", []):
        line = item["line"]
        if (not re.fullmatch(r"L[0-9]{2}\.[0-9]+", line) or line in seen
                or item["os"] != [] or not item["basis"]):
            raise RegistryError("invalid documentary closure")
        seen.add(line)
        strings(item["criteria"], line + " criteria")
        if not set(strings(item["receipts"], line + " receipts")) <= receipts.keys():
            raise RegistryError("unknown documentary receipt")
        sources = strings(item["sources"], line + " sources")
        for source in sources:
            source_path(source, root)
        for source, digest in item["reviewed_sources"].items():
            if (source not in sources or not re.fullmatch(r"[0-9a-f]{64}", digest)
                    or hashlib.sha256(source_path(source, root).read_bytes()).hexdigest() != digest):
                raise RegistryError("documentary source changed; independent review required")
        if not item["reviewed_sources"]:
            raise RegistryError("missing reviewed documentary sources")
        for gate in strings(item["gates"], line + " gates"):
            source_path(gate, root)
        for protector in item["protectors"]:
            if (protector["source"] not in sources
                    or protector["test"] not in source_path(protector["source"], root).read_text()):
                raise RegistryError("missing documentary protector")
        if not item["protectors"]:
            raise RegistryError("missing documentary protector")
    return seen


def load_registry(path=DEFAULT, root=ROOT):
    try:
        value = json.loads(path.read_text())
        if value["version"] != 1:
            raise RegistryError("unsupported registry version")
        receipts = value["receipts"]
        for receipt in receipts.values():
            if (not receipt["path"] or not re.fullmatch(r"[0-9a-f]{64}", receipt["sha256"])):
                raise RegistryError("receipt locator and SHA256 required")
        documentary = documentary_links(value, receipts, root)
        commands = value["commands"]
        if not isinstance(commands, list) or not commands:
            raise RegistryError("registry has no protective command")
        ids, invocations, covered = set(), set(), {}
        for command in commands:
            identifier = command["id"]
            argv = command["argv"]
            if (not isinstance(identifier, str) or not identifier.strip()
                    or not isinstance(argv, list) or len(argv) < 2
                    or any(not isinstance(arg, str) or not arg for arg in argv)):
                raise RegistryError("command identifier and argument vector required")
            if identifier in ids or tuple(argv) in invocations:
                raise RegistryError("duplicate protective command")
            ids.add(identifier)
            invocations.add(tuple(argv))
            if any(Path(arg).name.startswith("gate-") or Path(arg).name == Path(__file__).name
                   for arg in argv):
                raise RegistryError("recursive gate or registry command")
            sources = strings(command["sources"], identifier + " sources")
            for source in sources:
                source_path(source, root)
            if argv[0] == "python":
                if len(argv) < 2 or argv[1] not in sources:
                    raise RegistryError("Python command must name its protective source")
                if "test_argv" in command:
                    cargo_test(command, command["test_argv"])
            elif argv[0] == "cargo":
                if argv[1:2] == ["test"]:
                    cargo_test(command, argv)
                elif argv[1:] != ["deny", "check", "licenses"]:
                    raise RegistryError("unsupported Cargo protective command")
            else:
                raise RegistryError("unsupported protective tool")
            coverage = command["coverage"]
            if not isinstance(coverage, list) or not coverage:
                raise RegistryError("command has no criterion/cell/receipt coverage")
            cells = set()
            for item in coverage:
                if not re.fullmatch(r"L[0-9]{2}\.[0-9]+", item["line"]):
                    raise RegistryError("invalid line identifier")
                if item["line"] in documentary:
                    raise RegistryError("documentary closure cannot claim native coverage")
                strings(item["criteria"], identifier + " criteria")
                platforms = strings(item["os"], identifier + " OS")
                if not set(platforms) <= PLATFORMS:
                    raise RegistryError("unsupported registry OS")
                for platform in platforms:
                    cell = (item["line"], platform)
                    if cell in cells:
                        raise RegistryError("duplicate command coverage cell")
                    cells.add(cell)
                    covered.setdefault(cell, set()).add(identifier)
                if not set(strings(item["receipts"], identifier + " receipts")) <= receipts.keys():
                    raise RegistryError("unknown protective receipt")
        declared = {}
        for cell in value["cells"]:
            key = (cell["line"], cell["os"])
            if key in declared or cell["os"] not in PLATFORMS:
                raise RegistryError("duplicate or unsupported declared cell")
            declared[key] = set(strings(cell["commands"], "cell commands"))
        if declared != covered:
            raise RegistryError("declared cell has a missing or unassigned protective command")
        return commands
    except (KeyError, TypeError, AttributeError, OSError, json.JSONDecodeError) as error:
        raise RegistryError(f"missing or malformed regression registry: {error}") from error


def select(commands, platform):
    return [command for command in commands
            if any(platform in item["os"] for item in command["coverage"])]


def resolve_command(command):
    argv = command["argv"].copy()
    program = sys.executable if argv[0] == "python" else shutil.which(argv[0])
    if not program:
        raise RegistryError(f"missing protective tool: {argv[0]}")
    argv[0] = str(Path(program).absolute())
    return argv


def exit_code(code):
    return code if code >= 0 else 128 - code


def run_commands(commands, root=ROOT, env=None):
    # Resolve every selected tool before any command starts. Never run a shell.
    prepared = [(command, resolve_command(command)) for command in commands]
    for command, argv in prepared:
        print(f"[regression] {command['id']}: {json.dumps(argv)}", flush=True)
        if command["argv"] == ["python", "scripts/regression_registry_checks.py", "web-no-listener"]:
            preflight = subprocess.run(
                [argv[0], str(root / "scripts/web_no_listener_preflight.py")], cwd=root, env=env)
            if preflight.returncode:
                code = exit_code(preflight.returncode)
                print(f"[regression] {command['id']}: preflight exit={code}", flush=True)
                return code
        if command["argv"][:2] == ["cargo", "test"]:
            separator = argv.index("--")
            listing = argv[:separator + 1] + [*argv[separator + 1:], "--list"]
            listed = subprocess.run(listing, cwd=root, env=env, text=True, stdout=subprocess.PIPE)
            if listed.returncode:
                print(listed.stdout, end="", flush=True)
                return exit_code(listed.returncode)
            names = {line[:-6] for line in listed.stdout.splitlines() if line.endswith(": test")}
            missing = [name for name in command["required_tests"]
                       if not any(actual == name or actual.endswith("::" + name) for actual in names)]
            if len(names) < command["min_tests"] or missing:
                raise RegistryError(f"{command['id']}: protective selection missing: "
                                    f"{len(names)} tests, required names absent={missing}")
        ignored = command["argv"][:2] == ["cargo", "test"] and "--ignored" in argv
        result = subprocess.run(argv, cwd=root, env=env,
                                **(dict(text=True, stdout=subprocess.PIPE) if ignored else {}))
        if ignored:
            print(result.stdout, end="", flush=True)
        code = exit_code(result.returncode)
        print(f"[regression] {command['id']}: exit={code}", flush=True)
        if code:
            return code
        if ignored:
            passed = set(re.findall(r"^test (\S+) \.\.\. ok$", result.stdout, re.MULTILINE))
            if len(passed) < command["min_tests"] or not set(command["required_tests"]) <= passed:
                raise RegistryError(f"{command['id']}: required ignored test did not pass")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=sorted(PLATFORMS), required=True)
    parser.add_argument("--registry", type=Path, default=DEFAULT)
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--cut-repo", type=Path)
    parser.add_argument("--source-repo", type=Path)
    parser.add_argument("--cut-rule", type=Path)
    parser.add_argument("--trunk")
    parser.add_argument("--cut-output", type=Path)
    args = parser.parse_args()
    projection = (args.cut_repo, args.source_repo, args.cut_rule, args.trunk, args.cut_output)
    if any(projection):
        if not all(projection) or args.list or args.registry != DEFAULT:
            raise RegistryError("public cut replay requires all five cut arguments and the maintained registry")
        from public_cut_registry import replay
        return replay(args)
    commands = select(load_registry(args.registry), args.platform)
    if args.list:
        print(json.dumps(commands, indent=2))
        return 0
    native = {"darwin": "macos", "linux": "linux", "win32": "windows"}.get(sys.platform)
    if args.platform != native:
        raise RegistryError("protective execution requires the native OS; use --list to inspect")
    if os.environ.get("RMUX_REGRESSION_REGISTRY_ACTIVE"):
        raise RegistryError("recursive registry execution")
    if not commands:
        print(f"[regression] {args.platform}: NON_PROUVÉ; no accepted selectable closed-line cells")
        return 0
    if not os.environ.get("RMUX_IDENTITY_HEAD"):
        return exit_code(subprocess.run([
            sys.executable, str(ROOT / "scripts/candidate_identity.py"), "with", "--",
            sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]], cwd=ROOT).returncode)
    from candidate_identity import validate_context
    validate_context()
    os.environ["RMUX_REGRESSION_REGISTRY_ACTIVE"] = "1"
    return run_commands(commands)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RegistryError, OSError) as error:
        print(f"regression registry: {error}", file=sys.stderr)
        sys.exit(2)
