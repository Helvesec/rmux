"""Product gate regression; compiler execution is tested separately on native hosts."""
import importlib.util
from contextlib import redirect_stdout
import io
import itertools
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]


def metadata():
    return {"workspace_members": ["cli", "sdk", "plain"], "packages": [
        {"id": "cli", "name": "rmux", "features": {
            "default": ["web"], "web": [], "tiny-cli": [], "kitty-keyboard": []}},
        {"id": "sdk", "name": "rmux-sdk", "features": {
            "default": ["web"], "web": [], "regex": [], "crossterm": []}},
        {"id": "plain", "name": "rmux-types", "features": {}},
        {"id": "external", "name": "external", "features": {"unrelated": []}},
    ]}


class FeaturePowerset(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location(
            "check_feature_powerset", ROOT / "scripts/check_feature_powerset.py")
        self.gate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.gate)

    def test_workspace_acceptance_and_isolated_full_tiny_sdk_powersets(self):
        commands = self.gate.commands(metadata())
        self.assertEqual(commands[:3], [
            ["cargo", "check", "--workspace", "--all-targets", "--locked", *flags]
            for flags in (["--no-default-features"], [], ["--all-features"])])
        expected = []
        for name, features in (
            ("rmux", ("kitty-keyboard", "tiny-cli", "web")),
            ("rmux-sdk", ("crossterm", "regex", "web")),
            ("rmux-types", ()),
        ):
            prefix = ["cargo", "check", "-p", name, "--all-targets", "--locked"]
            expected.extend([prefix, prefix + ["--all-features"]])
            for size in range(len(features) + 1):
                for subset in itertools.combinations(features, size):
                    expected.append(prefix + ["--no-default-features"] +
                                    (["--features", ",".join(subset)] if subset else []))
        self.assertEqual(commands[3:-2], expected)
        self.assertEqual(commands[-2:], [
            ["cargo", "check", "-p", "rmux", "--release", "--bin", "rmux",
             "--locked", "--features", "tiny-cli", *flags]
            for flags in ([], ["--no-default-features"])])

    def test_feature_added_to_metadata_is_checked_without_editing_the_gate(self):
        value = metadata()
        value["packages"][0]["features"]["new-feature"] = []
        commands = self.gate.commands(value)
        selected = [c for c in commands if "-p" in c and "--release" not in c
                    and c[c.index("-p") + 1] == "rmux"]
        self.assertEqual(len(selected), 18)
        self.assertIn("kitty-keyboard,new-feature,tiny-cli,web", selected[-1])

    def test_incomplete_or_duplicate_workspace_metadata_is_rejected(self):
        for variant in ("empty", "missing", "duplicate-member", "duplicate-package"):
            value = metadata()
            if variant == "empty":
                value["workspace_members"] = []
            elif variant == "missing":
                value["packages"].pop(0)
            elif variant == "duplicate-member":
                value["workspace_members"].append("cli")
            else:
                value["packages"].append(value["packages"][0])
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                self.gate.commands(value)

    def test_execution_preserves_windows_target_and_checks_every_command(self):
        value = metadata()
        result = subprocess.CompletedProcess([], 0, stdout=json.dumps(value))
        with patch.object(self.gate.subprocess, "run", return_value=result) as run, redirect_stdout(io.StringIO()):
            self.gate.main(["--target", "x86_64-pc-windows-msvc"])
        checks = run.call_args_list[1:]
        self.assertEqual([c.args[0] for c in checks], [
            c + ["--target", "x86_64-pc-windows-msvc"] for c in self.gate.commands(value)])
        self.assertTrue(all(c.kwargs["check"] for c in run.call_args_list))
        self.assertTrue(all(Path(c.kwargs["cwd"]) == ROOT for c in run.call_args_list))

    def test_compiler_or_metadata_failure_cannot_be_reported_green(self):
        value = metadata()
        result = subprocess.CompletedProcess([], 0, stdout=json.dumps(value))
        for failure_index in (0, 1, 6, len(self.gate.commands(value)) - 1,
                              len(self.gate.commands(value))):
            error = subprocess.CalledProcessError(101, ["cargo", "check"])
            effects = [result] * failure_index + [error]
            with self.subTest(failure_index=failure_index):
                with patch.object(self.gate.subprocess, "run", side_effect=effects) as run, redirect_stdout(io.StringIO()):
                    with self.assertRaises(subprocess.CalledProcessError):
                        self.gate.main([])
                self.assertEqual(run.call_count, failure_index + 1)


class ProductGateWiring(unittest.TestCase):
    def test_standard_and_release_gates_invoke_the_compilation_acceptance(self):
        for name in ("gate-unix-fast.sh", "gate-windows-fast.ps1",
                     "release-review-gate.sh", "release-review-gate-windows.ps1"):
            with self.subTest(gate=name):
                source = (ROOT / "scripts" / name).read_text()
                self.assertIn("scripts/check_feature_powerset.py", source)
                self.assertIn("test_feature_powerset.py", source)
                if name == "gate-windows-fast.ps1":
                    self.assertIn("scripts/check_feature_powerset.py --target $Target", source)

    if os.name == "posix":
        def test_unix_gates_reach_and_propagate_workspace_and_tiny_compile_failures(self):
            # Execute the real entrypoints with a compiler spy. This proves gate
            # control flow only; it is deliberately not native compiler evidence.
            audit = ROOT / ".rmux-audit" / "feature-gate-fixtures"
            audit.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=audit) as directory:
                fixture = Path(directory)
                (fixture / "metadata.json").write_text(json.dumps(metadata()))
                python = fixture / "python"
                python.write_text(
                    "#!" + sys.executable + "\nimport os, subprocess, sys\n"
                    "if 'scripts/check_feature_powerset.py' in sys.argv:\n"
                    " sys.exit(subprocess.call([sys.executable, *sys.argv[1:]]))\n"
                    "sys.exit(0)\n")
                cargo = fixture / "cargo"
                cargo.write_text(
                    "#!" + sys.executable + "\nimport json, os, pathlib, sys\n"
                    "root=pathlib.Path(__file__).parent\nargs=sys.argv[1:]\n"
                    "with (root/'calls.jsonl').open('a') as f: f.write(json.dumps(args)+'\\n')\n"
                    "if args[0]=='metadata':\n"
                    " print((root/'metadata.json').read_text()); sys.exit(0)\n"
                    "if args[0]=='check':\n"
                    " if os.environ['FEATURE_FAILURE']=='workspace' and '--workspace' in args "
                    "and '--no-default-features' in args: sys.exit(73)\n"
                    " if os.environ['FEATURE_FAILURE']=='tiny' and '-p' in args "
                    "and args[args.index('-p')+1]=='rmux' and '--features' in args "
                    "and args[args.index('--features')+1]=='tiny-cli': sys.exit(73)\n"
                    " if os.environ['FEATURE_FAILURE'].startswith('release-tiny') "
                    "and '--release' in args and '--bin' in args "
                    "and args[args.index('--bin')+1]=='rmux' "
                    "and '--features' in args and args[args.index('--features')+1]=='tiny-cli' "
                    "and ('--no-default-features' in args)=="
                    "(os.environ['FEATURE_FAILURE']=='release-tiny-minimal'): sys.exit(73)\n"
                    "sys.exit(74 if args[0]=='clippy' else 0)\n")
                node = fixture / "node"
                node.write_text("#!/bin/sh\nexit 0\n")
                for tool in (python, cargo, node):
                    tool.chmod(0o755)
                env = dict(os.environ, PATH=str(fixture) + os.pathsep + os.environ["PATH"],
                           RMUX_PYTHON=str(python), RMUX_XTERM_NODE=str(node),
                           RMUX_IDENTITY_HEAD="feature-gate-fixture", CARGO_BUILD_JOBS="1",
                           CARGO_TARGET_DIR=str(fixture / "target"))
                for script, args in (
                    ("gate-unix-fast.sh", ["--platform", "macos", "--test-threads", "1"]),
                    ("gate-unix-fast.sh", ["--platform", "linux", "--test-threads", "1",
                                           "--skip-tiny-routes"]),
                    ("release-review-gate.sh", ["--section", "lint"]),
                ):
                    for failure in ("workspace", "tiny", "release-tiny", "release-tiny-minimal", "none"):
                        with self.subTest(script=script, args=args, failure=failure):
                            log = fixture / "calls.jsonl"
                            log.write_text("")
                            env["FEATURE_FAILURE"] = failure
                            result = subprocess.run(
                                ["/bin/bash", str(ROOT / "scripts" / script), *args],
                                cwd=ROOT, env=env, text=True, capture_output=True, timeout=30)
                            self.assertEqual(result.returncode, 74 if failure == "none" else 73,
                                             result.stdout + result.stderr)
                            calls = [json.loads(line) for line in log.read_text().splitlines()]
                            if failure == "none":
                                self.assertEqual(calls[-1][0], "clippy")
                                self.assertEqual(sum("--release" in c for c in calls), 2)
                            else:
                                self.assertEqual(calls[-1][0], "check")
                                self.assertNotIn("clippy", [c[0] for c in calls])
                                if failure.startswith("release-tiny"):
                                    self.assertIn("--release", calls[-1])
                                    self.assertNotIn("--all-targets", calls[-1])
                                    self.assertEqual(calls[-1][calls[-1].index("--bin") + 1], "rmux")
                                else:
                                    self.assertIn("--all-targets", calls[-1])
                                    self.assertIn("--no-default-features", calls[-1])


if __name__ == "__main__":
    unittest.main()
