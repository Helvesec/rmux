"""Configuration boundary specs; no compiler required."""

import json
import os
from pathlib import Path
import re
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from public_api_gate import Runner, require_unconfigured
from public_api_policy import Cell


class ConfigurationSpec(unittest.TestCase):
    def setUp(self):
        # Exercise literal path matching on Unix as well as native Windows.
        self.temp = tempfile.TemporaryDirectory(prefix="cfg.[literal](root)+^$")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.home = self.root / "private-home"
        self.cwd = self.root / "source/output/consumer"
        self.cwd.mkdir(parents=True)
        self.home.mkdir()

    def config(self, directory, name="config.toml", text=""):
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / name
        path.write_text(text)
        self.addCleanup(path.unlink, missing_ok=True)
        return path

    def runner(self, environment=None):
        with patch.dict(os.environ, environment or {}, clear=True):
            return Runner(self.root / "proof", self.root / "tools", self.home,
                          self.root / "semver", "x86_64-unknown-linux-gnu")

    def test_both_filenames_at_every_invocation_location(self):
        for location in (self.cwd, self.cwd.parent, self.cwd.parent.parent, self.root):
            for name in ("config", "config.toml"):
                with self.subTest(location=location, name=name):
                    path = self.config(location / ".cargo", name)
                    with self.assertRaisesRegex(ValueError, re.escape(str(path)) + ";"):
                        require_unconfigured(self.cwd, self.home)
                    path.unlink()
        for name in ("config", "config.toml"):
            path = self.config(self.home, name)
            with self.assertRaisesRegex(ValueError, re.escape(str(path)) + ";"):
                require_unconfigured(self.cwd, self.home)
            path.unlink()

    def test_shadowed_file_and_unsupported_syntax_are_never_ignored(self):
        legacy = self.config(self.home, "config", "# harmless but unsupported\n")
        modern = self.config(self.home, text="not even valid TOML")
        with self.assertRaisesRegex(ValueError, re.escape(str(legacy)) + ";"):
            require_unconfigured(self.cwd, self.home)
        legacy.unlink()
        with self.assertRaisesRegex(ValueError, re.escape(str(modern)) + ";"):
            require_unconfigured(self.cwd, self.home)

    def test_workspace_does_not_load_member_or_dependency_configuration(self):
        member = self.cwd / "member"
        self.config(member / ".cargo", text="[env]\nANYTHING = 'blocked'\n")
        require_unconfigured(self.cwd, self.home)
        with self.assertRaises(ValueError):
            require_unconfigured(member, self.home)

    def test_cache_is_not_configuration_and_explicit_home_replaces_default(self):
        self.config(self.home / "registry/cache", "cached.crate", "registry bytes")
        self.config(self.root / "unused-home/.cargo", text="[build]\nrustflags = ['--cfg=bad']")
        require_unconfigured(self.cwd, self.home)

    @unittest.skipIf(os.name == "nt", "symlink creation requires separate native privileges")
    def test_symlinked_config_and_dangling_link_fail_closed(self):
        path = self.home / "config"
        path.symlink_to(self.root / "absent")
        with self.assertRaises(ValueError):
            require_unconfigured(self.cwd, self.home)
        (self.root / "absent").write_text("")
        with self.assertRaises(ValueError):
            require_unconfigured(self.cwd, self.home)

    def test_every_runner_tool_rejects_before_spawn_even_for_expected_failure(self):
        runner = self.runner()
        self.config(self.home, text="[build]\nrustflags = ['--cfg=bad']")
        for tool in (runner.cargo, runner.rustc, runner.rustdoc, runner.semver):
            with patch("public_api_gate.subprocess.run") as spawn:
                with self.assertRaisesRegex(ValueError, "unsupported Cargo configuration"):
                    runner.run(Path(tool).name, [tool, "--version"], self.cwd, expected=1)
                spawn.assert_not_called()
        receipts = json.loads((runner.output / "commands.json").read_text())
        self.assertEqual(len(receipts), 4)
        self.assertTrue(all(r["exit"] is None and r["rejected_configuration"] for r in receipts))

    def test_config_added_after_previous_command_is_rechecked(self):
        runner = self.runner()
        with patch("public_api_gate.subprocess.run") as spawn:
            spawn.return_value.returncode = 0
            spawn.return_value.stdout = "version\n"
            runner.run("first", [runner.cargo, "--version"], self.cwd)
            self.config(self.cwd / ".cargo")
            with self.assertRaises(ValueError):
                runner.run("second", [runner.cargo, "--version"], self.cwd)
            self.assertEqual(spawn.call_count, 1)

    def test_success_exit_without_output_cannot_reuse_stale_json(self):
        runner = self.runner()
        cell = Cell("rmux-sdk", "no-default")
        stale = runner.output / "old-target" / runner.target / "doc/rmux_sdk.json"
        stale.parent.mkdir(parents=True)
        stale.write_text("stale rustdoc JSON")
        with patch("public_api_gate.subprocess.run") as spawn:
            spawn.return_value.returncode = 0
            spawn.return_value.stdout = ""
            with self.assertRaises(FileNotFoundError):
                runner.document(self.cwd, cell, "old")
        self.assertFalse(stale.exists())
        self.assertFalse((runner.output / "old-rmux-sdk-no-default.json").exists())

    def test_mixed_case_overrides_reject_without_leaking_values(self):
        for key in ("Cargo", "cArGo_EnCoDeD_RuStFlAgS", "RustDocFlags", "RustC_Wrapper",
                    "Cargo_Target_X86_64_PC_WINDOWS_MSVC_Linker", "CARGO_PROFILE_DEV_PANIC"):
            with self.subTest(key=key):
                with patch.dict(os.environ, {key: "private-value"}, clear=True):
                    runner = Runner(self.root / key, self.root, self.home, self.root, "native")
                with patch("public_api_gate.subprocess.run") as spawn:
                    with self.assertRaisesRegex(ValueError, "ambient compiler configuration"):
                        runner.run("rejected", [runner.rustc, "--version"])
                    spawn.assert_not_called()
                receipt = (runner.output / "commands.json").read_text()
                self.assertNotIn("private-value", receipt)
                self.assertNotIn(key, runner.env)

    def test_native_setup_and_explicit_tools_survive_environment_normalization(self):
        native = {"Path": "native-tools", "LIB": "native-lib", "INCLUDE": "native-include",
                  "SDKROOT": "native-sdk", "LIBPATH": "native-libpath", "SYSTEMROOT": "system"}
        runner = self.runner({**native, "Cargo_Home": "unused", "RUSTUP_TOOLCHAIN": "unused"})
        self.assertFalse(runner.ambient_overrides)
        for key in set(native) - {"Path"}:
            self.assertEqual(runner.env[key], native[key])
        self.assertEqual(runner.env["PATH"], str(self.root / "tools") + os.pathsep + "native-tools")
        self.assertEqual(runner.env["CARGO_HOME"], str(self.home))
        self.assertEqual(runner.env["RUSTC"], runner.rustc)
        self.assertEqual(runner.env["RUSTDOC"], runner.rustdoc)
        self.assertNotIn("Path", runner.env)


if __name__ == "__main__":
    unittest.main()
