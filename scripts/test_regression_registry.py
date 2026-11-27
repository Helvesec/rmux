"""REV3.1 L01.5: closed-line regressions must run from both fast gates."""
import hashlib
import json
import copy
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import io
from contextlib import redirect_stdout
from unittest.mock import patch

import regression_registry as registry
registry_module = registry
import regression_registry_checks as checks


ROOT = Path(__file__).resolve().parents[1]


# Reviewed baselines of scripts/regression_registry.json. The registry only
# grows: a baseline's commands and cells stay an exact prefix of the current
# ones, and each of its receipts keeps its SHA256 (the path is a historical
# locator that publication may rename). Commands and cells are pinned by the
# SHA256 of their canonical JSON, so the check holds in a published checkout
# where the reviewed commits are not reachable (one squashed release commit).
RECEIPT_SHA256 = {
    "tree-closed": "15e368ef05280996ab04ee7605f877f2f30b09ced35e5c5092d711243f2537f4",
    "tree-commands": "b417fbda6465682e5f2cefdbd7e45daa297493854d5b54d860926516dd195849",
    "web-closed": "5e059fd6e37e8cb799efc90381a37042d5fcb758e988c3775ba37e08d6874f10",
    "web-review": "cb5ff3ca2e422803e19a867943a71acae1df0d6759a63994ae9a0b6d388f4dfb",
    "web-author": "e5ff081b87bc15f6dfcac75d74418b967dbccad1177dbe54291dfbdbd3e64c26",
    "supply-review": "b8f7053c6a04128bc62b1f8c06fccf4d7c9b9ed649ac3812f28875bee4fc2ca0",
    "supply-author": "e58388beb738fa5e3b0a1085da7e3487b3fbf008d2ec0968b89394fc8459187c",
    "supply-native": "0b7466b9b915d0d792c581d45f9c42e43208c2d179bd6e569e3773370902dccf",
    "formats-closed": "fa4852df7a8313e3fb3b860c5f00c00f64b0bbec48f3a8a894ec93cefcfa70bb",
    "formats-prior": "c22970345afcad41601c51ac8d2bc762ea94be437864a04748cdab16ca126822",
    "formats-commands": "025113c5e0c4aba4e56826949661738f87fa62c808e39f68e429126895658db5",
    "send-keys-closed": "85a973c1924057103b9fbdaa17e9efe0a429fba8e906326349f8940022fcb441",
    "send-keys-commands": "1d8914ec983ba32515c199a3289d43901857a0e90675332f1e2f37fb1bacb61e",
    "ratchets-closed": "c96c1ca31a49039a3dfb40295621bb0b636eed3698662be50dc4bbcbc80c239a",
    "ratchets-suite": "311f0bfb7816e29f6c6526713208c2ac570fa24dc2aab5e6a3bfb08b9e0d89c6",
    "ratchets-windows-review": "832d1c7599d5a4c08244828ff320577ca8cdc9ab8a447f9be7541ef286193f96",
    "ratchets-windows-recipe": "e1e26c355f58b692074c54af6bb1447ef1db04a94eaad8843967ef9cf508b64f",
}
REGISTRY_BASELINES = {
    "62070a9d4f829bbb4587b2c140165883794000c1": dict(
        commands=(7, "71fe6c7b828a43c7cc5cc1e16b81928f11cb7301e5b02921563e96ba4f86ca87"),
        receipts=("tree-closed", "tree-commands", "web-closed", "web-review", "web-author",
                  "supply-review", "supply-author", "supply-native")),
    "a902a7dccd3c8ffd695c10936b6dbaf74d07c13f": dict(
        commands=(13, "55b5c4ce95d6c504c642f789ed5bdefff30f1c9093218bc58f862cc225fb1c3e"),
        cells=(9, "aa6591d3720ecef65bcfd884553dd409383dc12b0bdffba8faa43f62fd03086b"),
        receipts=tuple(RECEIPT_SHA256)),
}


def canonical_digest(value):
    text = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def assert_extends_baseline(test, value, commit):
    """The registry value still holds the pinned baseline of a reviewed commit."""
    pin = REGISTRY_BASELINES[commit]
    for section in ("commands", "cells"):
        if section in pin:
            count, digest = pin[section]
            test.assertGreaterEqual(len(value[section]), count, (commit, section))
            test.assertEqual(canonical_digest(value[section][:count]), digest, (commit, section))
    for name in pin["receipts"]:
        receipt = value["receipts"].get(name)
        test.assertIsNotNone(receipt, (commit, name))
        test.assertEqual(receipt["sha256"], RECEIPT_SHA256[name], (commit, name))
        test.assertEqual(set(receipt), {"path", "sha256"}, (commit, name))
        test.assertTrue(receipt["path"] and not receipt["path"].startswith("/"), receipt["path"])


class RegistryAcceptance(unittest.TestCase):
    def test_r1620_new_closures_have_exact_links_without_native_kitty_credit(self):
        value = json.loads(registry.DEFAULT.read_text())
        assert_extends_baseline(self, value, "a902a7dccd3c8ffd695c10936b6dbaf74d07c13f")
        links = [item for command in registry.load_registry() for item in command["coverage"]
                 if item["line"] == "L08.1"]
        self.assertTrue(links, "PASS510 L08.1 has no executable regression protector")
        self.assertEqual({c for item in links for c in item["criteria"]}, {"2A.options"})
        self.assertEqual({os for item in links for os in item["os"]}, {"linux"})
        for item in links:
            self.assertIn("options-closed", item["receipts"])
        self.assertEqual(value["receipts"]["options-closed"], dict(
            path="historical-projects/audit-archive-20260928/results/"
                 "R1-INT270-C4-H-U-NATIVE-REVIEW-510/REVUE.md",
            sha256="aa305ea28fc1e824f627f1daea394f6fa3ac921f39d70ee86e336134fb6d143a"))
        self.assertEqual(value["receipts"]["kitty-closed"], dict(
            path="historical-projects/audit-archive-20260928/results/"
                 "R1-L115-D5-CENTRAL-CLOSURE-REVIEW-523/REVUE.md",
            sha256="7bbbe74499f1b91916194cbd1a102b2958df667a8babd6ccd01b443d4845e15f"))
        kitty = next(item for item in value["documentary"] if item["line"] == "L11.5")
        self.assertEqual(kitty["criteria"], ["3.kitty-native"])
        self.assertEqual(kitty["os"], [])
        self.assertEqual(kitty["basis"], "D5 fallback; L11.4 REPORTÉE D71")
        self.assertEqual(kitty["receipts"], ["kitty-closed"])
        self.assertIn("crates/rmux-core/tests/kitty_fallback.rs", kitty["sources"])
        self.assertIn("xtask/src/msrv/tests.rs", kitty["sources"])
        self.assertTrue(all(item["line"] != "L11.5" for command in value["commands"]
                            for item in command["coverage"]))
        self.assertTrue(all(cell["line"] != "L11.5" for cell in value["cells"]))

    def test_r1442_windows_and_ignored_recipe_are_selected(self):
        commands = registry_module.load_registry()
        with self.subTest(finding="F1"):
            windows = registry_module.select(commands, "windows")
            self.assertEqual([item["id"] for item in windows], ["source-inventory-ratchets"])
            self.assertEqual(windows[0]["coverage"][0]["line"], "L01.4")
        with self.subTest(finding="F2"):
            linux = registry_module.select(commands, "linux")
            bank = next((item for item in linux if item["id"] == "formats-core-options-ignored"), None)
            self.assertIsNotNone(bank, "INT118 core-options-ignored must execute, not merely list")
            self.assertEqual(bank["argv"], ["python", "scripts/regression_registry_checks.py", "options-bank"])
            self.assertEqual(bank["required_tests"], [
                "options::tests::registry_metadata::generated_options_bank_inventory"])
            self.assertEqual(bank["min_tests"], 1)
            self.assertEqual(bank["test_argv"], ["cargo", "test", "--locked", "--offline",
                "-p", "rmux-core", bank["required_tests"][0], "--", "--ignored", "--exact"])

    def test_closed_lines_have_protective_commands_in_both_fast_gates(self):
        registry = ROOT / "scripts/regression_registry.json"
        self.assertTrue(registry.is_file(), "REV3.1 L01.5 registry is absent")
        entries = json.loads(registry.read_text())["commands"]
        self.assertTrue(entries)
        covered = set()
        for entry in entries:
            self.assertTrue(entry["argv"])
            self.assertTrue(entry["sources"])
            for coverage in entry["coverage"]:
                covered.add(coverage["line"])
                for field in ("criteria", "os", "receipts"):
                    self.assertTrue(coverage[field], (entry["id"], field))
        self.assertEqual(covered, {"L01.4", "L08.1", "L08.6", "L08.7", "L12.4", "L16.2", "L14.5"})
        expected = {
            "L01.4": ({"transverse"}, {"linux", "macos", "windows"}),
            "L08.1": ({"2A.options"}, {"linux"}),
            "L08.6": ({"2A.formats-rest"}, {"linux"}),
            "L08.7": ({"2A.formats-rest", "2A.send-f"}, {"linux", "macos"}),
            "L12.4": ({"3.composition"}, {"linux"}),
            "L16.2": ({"4.web"}, {"linux"}),
            "L14.5": ({"4.supply"}, {"linux"}),
        }
        for line, (criteria, platforms) in expected.items():
            items = [item for entry in entries for item in entry["coverage"]
                     if item["line"] == line]
            self.assertEqual({name for item in items for name in item["criteria"]}, criteria)
            self.assertEqual({name for item in items for name in item["os"]}, platforms)
        receipts = json.loads(registry.read_text())["receipts"]
        for filename in (
                "R1-L086-C3-S1077-CLOSURE-REVIEW-210/REVUE.md",
                "R1-INT118-GROUP-NATIVE-FF-REVIEW-216/REVUE.md",
                "R1-L014-INT151-LANDED-CLOSURE-REVIEW-290/REVUE.md"):
            self.assertTrue(any(item["path"].endswith(filename) for item in receipts.values()))
        assert_extends_baseline(self, json.loads(registry.read_text()),
                                "62070a9d4f829bbb4587b2c140165883794000c1")
        by_id = {entry["id"]: entry for entry in entries}
        for name, mode in (("independent-formats-corpus", "formats-corpus"),
                           ("source-inventory-ratchets", "source-inventory")):
            self.assertEqual(by_id[name]["argv"],
                             ["python", "scripts/regression_registry_checks.py", mode])
        self.assertEqual(by_id["send-keys-cli"]["min_tests"], 7)
        self.assertEqual(by_id["send-keys-lib"]["min_tests"], 11)
        self.assertEqual(by_id["send-keys-lib"]["argv"], [
            "cargo", "test", "--locked", "--offline", "-p", "rmux-server", "--lib", "--",
            "handler::tests::send_keys_tests::format_expansion::",
            "handler::tests::send_keys_tests::basic_dispatch::"
            "send_keys_plain_input_uses_copy_mode_until_copy_mode_exits",
            "handler::tests::send_keys_tests::basic_dispatch::send_keys_k_",
            "handler::tests::send_keys_tests::basic_dispatch::"
            "switch_client_t_sets_custom_key_table_for_next_k_dispatch", "--test-threads=1"])
        commands = json.loads(registry.read_text())["commands"]
        self.assertIn(["cargo", "test", "--locked", "--offline", "-p", "rmux",
                       "--test", "send_keys_formats", "--", "--test-threads=1"],
                      [entry["argv"] for entry in commands])
        self.assertTrue(any(entry["argv"] == ["cargo", "test", "--locked", "--offline",
                            "-p", "rmux-core", "--lib", "--", "--test-threads=6"]
                            and entry["min_tests"] == 1600 for entry in commands))
        self.assertEqual([item["id"] for item in registry_module.select(
            registry_module.load_registry(), "windows")], ["source-inventory-ratchets"])
        for gate in ("gate-unix-fast.sh", "gate-windows-fast.ps1"):
            text = (ROOT / "scripts" / gate).read_text()
            self.assertEqual(text.count("regression_registry.py"), 1, gate)
            self.assertIn("--platform", text, gate)

    def test_baseline_pins_are_the_reviewed_registry_blobs(self):
        for commit, pin in REGISTRY_BASELINES.items():
            with self.subTest(commit=commit):
                shown = subprocess.run(
                    ["git", "show", f"{commit}:scripts/regression_registry.json"],
                    cwd=ROOT, text=True, capture_output=True)
                if shown.returncode != 0:
                    self.skipTest(f"{commit} is not reachable in this checkout")
                baseline = json.loads(shown.stdout)
                for section in ("commands", "cells"):
                    if section in pin:
                        self.assertEqual(pin[section][0], len(baseline[section]), section)
                self.assertEqual(set(pin["receipts"]), set(baseline["receipts"]))
                assert_extends_baseline(self, baseline, commit)


class RegistryBehavior(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.script = self.root / "probe.py"
        self.script.write_text("import sys\nsys.exit(int(sys.argv[1]))\n")
        self.command = dict(id="fixture", argv=["python", "probe.py", "0"], sources=["probe.py"],
                            coverage=[dict(line="L12.4", criteria=["3.composition"],
                                           os=["macos"], receipts=["retained"])])
        self.value = dict(version=1, receipts={"retained": dict(path="historical-receipt",
                                                              sha256="a" * 64)},
                          cells=[dict(line="L12.4", os="macos", commands=["fixture"])],
                          commands=[self.command])

    def load(self):
        path = self.root / "registry.json"
        path.write_text(json.dumps(self.value))
        return registry.load_registry(path, self.root)

    def test_missing_registry_command_source_or_receipt_is_refused(self):
        with self.assertRaises(registry.RegistryError):
            registry.load_registry(self.root / "absent", self.root)
        for defect in ("commands", "source", "receipt", "population", "tool", "os"):
            with self.subTest(defect=defect):
                saved = copy.deepcopy(self.value)
                if defect == "commands":
                    self.value["commands"] = []
                elif defect == "source":
                    self.command["sources"] = ["missing.py"]
                elif defect == "receipt":
                    self.command["coverage"][0]["receipts"] = ["missing"]
                elif defect == "population":
                    self.command["argv"] = ["cargo", "test", "--locked", "--"]
                    self.command["min_tests"] = 0
                    self.command["required_tests"] = ["protected"]
                elif defect == "tool":
                    self.command["argv"][0] = "sh"
                else:
                    self.command["coverage"][0]["os"] = ["windows10"]
                with self.assertRaises(registry.RegistryError):
                    self.load()
                self.value = saved
                self.command = self.value["commands"][0]

    def test_missing_executable_prevents_any_command(self):
        missing = copy.deepcopy(self.command)
        missing["argv"] = ["cargo", "deny", "check", "licenses"]
        with patch.object(registry.shutil, "which", return_value=None), \
                patch.object(registry.subprocess, "run") as run:
            with self.assertRaisesRegex(registry.RegistryError, "missing protective tool"):
                registry.run_commands([self.command, missing], self.root)
            run.assert_not_called()

    def test_real_failure_exit_is_preserved_and_later_command_is_not_run(self):
        self.command["argv"][-1] = "37"
        later = copy.deepcopy(self.command)
        later["argv"] = ["python", "absent.py"]
        self.assertEqual(registry.run_commands([self.command, later], self.root), 37)

    def test_real_success_runs_once_with_absolute_interpreter(self):
        self.script.write_text("from pathlib import Path\np=Path('calls'); "
                               "p.write_text(p.read_text()+'x' if p.exists() else 'x')\n")
        commands = self.load()
        self.assertEqual(registry.run_commands(commands, self.root), 0)
        self.assertEqual((self.root / "calls").read_text(), "x")
        self.assertTrue(Path(registry.resolve_command(commands[0])[0]).is_absolute())

    def test_os_selection_has_no_duplicate_dispatch(self):
        self.command["coverage"][0]["os"] = ["macos", "windows"]
        extra = copy.deepcopy(self.command["coverage"][0])
        extra["line"] = "L16.2"
        self.command["coverage"].append(extra)
        self.value["cells"] = [dict(line=line, os=platform, commands=["fixture"])
                               for line in ("L12.4", "L16.2") for platform in ("macos", "windows")]
        commands = self.load()
        self.assertEqual(registry.select(commands, "linux"), [])
        for platform in ("macos", "windows"):
            self.assertEqual(registry.select(commands, platform), commands)

    def test_duplicate_and_recursive_commands_are_refused(self):
        duplicate = copy.deepcopy(self.command)
        duplicate["id"] = "different-id"
        self.value["commands"].append(duplicate)
        with self.assertRaisesRegex(registry.RegistryError, "duplicate"):
            self.load()
        self.value["commands"].pop()
        for name in ("scripts/gate-unix-fast.sh", "scripts/gate-windows-fast.ps1",
                     "scripts/regression_registry.py"):
            self.command["argv"][1] = name
            with self.assertRaisesRegex(registry.RegistryError, "recursive"):
                self.load()

    def test_removing_one_declared_protective_command_is_refused(self):
        self.value["cells"][0]["commands"].append("missing-protective-command")
        with self.assertRaisesRegex(registry.RegistryError, "missing or unassigned"):
            self.load()

    def test_cli_rejects_missing_registry_foreign_os_and_recursive_run(self):
        script = ROOT / "scripts/regression_registry.py"
        native = {"darwin": "macos", "linux": "linux", "win32": "windows"}[sys.platform]
        foreign = "windows" if native != "windows" else "macos"
        for args, extra, message in (
                (["--platform", native, "--registry", str(self.root / "absent")], {}, "malformed"),
                (["--platform", foreign], {}, "native OS"),
                (["--platform", native], {"RMUX_REGRESSION_REGISTRY_ACTIVE": "1"}, "recursive")):
            result = subprocess.run([sys.executable, str(script), *args], cwd=ROOT,
                                    env=dict(os.environ, **extra), text=True, capture_output=True)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn(message, result.stderr)

    def test_cargo_empty_or_missing_protective_selection_is_refused(self):
        self.command.update(argv=["cargo", "test", "--locked", "--", "protected"],
                            min_tests=1, required_tests=["protected"])
        for output in ("", "different: test\n"):
            with patch.object(registry.shutil, "which", return_value="/fixture/cargo"), \
                    patch.object(registry.subprocess, "run",
                                 return_value=subprocess.CompletedProcess([], 0, output)) as run:
                with self.assertRaisesRegex(registry.RegistryError, "protective selection missing"):
                    registry.run_commands([self.command], self.root)
                self.assertEqual(run.call_count, 1)

    def test_cargo_listing_and_test_failures_propagate(self):
        self.command.update(argv=["cargo", "test", "--locked", "--", "protected"],
                            min_tests=1, required_tests=["protected"])
        for outcomes in ([subprocess.CompletedProcess([], 71, "")],
                         [subprocess.CompletedProcess([], 0, "module::protected: test\n"),
                          subprocess.CompletedProcess([], 71)]):
            with patch.object(registry.shutil, "which", return_value="/fixture/cargo"), \
                    patch.object(registry.subprocess, "run", side_effect=outcomes) as run:
                self.assertEqual(registry.run_commands([self.command], self.root), 71)
                self.assertEqual(run.call_count, len(outcomes))
        self.assertEqual(registry.exit_code(-15), 143)

    def test_delegated_cargo_population_and_recursion_are_checked_by_loader(self):
        self.command.update(test_argv=["cargo", "test", "--locked", "--", "protected"],
                            min_tests=1, required_tests=["protected"])
        self.load()
        for defect in ("locked", "separator", "tool", "population", "required", "recursive"):
            with self.subTest(defect=defect):
                saved = copy.deepcopy(self.command)
                if defect == "locked":
                    self.command["test_argv"].remove("--locked")
                elif defect == "separator":
                    self.command["test_argv"].remove("--")
                elif defect == "tool":
                    self.command["test_argv"][0] = "sh"
                elif defect == "population":
                    self.command["min_tests"] = 0
                elif defect == "required":
                    self.command["required_tests"] = []
                else:
                    self.command["test_argv"].append("scripts/gate-windows-fast.ps1")
                with self.assertRaises(registry.RegistryError):
                    self.load()
                self.command.clear()
                self.command.update(saved)

    def test_ignored_cargo_requires_actual_pass_and_preserves_original_exit(self):
        self.command.update(argv=["cargo", "test", "--locked", "protected", "--",
                                  "--ignored", "--exact"], min_tests=1,
                            required_tests=["protected"])
        for output, code, error in (("test protected ... ok\n", 0, None),
                                    ("test protected ... ignored\n", 0, registry.RegistryError),
                                    ("test other ... ok\n", 0, registry.RegistryError),
                                    ("", 0, registry.RegistryError), ("", 71, None)):
            with self.subTest(output=output, code=code), \
                    patch.object(registry.shutil, "which", return_value="/fixture/cargo"), \
                    patch.object(registry.subprocess, "run", side_effect=[
                        subprocess.CompletedProcess([], 0, "protected: test\n"),
                        subprocess.CompletedProcess([], code, output)]) as run, \
                    redirect_stdout(io.StringIO()):
                env = {"RMUX_OPTIONS_BANK_OUTPUT": "fresh"}
                if error:
                    with self.assertRaises(error):
                        registry.run_commands([self.command], self.root, env=env)
                else:
                    self.assertEqual(registry.run_commands([self.command], self.root, env=env), code)
                self.assertEqual(run.call_args_list[0].args[0][-3:],
                                 ["--ignored", "--exact", "--list"])
                self.assertEqual(run.call_args_list[1].args[0][-2:], ["--ignored", "--exact"])
                self.assertTrue(all(call.kwargs["env"] == env for call in run.call_args_list))

    def test_committed_registry_selects_only_accepted_native_recipes(self):
        commands = registry.load_registry()
        self.assertEqual(len(registry.select(commands, "linux")), 14)
        self.assertEqual({command["id"] for command in registry.select(commands, "macos")},
                         {"send-keys-cli", "send-keys-lib", "source-inventory-ratchets"})
        self.assertEqual([item["id"] for item in registry.select(commands, "windows")],
                         ["source-inventory-ratchets"])

    def test_empty_native_selection_is_explicitly_unproven_without_identity_credit(self):
        output = io.StringIO()
        with patch.object(registry, "select", return_value=[]), \
                patch.object(sys, "platform", "win32"), \
                patch.object(sys, "argv", ["registry", "--platform", "windows"]), \
                patch.dict(os.environ, {}, clear=True), \
                patch("candidate_identity.validate_context") as validate, \
                patch.object(registry, "run_commands") as execute, redirect_stdout(output):
            self.assertEqual(registry.main(), 0)
        self.assertIn("NON_PROUVÉ", output.getvalue())
        self.assertNotIn("PASS", output.getvalue())
        validate.assert_not_called()
        execute.assert_not_called()

    def test_independent_corpus_delegates_the_accepted_recipe_on_current_head(self):
        calls = []
        receipt = self.root / "attested-launcher.json"
        receipt.write_text(json.dumps(dict(kind="unix-launcher",
                                          producer=str(self.root / "current-image"))))

        def observed(argv, **kwargs):
            calls.append(list(argv))
            if "bounded-corpus" in argv or "plan" in argv:
                Path(argv[-1]).write_text("fixture prospective input")
            if "bounded-linux" in argv and "oracle" in argv:
                Path(argv[6]).mkdir()
                (Path(argv[6]) / "RESULT.json").write_text("fixture measured oracle")
            if "bounded-linux" in argv and "candidate" in argv:
                frozen = json.loads(Path(argv[argv.index("--oracle") + 1]).read_text())
                self.assertEqual(Path(frozen["receipt"]["path"]).read_text(), "fixture measured oracle")
                self.assertEqual(frozen["receipt"]["bytes"], len(b"fixture measured oracle"))
            return subprocess.CompletedProcess(argv, 0, str(self.root / "current-launcher") + "\n")

        with patch.object(checks, "ROOT", self.root), \
                patch.object(checks.sys, "platform", "linux"), \
                patch.object(checks.identity, "context", return_value=(self.root, "current-head", self.root)), \
                patch.object(checks.identity, "verify_built", return_value={"build": str(receipt)}), \
                patch.object(checks, "run", side_effect=observed):
            checks.formats_corpus(self.root)
        self.assertEqual(calls[0], ["cargo", "build", "--locked", "--offline",
                                    "--no-default-features", "-p", "rmux", "--bin", "rmux",
                                    "--bin", "rmux-daemon"])
        contract = calls[2]
        self.assertIn("current-head", contract)
        self.assertIn("/usr/local/bin/tmux", contract)
        self.assertIn(self.root / "current-image", contract)
        self.assertEqual([argv[3] for argv in calls[2:6]],
                         ["bounded-corpus", "bounded-reader", "bounded-linux", "bounded-linux"])
        self.assertIn("oracle", calls[4])
        self.assertIn("candidate", calls[5])
        self.assertIn("--oracle", calls[5])
        self.assertEqual(calls[6][1:], ["-B", "-m", "unittest", "discover", "-s",
                                       "tests/format_jobs_reference", "-v"])
        with patch.object(checks.sys, "platform", "darwin"), patch.object(checks, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "accepted Linux"):
                checks.formats_corpus(self.root)
            run.assert_not_called()

    def test_source_inventory_uses_existing_standalone_authority_and_preserves_parent(self):
        env = dict(RMUX_IDENTITY_HEAD="current-head", RMUX_IDENTITY_SOURCE=str(self.root),
                   RMUX_IDENTITY_TARGET=str(self.root), RMUX_IDENTITY_RUSTC="/fixture/rustc",
                   RMUX_IDENTITY_RUSTDOC="/fixture/rustdoc", RUSTC_WRAPPER="/fixture/wrapper",
                   CARGO_TARGET_DIR=str(self.root), CARGO_TARGET_FIXTURE_RUNNER="parent-runner",
                   CARGO_HOME="offline-cache", RMUX_REGRESSION_REGISTRY_ACTIVE="1",
                   RMUX_LEDGER_EVIDENCE="/fixture/proofs")
        ledger = json.loads((ROOT / "scripts/source-inventory/production-ledger.json").read_text())
        with patch.object(checks, "require_ledger_evidence", return_value=self.root) as require, \
                patch.object(checks.identity, "context", return_value=(self.root, "current-head", self.root)), \
                patch.object(checks.identity, "support") as support, \
                patch.dict(os.environ, env, clear=True), patch.object(checks, "run") as run:
            support.return_value.binding.return_value = None
            checks.source_inventory(self.root)
            require.assert_called_once_with(ledger, os.environ)
            self.assertEqual(dict(os.environ), env)
        argv = run.call_args.args[0]
        self.assertEqual(argv[2:5], ["with-manifest",
                                    self.root / "scripts/source-inventory/Cargo.toml", "--"])
        fresh = run.call_args.kwargs["env"]
        self.assertFalse(any(name.startswith(("RMUX_IDENTITY_", "CARGO_TARGET_")) for name in fresh))
        self.assertNotIn("RUSTC_WRAPPER", fresh)
        self.assertEqual(fresh["RMUX_LEDGER_EVIDENCE"], "/fixture/proofs")
        self.assertEqual(fresh["CARGO_HOME"], "offline-cache")
        self.assertEqual(fresh["RUSTDOC"], "/fixture/rustdoc")
        self.assertEqual(fresh["RMUX_REGRESSION_REGISTRY_ACTIVE"], "1")

    def test_source_inventory_refuses_empty_skipped_or_failed_canaries(self):
        names = ("test_new_maintained_tooling_and_exact_size_boundary",
                 "test_cli_size_ratchet_and_reduction_checks_fail",
                 "test_d65_blacklist_and_production_ratchet", *checks.LEDGER_PROOF_TESTS)
        valid = "".join(name + " (fixture.Case.test) ... ok\n" for name in names)
        valid += "Ran 50 tests in 0.01s\nOK\n"
        without_proofs = "".join(name + " (fixture.Case.test) ... ok\n" for name in names[:3])
        without_proofs += "Ran 50 tests in 0.01s\nOK\n"
        cases = ((valid, 0, None, self.root), ("Ran 0 tests in 0.01s\nOK\n", 0, RuntimeError, self.root),
                 (valid.replace("... ok", "... skipped", 1), 0, RuntimeError, self.root),
                 (valid.replace(checks.LEDGER_PROOF_TESTS[0], "other"), 0, RuntimeError, self.root),
                 (valid.replace(checks.LEDGER_PROOF_TESTS[0] + " (fixture.Case.test) ... ok",
                                checks.LEDGER_PROOF_TESTS[0] + " (fixture.Case.test) ... skipped 'proof'"),
                  0, RuntimeError, self.root),
                 (valid.replace("Ran 50 tests in 0.01s\nOK\n", "Ran 52 tests in 0.01s\nOK (skipped=3)\n"),
                  0, RuntimeError, self.root),
                 (valid, 37, subprocess.CalledProcessError, self.root),
                 (without_proofs, 0, RuntimeError, self.root), (without_proofs, 0, None, None))
        for output, code, error, evidence in cases:
            with self.subTest(code=code, output=output, evidence=evidence), \
                    patch.object(checks, "require_ledger_evidence", return_value=evidence), \
                    patch.object(checks.identity, "context", return_value=(self.root, "current-head", self.root)), \
                    patch.object(checks.identity, "support") as support, \
                    patch.object(checks.identity, "verify_built", return_value={"fixture": "current-head"}), \
                    patch.object(checks, "run", return_value=subprocess.CompletedProcess(
                        [], 0, str(self.root / "guarded-parser") + "\n")), \
                    patch.object(checks.subprocess, "run",
                         return_value=subprocess.CompletedProcess([], code, output)) as run, \
                    redirect_stdout(io.StringIO()):
                support.return_value.binding.return_value = {
                    "manifest": str(self.root / "scripts/source-inventory/Cargo.toml")}
                if error is None:
                    checks.source_inventory(self.root)
                else:
                    with self.assertRaises(error):
                        checks.source_inventory(self.root)
                self.assertEqual(run.call_args.args[0][1:], ["-B", "-m", "unittest", "discover",
                    "-s", "scripts/source-inventory", "-p", "test*.py", "-v"])
                self.assertEqual(run.call_args.kwargs["env"]["SOURCE_SYNTAX"],
                                 str(self.root / "guarded-parser"))
        # The real rule and ledger: a missing proof is red before any identity or Cargo step.
        for value, message in ((None, "proofs require RMUX_LEDGER_EVIDENCE"), ("", "absolute"),
                               ("relative-proofs", "absolute"), (str(self.root / "absent"), "absolute"),
                               (str(self.root), "ledger proof sha256:")):
            env = {} if value is None else {"RMUX_LEDGER_EVIDENCE": value}
            with self.subTest(evidence=value), patch.dict(os.environ, env, clear=True), \
                    patch.object(checks.identity, "context") as context, \
                    patch.object(checks, "run") as run, patch.object(checks.subprocess, "run") as tested:
                with self.assertRaisesRegex(RuntimeError, message):
                    checks.source_inventory(self.root)
                context.assert_not_called()
                run.assert_not_called()
                tested.assert_not_called()

    def test_windows_source_inventory_qualifies_the_native_exe(self):
        names = ("test_new_maintained_tooling_and_exact_size_boundary",
                 "test_cli_size_ratchet_and_reduction_checks_fail", "test_d65_blacklist_and_production_ratchet",
                 *checks.LEDGER_PROOF_TESTS)
        output = "".join(name + " (fixture.Case.test) ... ok\n" for name in names)
        output += "Ran 50 tests in 0.01s\nOK\n"
        binary = self.root / "debug/rmux-source-syntax.exe"
        with patch.object(checks.sys, "platform", "win32"), \
                patch.object(checks, "require_ledger_evidence", return_value=self.root), \
                patch.object(checks.identity, "context", return_value=(self.root, "current-head", self.root)), \
                patch.object(checks.identity, "support") as support, \
                patch.object(checks.identity, "verify_built", return_value={"head": "current-head"}) as verify, \
                patch.object(checks, "run", return_value=subprocess.CompletedProcess([], 0, str(binary) + "\n")) as run, \
                patch.object(checks.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, output)) as tested, \
                redirect_stdout(io.StringIO()):
            support.return_value.binding.return_value = {
                "manifest": str(self.root / "scripts/source-inventory/Cargo.toml")}
            checks.source_inventory(self.root)
        self.assertEqual(run.call_args_list[0].args[0], ["cargo", "build", "--locked", "--offline",
            "--manifest-path", self.root / "scripts/source-inventory/Cargo.toml", "--jobs", "2"])
        self.assertEqual(run.call_args_list[1].args[0][-1], binary)
        self.assertEqual(verify.call_count, 2)
        self.assertTrue(all(call.args == (binary, self.root, "current-head", self.root)
                            for call in verify.call_args_list))
        self.assertEqual(tested.call_args.kwargs["env"]["SOURCE_SYNTAX"], str(binary))
        self.assertEqual(tested.call_args.kwargs["env"].get("PYTHONUTF8"), "1")

    def test_options_bank_delegates_exact_recipe_with_fresh_head_output(self):
        import hashlib
        oracle = self.root / "options-table.c"
        oracle.write_bytes(b"authenticated by retained Rust test")
        parent = dict(os.environ, RMUX_FROZEN_OPTIONS_TABLE=str(oracle),
                      RMUX_OPTIONS_BANK_OUTPUT="old-output", RMUX_IDENTITY_HEAD="current-head",
                      RMUX_REGRESSION_REGISTRY_ACTIVE="1")
        paths = []

        def execute(commands, root, env):
            self.assertEqual(root, ROOT)
            self.assertEqual(commands[0]["argv"][-2:], ["--ignored", "--exact"])
            self.assertEqual(commands[0]["argv"][6], commands[0]["required_tests"][0])
            path = Path(env["RMUX_OPTIONS_BANK_OUTPUT"])
            self.assertTrue(path.is_relative_to(self.root))
            self.assertFalse(path.exists())
            self.assertEqual(env["RMUX_FROZEN_OPTIONS_TABLE"], str(oracle.resolve()))
            self.assertEqual(env["RMUX_IDENTITY_HEAD"], "current-head")
            self.assertEqual(env["RMUX_REGRESSION_REGISTRY_ACTIVE"], "1")
            paths.append(path)
            path.write_text(json.dumps(dict(target_os="linux", registry_count=153,
                oracle=dict(source_sha256=hashlib.sha256(oracle.read_bytes()).hexdigest()))))
            return 0

        with patch.dict(os.environ, parent, clear=True), \
                patch.object(checks.sys, "platform", "linux"), \
                patch.object(registry, "run_commands", side_effect=execute), \
                redirect_stdout(io.StringIO()):
            checks.options_bank(self.root)
            checks.options_bank(self.root)
            self.assertEqual(dict(os.environ), parent)
        self.assertNotEqual(paths[0], paths[1])
        with patch.object(checks.sys, "platform", "win32"), \
                patch.object(registry, "run_commands") as run:
            with self.assertRaisesRegex(RuntimeError, "accepted Linux"):
                checks.options_bank(self.root)
            run.assert_not_called()

    def test_options_bank_rejects_missing_export_and_propagates_cargo_exit(self):
        oracle = self.root / "options-table.c"
        oracle.write_text("oracle")
        for code, error in ((0, FileNotFoundError), (71, subprocess.CalledProcessError)):
            with self.subTest(code=code), patch.object(checks.sys, "platform", "linux"), \
                    patch.dict(os.environ, RMUX_FROZEN_OPTIONS_TABLE=str(oracle)), \
                    patch.object(registry, "run_commands", return_value=code):
                with self.assertRaises(error) as caught:
                    checks.options_bank(self.root)
                if code:
                    self.assertEqual(caught.exception.returncode, code)

    def test_sdk_probe_recipe_declares_a_guarded_binary_link(self):
        artifacts = [dict(reason="compiler-artifact", target=dict(name=name), filenames=[str(path)])
                     for name, path in (("rmux_sdk", self.root / "librmux_sdk.rlib"),
                                        ("tokio", self.root / "libtokio.rlib"))]
        artifacts.append(dict(reason="compiler-artifact", target=dict(name="rmux"),
                              executable=str(self.root / "rmux")))
        calls = []

        class CompileObserved(Exception):
            pass

        def observed(argv, **kwargs):
            calls.append(argv)
            if len(calls) == 1:
                return subprocess.CompletedProcess(argv, 0, "\n".join(map(json.dumps, artifacts)))
            raise CompileObserved()

        with patch.object(checks, "run", side_effect=observed), \
                patch.object(checks.shutil, "which", return_value="/fixture/cargo"), \
                patch.dict(os.environ, {"RMUX_IDENTITY_RUSTC": "/fixture/rustc"}):
            with self.assertRaises(CompileObserved):
                checks.web_no_listener(self.root)
        compiled = calls[1]
        self.assertEqual(compiled[compiled.index("--crate-type") + 1], "bin")
        self.assertIn("rustc", compiled)
        self.assertIn(ROOT / "scripts/candidate_identity.py", compiled)

    def test_reviewed_threat_model_change_is_refused(self):
        document = self.root / "threat.md"
        document.write_bytes(b"## RMUX threat model\nreviewed text\n")
        heading = "## RMUX threat model\n"
        import hashlib
        digest = hashlib.sha256(b"reviewed text\n").hexdigest()
        scripts = self.root / "scripts"
        scripts.mkdir()
        (scripts / "regression_registry.json").write_text(json.dumps(dict(
            threat_model=dict(path="threat.md", heading=heading, sha256=digest))))
        with patch.object(checks, "ROOT", self.root), \
                patch.object(checks.identity, "validate_context"), \
                patch.object(checks.identity, "context", return_value=(self.root, "head", self.root)), \
                patch.object(sys, "argv", ["checks", "threat-model"]):
            checks.main()
            document.write_bytes(b"## RMUX threat model\nunreviewed text\n")
            with self.assertRaisesRegex(RuntimeError, "reviewed threat model changed"):
                checks.main()

    def unix_gate_dispatch_fixture(self, node_exit=0):
        # Dispatch fixture: no Rust tool, native image or identity credit.
        scripts = self.root / "scripts"
        scripts.mkdir()
        gate = scripts / "gate-unix-fast.sh"
        shutil.copyfile(ROOT / "scripts/gate-unix-fast.sh", gate)
        python = self.root / "python-stub"
        python.write_text('#!/bin/sh\ncase "$*" in\n'
                          '*regression_registry.py*) printf "%s\\n" "$*" >> calls; exit 37;;\n'
                          '*) exit 0;;\nesac\n')
        python.chmod(0o755)
        for tool in ("cargo", "node"):
            path = self.root / tool
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        (self.root / "node").write_text(
            '#!/bin/sh\nprintf "%s\\n" "$*" >> node-calls\n'
            'exit ' + str(node_exit) + '\n')
        # The gate's explicit Node selector takes precedence over PATH.
        env = dict(os.environ, RMUX_IDENTITY_HEAD="dispatch-fixture", RMUX_PYTHON=str(python),
                   RMUX_XTERM_NODE=str(self.root / "node"),
                   PATH=str(self.root) + os.pathsep + os.environ["PATH"], CARGO_BUILD_JOBS="1")
        return gate, env

    @unittest.skipUnless(os.name == "posix", "Unix shell entry needs Unix")
    def test_unix_gate_dispatches_once_and_propagates_registry_exit(self):
        gate, env = self.unix_gate_dispatch_fixture()
        for platform in ("macos", "linux"):
            with self.subTest(platform=platform):
                result = subprocess.run(["/bin/bash", str(gate), "--platform", platform,
                                         "--test-threads", "1"], env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 37, result.stdout + result.stderr)
        self.assertEqual((self.root / "calls").read_text().splitlines(),
                         ["scripts/regression_registry.py --platform macos",
                          "scripts/regression_registry.py --platform linux"])
        self.assertEqual((self.root / "node-calls").read_text().splitlines(),
                         ["tests/xterm-oracle/viewport-oracle.mjs --check"] * 2)

    @unittest.skipUnless(os.name == "posix", "Unix shell entry needs Unix")
    def test_unix_gate_dispatch_fixture_owns_node_without_changing_parent(self):
        external = self.root / "external-node"
        external.write_text('#!/bin/sh\ntouch external-node-called\nexit 99\n')
        external.chmod(0o755)
        with patch.dict(os.environ, RMUX_XTERM_NODE=str(external)):
            parent = dict(os.environ)
            self.test_unix_gate_dispatches_once_and_propagates_registry_exit()
            self.assertEqual(dict(os.environ), parent)
        self.assertFalse((self.root / "external-node-called").exists())

    @unittest.skipUnless(os.name == "posix", "Unix shell entry needs Unix")
    def test_unix_gate_node_failure_stops_before_registry(self):
        gate, env = self.unix_gate_dispatch_fixture(node_exit=43)
        for platform in ("macos", "linux"):
            with self.subTest(platform=platform):
                result = subprocess.run(["/bin/bash", str(gate), "--platform", platform,
                                         "--test-threads", "1"], env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 43, result.stdout + result.stderr)
                self.assertFalse((self.root / "calls").exists())
        self.assertEqual((self.root / "node-calls").read_text().splitlines(),
                         ["tests/xterm-oracle/viewport-oracle.mjs --check"] * 2)

    def test_powershell_dispatch_preserves_identity_forwarding_and_exit(self):
        text = (ROOT / "scripts/gate-windows-fast.ps1").read_text()
        self.assertIn("with -- (Get-Process -Id $PID).Path -NoProfile -File $PSCommandPath @forward", text)
        self.assertIn("& $identityPython scripts/regression_registry.py --platform windows\n"
                      "    if ($LASTEXITCODE) { exit $LASTEXITCODE }", text)


if __name__ == "__main__":
    unittest.main()
