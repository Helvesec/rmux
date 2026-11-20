"""Closed-line wiring and simulated native delegation; no native proof here."""
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import regression_registry as registry
import regression_registry_options as options


ROOT = registry.ROOT


class ClosedCoverage(unittest.TestCase):
    def test_accepted_native_recipe_and_documentary_guards(self):
        value = json.loads(registry.DEFAULT.read_text())
        command = next(item for item in value["commands"] if item["id"] == "options-per-option")
        self.assertEqual(command["argv"],
                         ["python", "scripts/regression_registry_checks.py", "options-per-option"])
        self.assertEqual(command["population"], dict(options=153, phases=306,
                         first=["default-shell", "status-interval"], flag_operations_per_phase=58))
        self.assertEqual(command["recipe"]["driver"], ["python", "-B", "tests/options_bank_linux.py",
                         "per-option", "CONTRACT", "PHASE", "OUTPUT"])
        self.assertEqual(command["recipe"]["timeout_seconds"], 420)
        self.assertEqual(hashlib.sha256(options.FLAGS.read_bytes()).hexdigest(),
                         "ee1a64f3c2fcb0140d63cea52818229cf97aa1d8234defcd7e12289e8d66955d")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "registry.json"
            for defect in ("native", "digest", "protector", "receipt", "gate", "source"):
                changed = copy.deepcopy(value)
                doc = changed["documentary"][0]
                if defect == "native":
                    doc["os"] = ["linux"]
                elif defect == "digest":
                    doc["reviewed_sources"]["Cargo.toml"] = "0" * 64
                elif defect == "protector":
                    doc["protectors"][0]["test"] = "removed_fallback_regression"
                elif defect == "receipt":
                    doc["receipts"] = ["unknown"]
                elif defect == "gate":
                    doc["gates"] = ["scripts/nonexistent-gate.sh"]
                else:
                    doc["sources"].remove("Cargo.toml")
                path.write_text(json.dumps(changed))
                with self.subTest(defect=defect), self.assertRaises(registry.RegistryError):
                    registry.load_registry(path)
            changed = copy.deepcopy(value)
            changed["commands"][-1]["coverage"][0]["line"] = "L11.5"
            path.write_text(json.dumps(changed))
            with self.assertRaisesRegex(registry.RegistryError, "cannot claim native"):
                registry.load_registry(path)

    def test_native_entry_matrix_preserves_identity_exit_and_single_dispatch(self):
        for platform, native, count in (("linux", "linux", 14), ("macos", "darwin", 3),
                                        ("windows", "win32", 1)):
            with self.subTest(platform=platform), \
                    patch.object(sys, "platform", native), \
                    patch.object(sys, "argv", ["registry", "--platform", platform]), \
                    patch.dict(os.environ, {"RMUX_IDENTITY_HEAD": "fixture"}, clear=True), \
                    patch("candidate_identity.validate_context") as validate, \
                    patch.object(registry, "run_commands", return_value=37) as execute:
                self.assertEqual(registry.main(), 37)
                validate.assert_called_once_with()
                execute.assert_called_once()
                selected = execute.call_args.args[0]
                self.assertEqual(len(selected), count)
                self.assertEqual(len({item["id"] for item in selected}), count)
                self.assertEqual(any(item["id"] == "options-per-option" for item in selected),
                                 platform == "linux")
                self.assertEqual(os.environ["RMUX_REGRESSION_REGISTRY_ACTIVE"], "1")
            with self.subTest(platform=platform, missing_identity=True), \
                    patch.object(sys, "platform", native), \
                    patch.object(sys, "argv", ["registry", "--platform", platform]), \
                    patch.dict(os.environ, {}, clear=True), \
                    patch.object(registry.subprocess, "run",
                                 return_value=subprocess.CompletedProcess([], 89)) as wrapped, \
                    patch.object(registry, "run_commands") as execute:
                self.assertEqual(registry.main(), 89)
                wrapped.assert_called_once()
                self.assertIn(str(ROOT / "scripts/candidate_identity.py"), wrapped.call_args.args[0])
                self.assertEqual(wrapped.call_args.args[0][-2:], ["--platform", platform])
                execute.assert_not_called()


class OptionsDelegation(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bank, self.adapter = options.bank_modules()

    def test_contract_rebinds_only_current_inputs_and_preserves_recipe_limits(self):
        template = self.adapter.load(ROOT / "tests/options_bank_contract.json")
        paths = [self.root / name for name in ("registry", "candidate", "oracle")]
        for path in paths:
            path.write_text(path.name)
        export, candidate, oracle = paths
        head = "f" * 40
        contract = options.contract_for(template, "copy-command", export, ROOT, head,
                                        candidate, oracle, self.bank, self.adapter)
        self.assertEqual(contract["native_source"], head)
        self.assertEqual(contract["source_path"], str(ROOT))
        self.assertEqual(contract["native_os"], "linux")
        self.assertEqual(contract["registry"], self.adapter.pin(export))
        self.assertEqual(contract["python"], self.adapter.pin(Path(sys.executable).resolve()))
        self.assertEqual(contract["native_binaries"], dict(candidate=self.adapter.pin(candidate),
                                                        oracle=self.adapter.pin(oracle)))
        self.assertEqual(contract["limits"], template["limits"])
        self.assertEqual(contract["recipe"], template["recipe"])
        self.assertEqual(contract["default_recipe"], dict(schema=1, kind="per-option-v1",
                         names=["copy-command"], startup=self.bank.REGISTRY_STARTUP_V1,
                         environment={"SHELL": "/bin/bash"}, token="backspace-input-s1096"))
        self.assertEqual(contract["default_recipe_sha256"], hashlib.sha256(json.dumps(
                         contract["default_recipe"], sort_keys=True).encode()).hexdigest())
        self.assertIn(self.adapter.pin(export), contract["artifacts"])
        self.assertNotIn("native_source", template)
        table = ROOT / "crates/rmux-core/src/options/registry/table.rs"
        self.assertEqual([reference for reference in contract["artifacts"]
                          if reference["path"] == str(table)], [self.adapter.pin(table)])

        # Project the real pinned sources, then remove only the current table.
        source = self.root / "source"
        for reference in contract["artifacts"]:
            original = Path(reference["path"])
            if original.is_relative_to(ROOT):
                copied = source / original.relative_to(ROOT)
                copied.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(original, copied)
        copied_table = source / table.relative_to(ROOT)
        with patch.object(options, "ROOT", source):
            copied_contract = options.contract_for(template, "copy-command", export, source,
                                                   head, candidate, oracle, self.bank, self.adapter)
            self.assertIn(self.adapter.pin(copied_table), copied_contract["artifacts"])
            copied_table.unlink()
            with self.assertRaises(FileNotFoundError) as caught:
                options.contract_for(template, "copy-command", export, source, head,
                                     candidate, oracle, self.bank, self.adapter)
            self.assertEqual(caught.exception.filename, str(copied_table))

    def test_modified_native_plan_is_refused(self):
        path = self.root / "flags.json"
        plan = options.accepted_plan()
        plan["operations"][2]["contains"] = "weakened"
        path.write_text(json.dumps(plan))
        with patch.object(options, "FLAGS", path), self.assertRaisesRegex(RuntimeError, "plan changed"):
            options.accepted_plan()

    def test_complete_independent_inventory_keeps_first_two_and_rejects_loss_or_rename(self):
        from options_bank_linux_spec import fixture
        document, template = fixture()
        names = options.option_names(document, template, self.bank)
        self.assertEqual(names[:2], ["default-shell", "status-interval"])
        self.assertEqual(len(names), 153)
        self.assertEqual(set(names), set(self.bank.canonical_registry_identities()))
        for defect in ("omit", "rename", "identity", "duplicate"):
            broken = copy.deepcopy(document)
            if defect == "omit":
                broken["options"].pop()
                broken["registry_count"] -= 1
            elif defect == "rename":
                broken["options"][0]["name"] = "renamed"
            elif defect == "identity":
                broken["options"][0]["id"] = "named:renamed"
            else:
                broken["options"][0] = broken["options"][1]
            with self.subTest(defect=defect), self.assertRaises(self.bank.GateError):
                options.option_names(broken, template, self.bank)

    def test_mode_dispatches_once_with_private_target(self):
        import regression_registry_checks as checks
        with patch.object(checks.identity, "validate_context") as validate, \
                patch.object(checks.identity, "context", return_value=(ROOT, "current", self.root)), \
                patch.object(sys, "argv", ["checks", "options-per-option"]), \
                patch.object(options, "replay") as replay:
            checks.main()
        validate.assert_called_once_with()
        replay.assert_called_once_with(self.root)

    def test_per_option_freezes_oracle_before_current_candidate_and_stops_on_failure(self):
        for failure in (None, "oracle", "candidate", "mismatch"):
            with self.subTest(failure=failure):
                directory = self.root / str(failure)
                directory.mkdir()
                calls = []
                def run(argv, timeout):
                    phase, output = argv[-2:]
                    self.assertEqual(timeout, 420)
                    self.assertEqual(argv[1:4], ["-B", ROOT / "tests/options_bank_linux.py", "per-option"])
                    if phase == "candidate":
                        prior = self.adapter.load(directory / "ORACLE-FROZEN.json")
                        self.adapter.authenticate(prior["receipt"])
                        self.assertEqual(launch.call_count, 1)
                    calls.append(phase)
                    if phase == failure:
                        raise subprocess.CalledProcessError(71, argv)
                    output.mkdir()
                    self.adapter.save(output / "RESULT.json", dict(phase=phase))
                def observe(receipt, contract, phase):
                    return {"observed": "changed" if failure == "mismatch" and phase == "candidate" else "same"}
                with patch.object(options.identity, "record_launch") as launch, \
                        patch.object(self.adapter, "per_option_phase", side_effect=observe):
                    if failure in ("oracle", "candidate"):
                        with self.assertRaises(subprocess.CalledProcessError) as caught:
                            options.per_option_pair({}, directory, self.adapter, run, {"head": "current"})
                        self.assertEqual(caught.exception.returncode, 71)
                    elif failure == "mismatch":
                        with self.assertRaisesRegex(RuntimeError, "oracle/candidate mismatch"):
                            options.per_option_pair({}, directory, self.adapter, run, {"head": "current"})
                    else:
                        options.per_option_pair({}, directory, self.adapter, run, {"head": "current"})
                        launch.assert_called_once_with([{"head": "current"}])
                self.assertEqual(calls, ["oracle"] if failure == "oracle" else ["oracle", "candidate"])

    def test_flags_require_real_hook_and_scope_outputs_and_native_cleanup(self):
        plan = self.adapter.load(options.FLAGS)
        artifact = self.root / "pinned"
        artifact.write_text("current inputs")
        reference = self.adapter.pin(artifact)
        contract = dict(native_source="current", artifacts=[reference],
                        native_binaries=dict(oracle=reference, candidate=reference),
                        python=reference, default_driver=reference)
        for defect in (None, "H-hook-list", "U-session-after-pane", "cleanup", "disk"):
            if defect not in (None, "cleanup", "disk"):
                self.assertIn(defect, {item["role"] for item in plan["operations"]})
            runner = Mock()
            runner.record = dict(commands=[], closure=dict(status="FAIL" if defect == "cleanup" else "PASS"))
            def command(role, argv, check):
                step = next(item for item in plan["operations"] if item["role"] == role)
                stdout = step.get("stdout", step.get("contains", ""))
                if role == defect:
                    stdout = "wrong\n"
                runner.record["commands"].append(dict(exit_code=step["exit_code"],
                                        stderr=step.get("stderr_contains", "")))
                return stdout, len(runner.record["commands"]) - 1
            runner.command.side_effect = command
            folder = self.root / str(defect)
            with self.subTest(defect=defect), patch.object(self.adapter, "Run", return_value=runner), \
                    patch.object(self.adapter, "describe", return_value=dict(synthetic=True)), \
                    patch.object(options.identity, "record_launch") as launch, \
                    patch.object(options.shutil, "disk_usage", return_value=SimpleNamespace(
                                 free=(30 if defect == "disk" else 70) * 2**30)):
                if defect is None:
                    checks = options.flags_phase(contract, "candidate", folder,
                                                  plan, self.adapter, {})
                    self.assertEqual(len(checks), 58)
                else:
                    with self.assertRaises(RuntimeError):
                        options.flags_phase(contract, "candidate", folder,
                                            plan, self.adapter, {})
                runner.cleanup.assert_called_once()
                launch.assert_called_once_with([{}])
                self.assertTrue((folder / "RESULT.json").is_file())
                result = self.adapter.load(folder / "RESULT.json")
                self.assertEqual(result["status"], "PASS" if defect is None else "FAIL")

    def test_foreign_native_bank_is_refused_before_any_work(self):
        for platform in ("darwin", "win32"):
            with self.subTest(platform=platform), patch.object(sys, "platform", platform), \
                    patch.object(options.identity, "validate_context") as validate:
                with self.assertRaisesRegex(RuntimeError, "accepted Linux profile"):
                    options.replay(self.root)
                validate.assert_not_called()


if __name__ == "__main__":
    unittest.main()
