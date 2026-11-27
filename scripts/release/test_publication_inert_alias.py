"""Publication accepts only the inert repository task alias, never inherited config."""

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
ALIAS = '[alias]\nxtask = "run --locked -p xtask --"\n'


class PublicationInertAliasTest(unittest.TestCase):
    def setUp(self):
        path = ROOT / "scripts/release/publication-dry-run.py"
        spec = importlib.util.spec_from_file_location("publication_dry_run", path)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        # A fixture outside the checkout must not inherit its Cargo config; the
        # writable temporary directory is used because the parent may be read-only.
        directory = tempfile.TemporaryDirectory(prefix=".rmux-publication-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        self.source = self.root / "parent/source"
        self.source.mkdir(parents=True)
        self.args = argparse.Namespace(source_sha=SHA, source_dir=self.source,
            version="1.2.3", release_ref="v1.2.3-rc.2", target_dir=self.root / "rehearsal",
            cargo=self.root / "toolchain" / ("cargo.exe" if os.name == "nt" else "cargo"))
        self.calls = []

    def config(self, owner=None, name="config.toml", content=ALIAS):
        path = (owner or self.source) / ".cargo" / name
        path.parent.mkdir(exist_ok=True)
        path.write_bytes(content.encode("utf-8"))
        return path

    def cargo(self, command, **kwargs):
        self.calls.append(command)
        self.assertEqual(Path(command[0]), self.args.cargo)
        self.assertEqual(kwargs["cwd"], self.source)
        self.assertTrue(Path(kwargs["env"]["CARGO_HOME"]).is_relative_to(self.args.target_dir))
        if command[1] == "metadata":
            metadata = {"workspace_members": ["a", "b"], "packages": [
                {"id": name, "name": name, "version": "1.2.3",
                 "publish": None, "dependencies": []} for name in ("a", "b")]}
            return subprocess.CompletedProcess(command, 0, json.dumps(metadata), "")
        self.assertEqual(command[1:], ["publish", "--dry-run", "--locked", "--package", command[-1]])
        return subprocess.CompletedProcess(command, 0, "", "")

    def execute(self, cargo=None):
        with patch.object(self.module.publisher, "validate_source_checkout", return_value=self.source), \
             patch.object(self.module.subprocess, "run", side_effect=cargo or self.cargo), \
             patch.object(self.module.publisher, "registry_bytes", side_effect=AssertionError("remote access")), \
             patch.object(self.module.publisher, "run_cargo", side_effect=AssertionError("live publication")):
            return self.module.execute(self.args)

    def assert_refused(self):
        with self.assertRaisesRegex(ValueError, "Cargo configuration"):
            self.execute()
        self.assertEqual(self.calls, [])
        self.assertFalse(self.args.target_dir.exists())

    def test_repository_alias_rehearses_every_crate_with_dry_run_only(self):
        self.assertEqual((ROOT / ".cargo/config.toml").read_text(), ALIAS)
        self.config()
        report = self.execute()
        self.assertEqual(report["status"], "success")
        self.assertEqual(report["publish_order"], ["a", "b"])
        self.assertEqual([call[1] for call in self.calls], ["metadata", "publish", "publish"])
        self.assertEqual([result["exit_code"] for result in report["results"]], [0, 0])

    def test_only_exact_alias_value_and_table_are_inert(self):
        for name in ("config", "config.toml"):
            for content in (ALIAS, "# task only\r\n[alias]\r\nxtask = 'run --locked -p xtask --'\r\n"):
                with self.subTest(name=name, content=content):
                    path = self.config(name=name, content=content)
                    self.module.reject_cargo_configuration(self.source)
                    path.unlink()

    def test_absent_configuration_remains_accepted(self):
        self.assertEqual(self.execute()["status"], "success")

    def test_active_unknown_and_malformed_configuration_never_reaches_cargo(self):
        invalid = ["", "not TOML", "[alias]\n", ALIAS.replace("--locked ", ""),
            ALIAS.replace("run --locked -p xtask --", "!echo unexpected"),
            ALIAS + 'extra = "check"\n', ALIAS + '[build]\nrustc-wrapper = "wrapper"\n',
            ALIAS + '[env]\nRUSTC_BOOTSTRAP = "1"\n', ALIAS + '[unknown]\n',
            ALIAS + 'xtask = "check"\n', '[alias]\nxtask = ["run", "--locked", "-p", "xtask", "--"]\n']
        for name in ("config", "config.toml"):
            for content in invalid:
                with self.subTest(name=name, content=content):
                    path = self.config(name=name, content=content)
                    self.assert_refused()
                    path.unlink()

    def test_even_exact_alias_in_parent_or_ancestor_is_refused(self):
        for owner in (self.source.parent, self.root):
            for name in ("config", "config.toml"):
                with self.subTest(owner=owner, name=name):
                    path = self.config(owner=owner, name=name)
                    self.assert_refused()
                    path.unlink()

    def test_active_legacy_config_is_not_hidden_by_inert_toml(self):
        self.config()
        self.config(name="config", content='[build]\nrustc-wrapper = "wrapper"\n')
        self.assert_refused()

    def test_config_symlinks_and_non_files_are_refused(self):
        target = self.root / "alias.toml"
        target.write_text(ALIAS)
        directory = self.source / ".cargo"
        directory.mkdir()
        for name in ("config", "config.toml"):
            path = directory / name
            for destination in (target, self.root / "absent"):
                with self.subTest(name=name, destination=destination):
                    path.symlink_to(destination)
                    self.assert_refused()
                    path.unlink()
            path.mkdir()
            self.assert_refused()
            path.rmdir()

    def test_cargo_directory_symlink_is_refused(self):
        target = self.root / "cargo-config"
        target.mkdir()
        (target / "config.toml").write_text(ALIAS)
        (self.source / ".cargo").symlink_to(target, target_is_directory=True)
        self.assert_refused()

    def test_guard_is_rechecked_after_metadata_and_between_publications(self):
        config = self.config()
        for trigger in ("metadata", "publish"):
            with self.subTest(trigger=trigger):
                config.write_text(ALIAS)
                self.calls = []
                self.args.target_dir = self.root / trigger

                def mutate(command, **kwargs):
                    result = self.cargo(command, **kwargs)
                    if command[1] == trigger:
                        config.write_text(ALIAS + '[build]\nrustc-wrapper = "wrapper"\n')
                    return result

                with self.assertRaisesRegex(ValueError, "Cargo configuration"):
                    self.execute(mutate)
                self.assertEqual(len(self.calls), 1 if trigger == "metadata" else 2)
                report = json.loads((self.args.target_dir / "publication-dry-run.json").read_text())
                self.assertEqual(report["status"], "interrupted")


if __name__ == "__main__":
    unittest.main()
