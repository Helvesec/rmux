"""Causal coverage for the three R1-339 publication rehearsal findings."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SHA = "0123456789abcdef0123456789abcdef01234567"


def load(name):
    path = ROOT / "scripts/release" / (name + ".py")
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PublicationRehearsalCorrectionsTest(unittest.TestCase):
    def test_source_and_parent_cargo_config_refused_before_cargo(self):
        module = load("publication-dry-run")
        audit = ROOT / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        for level in ("source", "parent", "ancestor"):
            for filename in ("config", "config.toml"):
                with self.subTest(level=level, filename=filename), \
                     tempfile.TemporaryDirectory(dir=audit) as directory:
                    root = Path(directory)
                    source = root / "parent" / "source"
                    source.mkdir(parents=True)
                    owner = {"source": source, "parent": source.parent,
                             "ancestor": root}[level]
                    config = owner / ".cargo" / filename
                    config.parent.mkdir()
                    sentinel = root / "cargo-reached"
                    config.write_text('[build]\nrustc-wrapper = "fixture-wrapper"\n',
                                      encoding="utf-8")
                    args = argparse.Namespace(source_sha=SHA, source_dir=source,
                        version="1.2.3", release_ref="v1.2.3-rc.2",
                        target_dir=root / "rehearsal",
                        cargo=root / "toolchain" / ("cargo.exe" if os.name == "nt" else "cargo"))
                    metadata = {"workspace_members": ["rmux"], "packages": [
                        {"id": "rmux", "name": "rmux", "version": "1.2.3",
                         "publish": None, "dependencies": []}]}

                    def cargo(command, **kwargs):
                        sentinel.write_text("Cargo reached", encoding="utf-8")
                        return subprocess.CompletedProcess(command, 0,
                            json.dumps(metadata) if command[1] == "metadata" else "", "")

                    with patch.object(module.publisher, "validate_source_checkout", return_value=source), \
                         patch.object(module, "check_space"), \
                         patch.object(module.subprocess, "run", side_effect=cargo) as invoked:
                        with self.assertRaisesRegex(ValueError, r"Cargo configuration"):
                            module.execute(args)
                    invoked.assert_not_called()
                    self.assertFalse(sentinel.exists(), "parent wrapper reached Cargo")

    def test_acceptance_cargo_fixture_is_absolute_under_root_for_both_platforms(self):
        fixture = load("test_publication_dry_run")
        for platform in ("posix", "nt"):
            with self.subTest(platform=platform):
                calls = []

                def arguments(**kwargs):
                    cargo = kwargs["cargo"]
                    root = kwargs["source_dir"].parent
                    self.assertTrue(cargo.is_absolute())
                    self.assertTrue(cargo.is_relative_to(root), "Cargo fixture is outside root")
                    self.assertEqual(cargo.name, "cargo.exe" if platform == "nt" else "cargo")
                    calls.append(kwargs["release_ref"])
                    return argparse.Namespace(**kwargs)

                with patch.object(fixture, "argparse", SimpleNamespace(Namespace=arguments)), \
                     patch.object(fixture, "os", SimpleNamespace(
                         name=platform, environ=os.environ, devnull=os.devnull)):
                    case = fixture.PublicationDryRunTest(
                        "test_order_dependencies_isolation_and_no_remote_mutation")
                    case.test_order_dependencies_isolation_and_no_remote_mutation()
                self.assertEqual(calls, ["v0.10.0", "v1.2.3-rc.2"])

    def test_runbook_candidate_verifier_command_is_supported(self):
        text = (ROOT / "docs/release/publication-runbook.md").read_text(encoding="utf-8")
        command = re.search(r"python3 scripts/release/verify-candidate-artifacts\.py (\S+)", text)
        self.assertIsNotNone(command)
        completed = subprocess.run([sys.executable,
            str(ROOT / "scripts/release/verify-candidate-artifacts.py"),
            command.group(1), "--help"], capture_output=True, text=True, check=False)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("--downloads-dir", completed.stdout)


if __name__ == "__main__":
    unittest.main()
