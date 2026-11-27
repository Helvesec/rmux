"""Acceptance test for the local publication rehearsal boundary."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SHA = "0123456789abcdef0123456789abcdef01234567"


class PublicationDryRunTest(unittest.TestCase):
    def test_order_dependencies_isolation_and_no_remote_mutation(self):
        path = ROOT / "scripts/release/publication-dry-run.py"
        self.assertTrue(path.is_file(), "local publication rehearsal is missing")
        spec = importlib.util.spec_from_file_location("publication_dry_run", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        # Keep the synthetic checkout outside the real checkout's Cargo ancestry,
        # in the writable temporary directory rather than the checkout's parent.
        with tempfile.TemporaryDirectory(prefix=".rmux-publication-") as directory:
            root = Path(directory).resolve()
            source = root / "source"
            source.mkdir()
            for version, release_ref in [("0.10.0", "v0.10.0"),
                                         ("1.2.3", "v1.2.3-rc.2")]:
                target = root / version
                args = argparse.Namespace(source_sha=SHA, source_dir=source,
                    version=version, release_ref=release_ref, target_dir=target,
                    cargo=root / "toolchain" / ("cargo.exe" if os.name == "nt" else "cargo"))
                packages = [
                    {"id": name, "name": name, "version": version,
                     "publish": publish, "dependencies": dependencies}
                    for name, publish, dependencies in [
                        ("rmux", None, [{"name": "rmux-bootstrap", "path": "/b", "kind": None}]),
                        ("rmux-bootstrap", None, [{"name": "rmux-types", "path": "/t", "kind": "build"}]),
                        ("rmux-types", None, [{"name": "rmux", "path": "/r", "kind": "dev", "req": "*"}]),
                        ("xtask", [], []),
                    ]
                ]
                metadata = {"workspace_members": [p["id"] for p in packages],
                            "packages": packages}
                calls = []

                def cargo(command, **kwargs):
                    self.assertEqual(Path(command[0]), args.cargo)
                    self.assertEqual(kwargs["cwd"], source)
                    env = kwargs["env"]
                    self.assertTrue(Path(env["HOME"]).is_relative_to(target))
                    self.assertTrue(Path(env["CARGO_HOME"]).is_relative_to(target))
                    self.assertEqual(env["GIT_CONFIG_GLOBAL"], os.devnull)
                    self.assertFalse(any("TOKEN" in key or "SECRET" in key or
                        "SSH" in key or key in {"RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"}
                        for key in env))
                    self.assertFalse((Path(env["CARGO_HOME"]) / "credentials.toml").exists())
                    if command[1] == "metadata":
                        self.assertIn("--locked", command)
                        return subprocess.CompletedProcess(command, 0, json.dumps(metadata), "")
                    self.assertEqual(command[1:], ["publish", "--dry-run", "--locked",
                                                   "--package", command[-1]])
                    calls.append(command[-1])
                    code = 1 if command[-1] == "rmux-bootstrap" else 0
                    return subprocess.CompletedProcess(command, code, "", "unpublished dependency" if code else "")

                with patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "fixture-secret",
                        "GITHUB_TOKEN": "fixture-secret", "SSH_AUTH_SOCK": "/identity",
                        "RUSTC_WRAPPER": "/unsafe-wrapper"}), \
                     patch.object(module.publisher, "validate_source_checkout", return_value=source), \
                     patch.object(module.subprocess, "run", side_effect=cargo), \
                     patch.object(module.publisher, "registry_bytes", side_effect=AssertionError("remote access")), \
                     patch.object(module.publisher, "run_cargo", side_effect=AssertionError("live publisher")):
                    report = module.execute(args)
                self.assertEqual(calls, ["rmux-types", "rmux-bootstrap", "rmux"])
                self.assertEqual(report["source_git_sha"], SHA)
                self.assertEqual(report["release_ref"], release_ref)
                self.assertEqual(report["version"], version)
                self.assertEqual(report["publish_order"], calls)
                self.assertEqual(report["workspace_dependencies"]["rmux-bootstrap"], ["rmux-types"])
                self.assertEqual([p["exit_code"] for p in report["results"]], [0, 1, 0])
                self.assertEqual(report["status"], "failed")
                self.assertEqual(json.loads((target / "publication-dry-run.json").read_text()), report)
                with patch.object(module.publisher, "validate_source_checkout", return_value=source), \
                     patch.object(module.subprocess, "run", side_effect=cargo):
                    with self.assertRaisesRegex(ValueError, "absent"):
                        module.execute(args)
                self.assertEqual(len(calls), 3)


if __name__ == "__main__":
    unittest.main()
