"""Portable accounting fixtures; no product executable or native driver runs."""
from contextlib import ExitStack, redirect_stdout
import copy
import io
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

import qualification_campaign as campaign
import qualification_evidence as collector
import qualification_final_smokes as final
from qualification_recipe import default_manifest
import test_final_artifact_smoke as archives

HEAD = archives.HEAD
METADATA = json.dumps({"workspace_members": ["rmux"], "packages": [
    {"id": "rmux", "name": "rmux", "features": {}}]})


class QualificationFinalSmokesTests(unittest.TestCase):
    # Reuse the signed archive fixtures, not the native smoke drivers.
    repack = archives.SignedArchiveTests.repack
    sign = archives.SignedArchiveTests.sign

    @classmethod
    def setUpClass(cls):
        archives.SignedArchiveTests.setUpClass.__func__(cls)

    def setUp(self):
        archives.SignedArchiveTests.setUp(self)
        self.value["subject"] = "final-artifact"
        headers = {
            "macos-aarch64": b"\xcf\xfa\xed\xfe" + (0x0100000c).to_bytes(4, "little"),
            "linux-x86_64": b"\x7fELF\x02\x01" + bytes(12) + (62).to_bytes(2, "little")}
        pe = bytearray(128)
        pe[:2], pe[60:64], pe[64:68] = b"MZ", (64).to_bytes(4, "little"), b"PE\0\0"
        pe[68:70], pe[88:90] = (0x8664).to_bytes(2, "little"), (0x20b).to_bytes(2, "little")
        headers["windows-x86_64"] = bytes(pe)
        driver = self.root / "driver.py"
        driver.write_text("raise AssertionError('accounting fixture must never execute')\n")
        for key, row in self.value["platforms"].items():
            names = ("rmux.exe", "rmux-daemon.exe", "libexec/rmux/rmux.exe") if key.startswith("windows") else (
                "bin/rmux", "bin/rmux-daemon", "libexec/rmux/rmux")
            for name in names:
                (self.packages[key] / name).write_bytes(
                    headers[key] + b"RMUX_RELEASE_IDENTITY_V1:" + HEAD.encode() + b":END")
            row["scenarios"] = {name: {
                "driver": {"path": driver.name, "sha256": final.file_hash(driver)},
                "tool": {"path": str(Path(sys.executable).resolve()),
                         "sha256": final.file_hash(sys.executable)},
                "argv": ["{tool}", "{driver}", "{rmux}", "{receipt}", name],
                "timeout_seconds": 5} for name in archives.REQUIRED[key][2]}
            self.repack(key)
        self.sign()
        stack = ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(patch.object(final.smoke, "load_signer_policy", return_value=self.policy))
        original = campaign.subprocess.check_output

        def metadata(argv, *args, **kwargs):
            if argv[:2] == ["cargo", "metadata"]:
                return METADATA
            return original(argv, *args, **kwargs)

        stack.enter_context(patch.object(campaign.subprocess, "check_output", side_effect=metadata))

    def recipe(self, qualification="Q1"):
        return default_manifest(self.root, HEAD, qualification, final_artifact_manifest=self.path)

    def output(self, key, output, *, windows_paths=False):
        row = self.value["platforms"][key]
        output.mkdir(parents=True)
        package = output / row["package_root"]
        shutil.copytree(self.packages[key], package)
        files = final.smoke.archive_files(self.root, row)
        binaries = final.smoke.check_package(package, files, row, HEAD, key)
        native_output = PureWindowsPath("C:/evidence/0.final-smokes") if windows_paths else output
        native_manifest = PureWindowsPath("C:/inputs/manifest.json") if windows_paths else self.path
        if windows_paths:
            for record in binaries.values():
                record["path"] = str(native_output / Path(record["path"]).relative_to(output).as_posix())
        result = {"schema": 1, "subject": "final-artifact", "source_sha": HEAD, "platform": key,
                  "manifest_sha256": final.file_hash(self.path), "signer": "fixture@local",
                  "archive": row["archive"], "binaries": binaries, "cells": {},
                  "status": "PASS", "product_qualification": False}
        (output / "installed.log").write_text("accounting fixture; no execution\n")
        for name, cell in row["scenarios"].items():
            proof = output / (name + ".json")
            proof.write_text(json.dumps({"scenario": name, "status": "PASS", "source_sha": HEAD,
                                         "subject": "final-artifact", "binaries": binaries}))
            (output / (name + ".log")).write_text("accounting fixture; no execution\n")
            tool_path = (PureWindowsPath if windows_paths else PurePosixPath)(cell["tool"]["path"])
            replacements = {"{tool}": str(tool_path),
                            "{driver}": str(native_manifest.parent / cell["driver"]["path"]),
                            "{rmux}": binaries["cli"]["path"],
                            "{receipt}": str(native_output / (name + ".json"))}
            result["cells"][name] = dict(status="PASS", driver=cell["driver"], tool=cell["tool"],
                argv=[replacements.get(a, a) for a in cell["argv"]], evidence_sha256=final.file_hash(proof))
        (output / "result.json").write_text(json.dumps(result))
        return result

    def test_recipe_preserves_q0_and_adds_only_signed_smokes_to_q1_q2(self):
        q0 = default_manifest(self.root, HEAD)
        self.assertEqual(q0["schema"], 1)
        self.assertNotIn("final_artifacts", q0)
        for qid in ("Q1", "Q2"):
            recipe = self.recipe(qid)
            self.assertEqual(recipe["schema"], 2)
            self.assertEqual(recipe["scenarios"][-1]["command"][1], "scripts/release/final_artifact_smoke.py")
            self.assertEqual(recipe["scenarios"][:-2], q0["scenarios"][:-1])
            self.assertEqual(recipe["scenarios"][-2]["profile"]["duration_seconds"], 86400)
            self.assertEqual(recipe["final_artifacts"]["manifest_sha256"], final.file_hash(self.path))
            self.assertNotIn(str(self.root), json.dumps(recipe))
        self.assertNotEqual(campaign.digest(self.recipe("Q1")), campaign.digest(self.recipe("Q2")))
        relocated = self.root / "relocated-inputs"
        relocated.mkdir()
        names = {self.path.name, self.path.name + ".sig", "driver.py"}
        names.update(row["archive"]["path"] for row in self.value["platforms"].values())
        for name in names:
            shutil.copy2(self.root / name, relocated / name)
        self.assertEqual(default_manifest(self.root, HEAD, "Q1",
                         final_artifact_manifest=relocated / self.path.name), self.recipe())

    def test_signed_inputs_reject_fixture_other_sha_missing_os_cell_and_pending(self):
        original = copy.deepcopy(self.value)
        for defect in ("fixture", "sha", "os", "cell", "pending"):
            with self.subTest(defect=defect):
                self.value = copy.deepcopy(original)
                if defect == "fixture":
                    self.value["subject"] = "fixture"
                elif defect == "sha":
                    self.value["source_sha"] = "0" * 40
                elif defect == "os":
                    del self.value["platforms"]["macos-aarch64"]
                elif defect == "cell":
                    del self.value["platforms"]["windows-x86_64"]["scenarios"]["daemon-survives-terminal-close"]
                else:
                    self.value["platforms"]["linux-x86_64"]["scenarios"]["ssh"] = None
                self.sign()
                with self.assertRaises(ValueError):
                    self.recipe()

    def test_mutated_signed_manifest_signature_archive_and_driver_are_refused(self):
        self.recipe()
        for path in (self.path, Path(str(self.path) + ".sig"), self.root / "driver.py",
                     self.root / self.value["platforms"]["linux-x86_64"]["archive"]["path"]):
            original = path.read_bytes()
            try:
                path.write_bytes(b"invalid signature" if path.suffix == ".sig" else original + b"mutated")
                with self.subTest(path=path.name), self.assertRaises(ValueError):
                    self.recipe()
            finally:
                path.write_bytes(original)

    def test_portable_windows_output_and_q1_to_q2_reuse(self):
        recipe = self.recipe()
        output = self.root / "copied-output"
        self.output("windows-x86_64", output, windows_paths=True)
        receipt = dict(source_sha=HEAD, native_os="windows11", run_id="a" * 64,
                       challenge_id="b" * 64, execution_id="c" * 64,
                       argv=["python", "scripts/release/final_artifact_smoke.py", "run", "--manifest",
                             "C:\\inputs\\manifest.json", "--output", "C:\\evidence\\0.final-smokes"])
        receipt["final_artifacts"] = final.seal_output(recipe, receipt, output, self.path)
        self.assertEqual(final.check_output(recipe, receipt, output, self.path), 5)
        for changed in (dict(recipe, qualification_id="Q2"), dict(recipe, source_sha="0" * 40)):
            with self.assertRaises(ValueError):
                final.check_output(changed, receipt, output, self.path)
        for field in ("challenge_id", "execution_id", "run_id"):
            with self.subTest(field=field), self.assertRaises(ValueError):
                final.check_output(recipe, dict(receipt, **{field: "d" * 64}), output, self.path)

    def tool_path_output(self, native_os):
        key = final.PLATFORMS[native_os]
        windows = native_os == "windows11"
        raw, normalized = (("C:/Tools/python.exe", "C:\\Tools\\python.exe") if windows else
                           ("/opt/tools/./python3", "/opt/tools/python3"))
        for cell in self.value["platforms"][key]["scenarios"].values():
            cell["tool"]["path"] = raw
        self.sign()
        recipe = self.recipe()
        output = self.root / ("tool-output-" + native_os)
        result = self.output(key, output, windows_paths=windows)
        # Literal producer representations, independent of the accounting code.
        for cell in result["cells"].values():
            cell["argv"][0] = normalized
        (output / "result.json").write_text(json.dumps(result))
        argv = (["python", "scripts/release/final_artifact_smoke.py", "run", "--manifest",
                 "C:\\inputs\\manifest.json", "--output", "C:\\evidence\\0.final-smokes"] if windows else
                final.command_arguments(final.scenario()["command"], self.path, output))
        receipt = dict(source_sha=HEAD, native_os=native_os, run_id="a" * 64,
                       challenge_id="b" * 64, execution_id="c" * 64, argv=argv)
        return recipe, receipt, output, result

    def test_normalized_tool_paths_match_native_driver_on_all_platforms(self):
        for native_os, count in (("windows11", 5), ("linux", 4), ("macos", 4)):
            with self.subTest(native_os=native_os):
                recipe, receipt, output, _ = self.tool_path_output(native_os)
                receipt["final_artifacts"] = final.seal_output(recipe, receipt, output, self.path)
                self.assertEqual(final.check_output(recipe, receipt, output, self.path), count)

    def test_normalized_tool_paths_still_reject_other_tools_and_traversal(self):
        for native_os in ("windows11", "linux", "macos"):
            recipe, receipt, output, result = self.tool_path_output(native_os)
            wrong_paths = (("C:\\Tools\\other.exe", "C:\\Tools\\..\\evil\\python.exe")
                           if native_os == "windows11" else
                           ("/opt/tools/other", "/opt/tools/../evil/python3"))
            for wrong in wrong_paths:
                changed = copy.deepcopy(result)
                next(iter(changed["cells"].values()))["argv"][0] = wrong
                (output / "result.json").write_text(json.dumps(changed))
                with self.subTest(native_os=native_os, tool=wrong):
                    with self.assertRaisesRegex(ValueError, "cell argv differs"):
                        final.seal_output(recipe, receipt, output, self.path)

    def test_output_rejects_missing_cell_fixture_other_sha_and_mutation(self):
        recipe = self.recipe()
        output = self.root / "output"
        original = self.output("linux-x86_64", output)
        receipt = dict(source_sha=HEAD, native_os="linux", run_id="a" * 64,
                       challenge_id="b" * 64, execution_id="c" * 64,
                       argv=final.command_arguments(final.scenario()["command"], self.path, output))
        receipt["final_artifacts"] = final.seal_output(recipe, receipt, output, self.path)
        result_path = output / "result.json"
        for defect in ("cell", "fixture", "sha", "status"):
            result = copy.deepcopy(original)
            if defect == "cell":
                del result["cells"]["ssh"]
            else:
                field, value = {"fixture": ("subject", "fixture"), "sha": ("source_sha", "0" * 40),
                                "status": ("status", "FAIL")}[defect]
                result[field] = value
            result_path.write_text(json.dumps(result))
            with self.subTest(defect=defect), self.assertRaises(ValueError):
                final.check_output(recipe, receipt, output, self.path)
        result_path.write_text(json.dumps(original))
        (output / "ssh.log").write_text("mutated")
        with self.assertRaisesRegex(ValueError, "binding differs"):
            final.check_output(recipe, receipt, output, self.path)

    def campaigns(self, recipe, omit_cell=False):
        """Simulate process I/O and host identity; keep runner/collector logic real."""
        external = tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2])
        self.addCleanup(external.cleanup)
        journal = Path(external.name)
        authority = journal / "authority.json"
        authority.write_text(json.dumps({"schema": 1, "campaigns": [{"source_sha": HEAD,
            "qualification_id": recipe["qualification_id"],
            "paths": {name: str(journal) for name in final.PLATFORMS}}]}))
        stack = ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(patch.object(campaign, "AUTHORITY_FILE", authority))
        stack.enter_context(patch.dict(os.environ, RMUX_QUALIFICATION_JOURNAL=str(journal)))
        stack.enter_context(patch.object(campaign.identity, "source_identity"))
        stack.enter_context(patch.object(campaign.identity, "validate_context"))
        # All fixtures live on this host. Real Windows path semantics are tested
        # separately above; these two substitutions never assert native authority.
        stack.enter_context(patch.object(collector, "PureWindowsPath", PurePosixPath))
        stack.enter_context(patch.object(final, "PureWindowsPath", PurePosixPath))
        old_cwd = Path.cwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, old_cwd)
        directories = []
        oracle = dict(version="tmux 3.7b", sha256="a" * 64, reference_sha256=None, source_sha=None)
        for os_name, key in final.PLATFORMS.items():
            target = (self.root / ".rmux-audit" / HEAD / ("a" * 26) if os_name == "windows11" else
                      self.root / ".rmux-audit/candidate-target" / HEAD / os_name / ("a" * 32))
            target.mkdir(parents=True)
            image = target / "fixture-image"
            native = "windows" if os_name == "windows11" else os_name
            source_hash = campaign.hashlib.sha256(os.fsencode(str(self.root))).hexdigest()
            image.write_text("RMUX_BUILD_IDENTITY_V1:" + HEAD + ":" + source_hash + ":" + native + ":END")
            record = dict(path=str(image), sha256=final.file_hash(image), commit=HEAD,
                          source=str(self.root), target=str(target), os=native)
            env = dict(os.environ, RMUX_IDENTITY_TARGET=str(target), RMUX_IDENTITY_HEAD=HEAD,
                       RMUX_IDENTITY_SOURCE=str(self.root))

            def command(argv, root, env, out, err):
                err.write_bytes(b"")
                if "scripts/release/final_artifact_smoke.py" in argv:
                    result = self.output(key, Path(argv[6]))
                    if omit_cell:
                        del result["cells"]["ssh"]
                        (Path(argv[6]) / "result.json").write_text(json.dumps(result))
                    out.write_text("fixture final smoke output\n")
                else:
                    out.write_text("probe: test\n" if "--list" in argv else
                                   "test probe ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n")
                return 0

            directory = self.root / ("evidence-" + os_name)
            with patch.object(campaign, "native_os", return_value=os_name), \
                 patch.object(campaign.identity, "prepare", return_value=env), \
                 patch.object(campaign, "oracle_record", return_value=oracle), \
                 patch.object(campaign, "launch_records", return_value=[{"executables": [record]}]), \
                 patch.object(campaign, "run_command", side_effect=command):
                result = campaign.run_campaign(self.root, recipe, directory, final_artifact_manifest=self.path)
            self.assertEqual(result["status"], "FAIL" if omit_cell else "PASS_CELL",
                             (directory / "receipts.json").read_text())
            directories.append(directory)
        return directories

    def test_zero_exit_missing_smoke_cell_stops_runner_and_cannot_collect(self):
        directories = self.campaigns(self.recipe(), omit_cell=True)
        for directory in directories:
            receipts = json.loads((directory / "receipts.json").read_text())
            self.assertEqual(receipts[-1]["exit_code"], 0)
            self.assertEqual(receipts[-1]["status"], "FAIL")
            self.assertIn("missing or unexpected final smoke cell", receipts[-1]["error"])
        with self.assertRaises(ValueError):
            collector.check_campaign(self.root, directories, final_artifact_manifest=self.path)

    def test_runner_and_collector_require_all_thirteen_cells_and_reject_reuse(self):
        recipe = self.recipe()
        directories = self.campaigns(recipe)
        result = collector.check_campaign(self.root, directories, final_artifact_manifest=self.path)
        self.assertEqual(result["final_smoke_cells"], 13)
        self.assertFalse(result["native_authenticity_or_product_verdict"])
        for path in (self.path, Path(str(self.path) + ".sig"), self.root / "driver.py",
                     self.root / self.value["platforms"]["macos-aarch64"]["archive"]["path"]):
            original = path.read_bytes()
            try:
                path.write_bytes(original + b"mutated")
                with self.subTest(path=path.name), self.assertRaises(ValueError):
                    collector.check_campaign(self.root, directories, final_artifact_manifest=self.path)
            finally:
                path.write_bytes(original)
        with self.assertRaises(ValueError):
            collector.check_campaign(self.root, directories[:-1], final_artifact_manifest=self.path)
        with self.assertRaises(ValueError):
            collector.check_campaign(self.root, directories, synthetic=True, final_artifact_manifest=self.path)
        last = directories[-1]
        receipts = json.loads((last / "receipts.json").read_text())
        final_receipt = receipts[-1]
        final_receipt["final_artifacts"]["qualification_id"] = "Q2"
        (last / "receipts.json").write_text(json.dumps(receipts))
        (last / (str(len(receipts) - 1) + ".receipt.json")).write_text(json.dumps(final_receipt))
        with self.assertRaisesRegex(ValueError, "execution binding differs"):
            collector.check_campaign(self.root, directories, final_artifact_manifest=self.path)

    def test_native_runner_and_collector_refuse_legacy_q1_before_execution(self):
        legacy = default_manifest(self.root, HEAD)
        legacy["qualification_id"] = "Q1"
        directory = self.root / "legacy"
        with patch.object(campaign, "run_command") as command:
            with self.assertRaisesRegex(ValueError, "legacy accounting"):
                campaign.run_campaign(self.root, legacy, directory)
            command.assert_not_called()
        directory.mkdir()
        (directory / "manifest.json").write_text(json.dumps(legacy))
        with self.assertRaisesRegex(ValueError, "legacy accounting"):
            collector.check_campaign(self.root, [directory])

    def test_cli_forwards_signed_inputs_and_refuses_missing_q1_inputs(self):
        sys.path.insert(0, str(Path(__file__).resolve().parent / "validation"))
        import contracts
        old_cwd = Path.cwd()
        self.addCleanup(os.chdir, old_cwd)
        base = ["contracts.py", "--root", str(self.root)]
        with patch.object(campaign.identity, "git", return_value=HEAD), \
             patch.object(campaign.identity, "source_identity"), \
             patch.object(campaign, "run_campaign", return_value={"status": "PASS_CELL"}) as run, \
             redirect_stdout(io.StringIO()):
            with patch.object(sys, "argv", base + ["--run-qualification", str(self.root / "new"),
                                                  "--qualification-id", "Q1"]):
                self.assertEqual(contracts.main(), 1)
            run.assert_not_called()
            with patch.object(sys, "argv", base + ["--run-qualification", str(self.root / "new"),
                    "--qualification-id", "Q2", "--final-artifact-manifest", str(self.path)]):
                self.assertEqual(contracts.main(), 0)
            self.assertEqual(run.call_args.args[1]["qualification_id"], "Q2")
            self.assertEqual(run.call_args.kwargs["final_artifact_manifest"], self.path)
        with patch.object(collector, "check_campaign", return_value={"status": "PASS_QUALIFICATION_ACCOUNTING"}) as check, \
             patch.object(sys, "argv", base + ["--check-qualification", str(self.root / "evidence"),
                                               "--final-artifact-manifest", str(self.path)]), \
             redirect_stdout(io.StringIO()):
            self.assertEqual(contracts.main(), 0)
            self.assertEqual(check.call_args.kwargs["final_artifact_manifest"], self.path)


if __name__ == "__main__":
    unittest.main()
