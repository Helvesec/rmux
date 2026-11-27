"""Causal acceptance coverage for the qualification runner."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import qualification_campaign as campaign
from qualification_evidence import check_campaign, check_portable_identity
from qualification_recipe import default_manifest

METADATA = json.dumps({"workspace_members": ["rmux"], "packages": [
    {"id": "rmux", "name": "rmux", "features": {"default": [], "sample": []}}]})


class QualificationCampaignTests(unittest.TestCase):
    def setUp(self):
        self.journal = tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2])
        self.journal_env = patch.dict(os.environ, {"RMUX_QUALIFICATION_JOURNAL": self.journal.name})
        self.journal_env.start()
        self.addCleanup(self.journal.cleanup)
        self.addCleanup(self.journal_env.stop)
        self.authority = Path(self.journal.name) / "authority.json"
        authority_patch = patch.object(campaign, "AUTHORITY_FILE", self.authority)
        authority_patch.start()
        self.addCleanup(authority_patch.stop)

    def authorize(self, manifest):
        self.authority.write_text(json.dumps({"schema": 1, "campaigns": [{
            "source_sha": manifest["source_sha"], "qualification_id": manifest["qualification_id"],
            "paths": {os_name: self.journal.name for os_name in ("macos", "linux", "windows11")}}]}))

    def fixture(self, command, cells=None, tests=False):
        manifest = {"schema": 1, "source_sha": "a" * 40, "qualification_id": "Q0",
                "scenarios": [{"id": "canary", "command": command, "tests": tests}],
                "cells": cells or [{"id": "fixture", "native_os": "macos"}]}
        self.authorize(manifest)
        return manifest

    def run_fixture(self, root, command, **kwargs):
        manifest = self.fixture(command, **kwargs)
        output = root / "evidence"
        return campaign.run_campaign(root, manifest, output, synthetic=True), output

    @patch("qualification_recipe.subprocess.check_output", return_value=METADATA)
    def test_os_applicability_executes_only_declared_native_recipes(self, metadata):
        manifest = default_manifest(Path(__file__).resolve().parents[1], "a" * 40)
        unix = next(s for s in manifest["scenarios"] if s["id"] == "scoreboard")
        self.assertEqual(set(unix.get("native_os", [])), {"linux", "macos"})
        windows = next(s for s in manifest["scenarios"] if s["id"] == "scoreboard/windows11")
        self.assertEqual(windows["native_os"], ["windows11"])
        self.assertIn("windows_terminal_matrix", windows["command"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cells = [{"id": os_name, "native_os": os_name} for os_name in ("linux", "windows11")]
            fixture = self.fixture([sys.executable, "-c", "print('unix')"], cells=cells)
            fixture["scenarios"][0]["native_os"] = ["linux"]
            fixture["scenarios"].append({"id": "windows", "native_os": ["windows11"],
                                       "command": [sys.executable, "-c", "print('windows')"]})
            result = campaign.run_campaign(root, fixture, root / "evidence", synthetic=True)
            self.assertEqual(result["expected"], 2)
            self.assertEqual(result["executed"], 2)
            self.assertEqual(check_campaign(root, [root / "evidence"], synthetic=True)["executions"], 2)
            # Exercise native dispatch/collection without claiming product proof.
            directories = []
            for os_name in ("linux", "windows11"):
                evidence = root / ("native-" + os_name)
                target = root / ("target-" + os_name)
                env = dict(os.environ, RMUX_IDENTITY_TARGET=str(target),
                           RMUX_IDENTITY_HEAD=fixture["source_sha"], RMUX_IDENTITY_SOURCE=str(root))
                oracle = {"version": "tmux 3.7b", "sha256": "b" * 64,
                          "reference_sha256": None, "source_sha": None}
                old_cwd = Path.cwd()
                try:
                    os.chdir(root)
                    with patch.object(campaign, "native_os", return_value=os_name), \
                         patch.object(campaign.identity, "source_identity"), \
                         patch.object(campaign.identity, "prepare", return_value=env), \
                         patch.object(campaign.identity, "validate_context"), \
                         patch.object(campaign, "default_manifest", return_value=fixture), \
                         patch.object(campaign, "oracle_record", return_value=oracle) as measured:
                        result = campaign.run_campaign(root, fixture, evidence)
                    self.assertEqual(result["status"], "PASS_CELL")
                    self.assertEqual(result["executed"], 1)
                    self.assertEqual(measured.call_count, 2 if os_name == "linux" else 0)
                finally:
                    os.chdir(old_cwd)
                directories.append(evidence)
            with patch.object(campaign.identity, "source_identity"), \
                 patch("qualification_evidence.default_manifest", return_value=fixture):
                self.assertEqual(check_campaign(root, directories)["executions"], 2)
                # Copied receipts cannot select an isolated authority at collection.
                isolated = root / "isolated-journal"
                isolated.mkdir()
                with patch.dict(os.environ, {"RMUX_QUALIFICATION_JOURNAL": str(isolated)}), \
                     self.assertRaisesRegex(ValueError, "shared.*journal"):
                    check_campaign(root, directories)
            bad = dict(fixture, scenarios=[dict(fixture["scenarios"][0], native_os=["windows10"])])
            with self.assertRaisesRegex(ValueError, "applicability"):
                campaign.validate_manifest(bad)

    @patch("qualification_recipe.subprocess.check_output", return_value=METADATA)
    @patch("qualification_final_smokes.bind_inputs", return_value={
        "manifest_sha256": "b" * 64, "signature_sha256": "c" * 64, "signer": "fixture"})
    def test_versioned_soak_recipe_executes_load_and_fails_closed(self, final_inputs, metadata):
        root = Path(__file__).resolve().parents[1]
        for qualification, mode, duration in (("Q0", "brief", 10), ("Q1", "long", 86400),
                                               ("Q2", "long", 86400)):
            manifest = default_manifest(root, "a" * 40, qualification)
            soaks = [s for s in manifest["scenarios"] if s["id"] == "soak/v1"]
            self.assertEqual(len(soaks), 1, "complete recipe omits the versioned soak")
            scenario = soaks[0]
            self.assertEqual(scenario["native_os"], ["linux", "macos", "windows11"])
            self.assertEqual(scenario["command"], ["python", "scripts/soak.py", "--mode", mode,
                                                    "--duration", str(duration)])
            profile = scenario["profile"]
            self.assertEqual(profile["schema"], 1)
            self.assertEqual(profile["topology"], {"sessions": 2, "windows_per_session": 2,
                                                   "panes_per_window": 2, "clients": 2})
            self.assertEqual(profile["concurrency"], 2)
            self.assertEqual(profile["rates_per_second"], {"input": 2, "capture": 2, "churn": 1})
            self.assertEqual(profile["maximum_skips"], 0)
            self.assertEqual(set(profile["postconditions"]),
                             {"topology", "roundtrip", "clients", "cleanup"})

        from soak_profile import soak_profile
        from soak import run_soak

        class Backend:
            def __init__(self, defect=None):
                self.events, self.defect = [], defect

            def setup(self, profile):
                self.events.append("setup")

            def exercise(self, operation, worker, sequence):
                self.events.append((operation, worker, sequence))
                return self.defect != operation

            def verify(self, profile):
                self.events.append("verify")
                return {name: self.defect != name for name in profile["postconditions"]
                        if self.defect != "missing"}

            def cleanup(self):
                self.events.append("cleanup")
                return self.defect != "cleanup"

        # Virtual time exercises the actual scheduler without running a real soak.
        class Clock:
            now = 0.0

            def monotonic(self):
                return self.now

            def sleep(self, seconds):
                self.now += seconds

        profile = soak_profile("brief")
        self.assertEqual(soak_profile("long")["duration_seconds"], 86400)
        self.assertEqual(soak_profile("long", 90000)["duration_seconds"], 90000)
        for mode, seconds in (("long", 10), ("brief", 0), ("brief", float("nan"))):
            with self.assertRaises(ValueError):
                soak_profile(mode, seconds)
        for defect in (None, "input", "capture", "churn", "topology", "roundtrip",
                       "clients", "cleanup", "missing"):
            with self.subTest(defect=defect):
                backend, clock = Backend(defect), Clock()
                report = run_soak(profile, backend, clock=clock)
                self.assertEqual(report["status"], "PASS_BRIEF" if defect is None else "FAIL")
                self.assertEqual(report["skipped"], 0)
                self.assertEqual(backend.events[-1], "cleanup")
                if defect is None:
                    self.assertGreaterEqual(report["elapsed_seconds"], 10)
                    for operation, minimum in profile["minimum_operations"].items():
                        self.assertGreaterEqual(report["operations"][operation], minimum)
                    self.assertEqual({e[1] for e in backend.events if isinstance(e, tuple)}, {0, 1})
                    self.assertTrue(all(report["postconditions"].values()))

    def test_measured_oracle_binds_both_selectors_and_rejects_conflict(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "tmux"
            binary.write_bytes(b"measured")
            env = {"RMUX_FROZEN_TMUX": str(binary)}
            with patch.object(campaign.subprocess, "check_output", return_value="tmux 3.7b\n"):
                measured = campaign.oracle_record(env)
                self.assertEqual(env.get("RMUX_TMUX_ORACLE"), measured["path"])
                self.assertEqual(env["RMUX_FROZEN_TMUX"], measured["path"])
                with self.assertRaisesRegex(ValueError, "selectors diverge"):
                    campaign.oracle_record(dict(env, RMUX_TMUX_ORACLE=str(binary.parent / "other")))

    def test_soak_workers_overlap_and_partial_failures_always_clean_up(self):
        import threading
        from soak import run_soak
        from soak_profile import soak_profile

        class Clock:
            now = 0

            def monotonic(self):
                return self.now

            def sleep(self, delay):
                self.now += delay

        class Backend:
            def __init__(self, defect=None):
                self.defect, self.cleaned = defect, False
                self.barrier = threading.Barrier(2, timeout=2)

            def setup(self, profile):
                if self.defect == "setup":
                    raise RuntimeError("partial setup")

            def exercise(self, name, worker, sequence):
                if name == "input":
                    self.barrier.wait()  # Sequential workers cannot pass this check.
                if self.defect == "operation":
                    raise RuntimeError("operation exception")
                if self.defect == "slow":
                    clock.now += 2
                return 1 if self.defect == "truthy" else True

            def verify(self, profile):
                if self.defect == "absent":
                    return None
                return dict.fromkeys(profile["postconditions"], True)

            def cleanup(self):
                self.cleaned = True
                if self.defect == "cleanup":
                    raise RuntimeError("cleanup exception")
                return True

        profile = soak_profile("brief", 1)
        for defect in (None, "setup", "operation", "slow", "truthy", "absent", "cleanup"):
            with self.subTest(defect=defect):
                backend, clock = Backend(defect), Clock()
                report = run_soak(profile, backend, clock=clock)
                self.assertEqual(report["status"], "PASS_BRIEF" if defect is None else "FAIL")
                self.assertTrue(backend.cleaned)
                self.assertEqual(report["skipped"], 0)
        backend = Backend()
        for key, value in (("concurrency", 1), ("maximum_skips", 1),
                           ("minimum_operations", {}), ("postconditions", ["cleanup"])):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "profile differs"):
                run_soak(dict(profile, **{key: value}), backend, clock=Clock())
        self.assertFalse(backend.cleaned)

    def test_two_clones_share_required_attempt_journal(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            root, clone = parent / "first", parent / "clone"
            root.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Qualification Test", "-c", "user.email=test@example.invalid",
                            "commit", "--allow-empty", "-qm", "fixture"], cwd=root, check=True)
            subprocess.run(["git", "clone", "-q", str(root), str(clone)], check=True)
            manifest = self.fixture([sys.executable, "-c", "print('ok')"])
            manifest["source_sha"] = campaign.identity.git(root, "rev-parse", "HEAD")
            self.authorize(manifest)
            with patch.dict(os.environ, {"RMUX_QUALIFICATION_JOURNAL": ""}):
                with self.assertRaisesRegex(ValueError, "shared.*journal"):
                    campaign.claim_attempts(root, manifest, manifest["cells"], root / "evidence", "absent", False)
            journal = Path(self.journal.name)
            with patch.dict(os.environ, {"RMUX_QUALIFICATION_JOURNAL": str(journal)}):
                campaign.claim_attempts(root, manifest, manifest["cells"], root / "evidence", "first", False)
                with self.assertRaisesRegex(ValueError, "already attempted"):
                    campaign.claim_attempts(clone, manifest, manifest["cells"], clone / "evidence", "second", False)

    def test_collector_rejects_copies_without_shared_journal(self):
        for defect in ("missing", "different"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                _, evidence = self.run_fixture(root, [sys.executable, "-c", "print('ok')"])
                records = list((root / ".rmux-audit/qualification-attempts").glob("*.json"))
                self.assertEqual(len(records), 1)
                if defect == "missing":
                    records[0].unlink()
                else:
                    value = json.loads(records[0].read_text())
                    value["run_id"] = "isolated-clone"
                    records[0].write_text(json.dumps(value))
                with self.assertRaisesRegex(ValueError, "shared.*journal"):
                    check_campaign(root, [evidence], synthetic=True)

    def test_second_clone_cannot_choose_a_fresh_external_journal(self):
        with tempfile.TemporaryDirectory() as directory, \
             tempfile.TemporaryDirectory(dir=Path(self.journal.name).parent) as other_journal:
            root, clone = Path(directory) / "first", Path(directory) / "clone"
            root.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Qualification Test", "-c", "user.email=test@example.invalid",
                            "commit", "--allow-empty", "-qm", "fixture"], cwd=root, check=True)
            subprocess.run(["git", "clone", "-q", str(root), str(clone)], check=True)
            manifest = self.fixture([sys.executable, "-c", "print('ok')"])
            manifest["source_sha"] = campaign.identity.git(root, "rev-parse", "HEAD")
            self.authorize(manifest)
            records = campaign.claim_attempts(root, manifest, manifest["cells"], root / "evidence", "first", False)
            self.assertEqual(records[0]["attempt"], 1)
            self.assertEqual(list(Path(other_journal).iterdir()), [])
            with patch.dict(os.environ, {"RMUX_QUALIFICATION_JOURNAL": other_journal}), \
                 self.assertRaisesRegex(ValueError, "shared.*journal"):
                campaign.claim_attempts(clone, manifest, manifest["cells"], clone / "evidence", "second", False)
            self.assertEqual(list(Path(other_journal).iterdir()), [])

    def test_collector_rejects_tickets_copied_to_another_external_journal(self):
        with tempfile.TemporaryDirectory() as directory, \
             tempfile.TemporaryDirectory(dir=Path(self.journal.name).parent) as other_journal:
            root = Path(directory).resolve()
            manifest = self.fixture([sys.executable, "-c", "print('ok')"],
                                    cells=[{"id": "fixture", "native_os": "windows11"}])
            env = dict(os.environ, RMUX_IDENTITY_TARGET=str(root / "target"),
                       RMUX_IDENTITY_HEAD=manifest["source_sha"], RMUX_IDENTITY_SOURCE=str(root))
            old_cwd = Path.cwd()
            try:
                os.chdir(root)
                with patch.object(campaign, "native_os", return_value="windows11"), \
                     patch.object(campaign.identity, "source_identity"), \
                     patch.object(campaign.identity, "prepare", return_value=env), \
                     patch.object(campaign.identity, "validate_context"), \
                     patch.object(campaign, "default_manifest", return_value=manifest):
                    result = campaign.run_campaign(root, manifest, root / "evidence")
                self.assertEqual(result["status"], "PASS_CELL")
            finally:
                os.chdir(old_cwd)
            with patch.object(campaign.identity, "source_identity"), \
                 patch("qualification_evidence.default_manifest", return_value=manifest):
                self.assertEqual(check_campaign(root, [root / "evidence"])["executions"], 1)
                self.authority.unlink()
                with self.assertRaisesRegex(ValueError, "shared.*journal"):
                    check_campaign(root, [root / "evidence"])
                self.authorize(manifest)
                for ticket in Path(self.journal.name).glob("*.json"):
                    campaign.shutil.copy2(ticket, Path(other_journal) / ticket.name)
                self.assertTrue(list(Path(other_journal).glob("*.json")))
                with patch.dict(os.environ, {"RMUX_QUALIFICATION_JOURNAL": other_journal}), \
                     self.assertRaisesRegex(ValueError, "shared.*journal"):
                    check_campaign(root, [root / "evidence"])

    def test_unregistered_campaign_never_enrolls_from_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.fixture([sys.executable, "-c", "print('ok')"])
            registered = json.loads(self.authority.read_text())
            for defect in ("missing", "sha", "qualification", "duplicate", "schema", "os", "relative"):
                with self.subTest(defect=defect):
                    registry = json.loads(json.dumps(registered))
                    entry = registry["campaigns"][0]
                    if defect == "missing":
                        self.authority.unlink()
                    else:
                        if defect == "sha":
                            entry["source_sha"] = "b" * 40
                        elif defect == "qualification":
                            entry["qualification_id"] = "Q2"
                        elif defect == "duplicate":
                            registry["campaigns"].append(entry)
                        elif defect == "schema":
                            registry["schema"] = 2
                        elif defect == "os":
                            entry["paths"].pop(campaign.native_os())
                        elif defect == "relative":
                            entry["paths"][campaign.native_os()] = "."
                        self.authority.write_text(json.dumps(registry))
                    with self.assertRaisesRegex(ValueError, "shared.*journal"):
                        campaign.claim_attempts(root, manifest, manifest["cells"], root / "evidence", "run", False)
                    self.assertFalse((Path(self.journal.name) /
                                      (campaign.digest(campaign.attempt_key(manifest, manifest["cells"][0])) +
                                       ".json")).exists())
                    self.authorize(manifest)

    def test_runner_inventory_declarations_match_live_blobs(self):
        root = Path(__file__).resolve().parents[1]
        declarations = json.loads((root / "scripts/source-inventory/declarations.json").read_text())
        rows = {path: role for role, paths in declarations["files"].items() for path in paths}
        for name, category in (("qualification_campaign.py", "tooling"), ("qualification_evidence.py", "tooling"),
                               ("qualification_recipe.py", "tooling"), ("test_qualification_campaign.py", "tests")):
            self.assertIn("scripts/" + name, rows)
            self.assertEqual(rows["scripts/" + name], category)
        blobs = next(group["members"]["scripts/validation/contracts.py"]
                     for group in declarations["document_groups"]
                     if "scripts/validation/contracts.py" in group["members"])
        self.assertIn(subprocess.check_output(
            ["git", "hash-object", "scripts/validation/contracts.py"], cwd=root, text=True).strip(), blobs)

    def test_failed_command_cannot_be_retried_into_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            command = [sys.executable, "-c", (
                "from pathlib import Path; import sys; "
                "p=Path('attempts'); n=int(p.read_text())+1 if p.exists() else 1; "
                "p.write_text(str(n)); sys.exit(1 if n == 1 else 0)"
            )]
            manifest = {
                "schema": 1, "source_sha": "a" * 40,
                "qualification_id": "Q1",
                "scenarios": [{"id": "retry-canary", "command": command}],
                "cells": [{"id": "fixture", "native_os": "macos"}],
            }
            evidence = root / "evidence"
            result = campaign.run_campaign(root, manifest, evidence, synthetic=True)
            self.assertEqual(result["status"], "FAIL")
            with self.assertRaises(ValueError):
                campaign.run_campaign(root, manifest, evidence, synthetic=True)
            with self.assertRaisesRegex(ValueError, "already attempted"):
                campaign.run_campaign(root, manifest, root / "new-evidence", synthetic=True)
            self.assertFalse((root / "new-evidence").exists())
            self.assertEqual((root / "attempts").read_text(), "1")
            self.assertEqual(subprocess.run(command, cwd=root).returncode, 0)
            receipts = json.loads((evidence / "receipts.json").read_text())
            self.assertEqual(len(receipts), 1)
            self.assertEqual(receipts[0]["status"], "FAIL")
            self.assertEqual(receipts[0]["attempt"], 1)

    def test_success_keeps_complete_raw_output_and_accounting(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, output = self.run_fixture(root, [sys.executable, "-c", "print('observed')"])
            self.assertEqual(result["status"], "PASS_FIXTURE")
            self.assertEqual((output / "0.stdout").read_text(), "observed\n")
            self.assertEqual(check_campaign(root, [output], synthetic=True)["executions"], 1)
            with self.assertRaises(ValueError):
                check_campaign(root, [output])

    def test_first_failure_stops_without_losing_pending_cells(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.fixture([sys.executable, "-c", "raise SystemExit(9)"])
            manifest["scenarios"].append({"id": "never", "command": [sys.executable, "-c", "print('never')"]})
            result = campaign.run_campaign(root, manifest, root / "evidence", synthetic=True)
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(result["pending"], [{"cell_id": "fixture", "scenario_id": "never"}])
            self.assertFalse((root / "evidence" / "1.receipt.json").exists())

    def test_successful_process_reporting_skip_is_red(self):
        for text in ("SKIP: oracle absent", "skipping prerequisite", "OK (skipped=1)",
                     "test result: ok. 1 passed; 0 failed; 1 ignored;"):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result, output = self.run_fixture(root, [sys.executable, "-c", "print(" + repr(text) + ")"])
                self.assertEqual(result["status"], "FAIL")
                with self.assertRaises(ValueError):
                    check_campaign(root, [output], synthetic=True)

    def test_missing_receipt_cannot_redefine_population(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, output = self.run_fixture(root, [sys.executable, "-c", "print('ok')"])
            (output / "receipts.json").write_text("[]")
            summary = json.loads((output / "summary.json").read_text())
            summary.update(executed=0, pending=[{"cell_id": "fixture", "scenario_id": "canary"}])
            (output / "summary.json").write_text(json.dumps(summary))
            with self.assertRaisesRegex(ValueError, "missing native cell"):
                check_campaign(root, [output], synthetic=True)

    def test_missing_native_cell_is_not_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cells = [{"id": "macos", "native_os": "macos"}, {"id": "linux", "native_os": "linux"}]
            _, output = self.run_fixture(root, [sys.executable, "-c", "print('ok')"], cells=cells)
            receipts = json.loads((output / "receipts.json").read_text())[:1]
            (output / "receipts.json").write_text(json.dumps(receipts))
            summary = json.loads((output / "summary.json").read_text())
            summary.update(executed=1, pending=[{"cell_id": "linux", "scenario_id": "canary"}])
            (output / "summary.json").write_text(json.dumps(summary))
            with self.assertRaisesRegex(ValueError, "missing native cell"):
                check_campaign(root, [output], synthetic=True)

    def test_output_or_identity_mutation_and_duplicate_evidence_are_red(self):
        for defect in ("output", "identity", "retry", "challenge", "duplicate"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                _, output = self.run_fixture(root, [sys.executable, "-c", "print('ok')"])
                if defect == "output":
                    (output / "0.stdout").write_text("different")
                elif defect != "duplicate":
                    receipts = json.loads((output / "receipts.json").read_text())
                    receipts[0][{"identity": "source_sha", "retry": "attempt", "challenge": "challenge_id"}[defect]] = (
                        2 if defect == "retry" else "b" * (40 if defect == "identity" else 64))
                    (output / "receipts.json").write_text(json.dumps(receipts))
                    (output / "0.receipt.json").write_text(json.dumps(receipts[0]))
                with self.assertRaises(ValueError):
                    check_campaign(root, [output, output] if defect == "duplicate" else [output], synthetic=True)

    def test_listed_tests_execute_exactly_once(self):
        for defect in (None, "missing", "empty", "ignored", "duplicate"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                listing = "one: test\ntwo: test\n2 tests, 0 benchmarks\n"
                execution = "test one ... ok\ntest two ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored;\n"
                if defect == "empty":
                    listing = "0 tests, 0 benchmarks\n"
                if defect == "missing":
                    execution = execution.replace("test two ... ok\n", "")
                if defect == "duplicate":
                    execution = execution.replace("test two", "test one")
                if defect == "ignored":
                    execution = execution.replace("0 ignored", "1 ignored")
                driver = root / "driver.py"
                driver.write_text("import sys\nprint(" + repr(listing) + " if '--list' in sys.argv else " + repr(execution) + ")\n")
                result, output = self.run_fixture(root, [sys.executable, "driver.py", "--", "--include-ignored"], tests=True)
                self.assertEqual(result["status"], "PASS_FIXTURE" if defect is None else "FAIL")
                if defect is None:
                    self.assertEqual(check_campaign(root, [output], synthetic=True)["executions"], 1)

    def test_skipped_tests_leave_the_listed_population(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            driver = root / "driver.py"
            driver.write_text(
                "import sys\n"
                "names = ['one'] + ([] if '--skip' in sys.argv else ['salvo'])\n"
                "if '--list' in sys.argv:\n"
                "    print(''.join(n + ': test\\n' for n in names))\n"
                "else:\n"
                "    print(''.join('test ' + n + ' ... ok\\n' for n in names) +\n"
                "          'test result: ok. %d passed; 0 failed; 0 ignored;' % len(names))\n")
            result, output = self.run_fixture(
                root, [sys.executable, "driver.py", "--", "--include-ignored", "--skip", "salvo"], tests=True)
            self.assertEqual(result["status"], "PASS_FIXTURE", result)
            receipt = json.loads((output / "receipts.json").read_text())[0]
            self.assertEqual(receipt["population"], {"one": 1})
            self.assertEqual(check_campaign(root, [output], synthetic=True)["executions"], 1)

    def test_two_checkout_paths_have_identical_population_and_verdict(self):
        with tempfile.TemporaryDirectory() as directory:
            results, digests = [], []
            for name in ("checkout one", "checkout-two"):
                root = Path(directory) / name
                root.mkdir()
                (root / "probe.py").write_text("print('same observable')\n")
                result, output = self.run_fixture(root, [sys.executable, "probe.py"])
                results.append(check_campaign(root, [output], synthetic=True)["status"])
                digests.append(result["manifest_sha256"])
            self.assertEqual(results, ["PASS_FIXTURE", "PASS_FIXTURE"])
            self.assertEqual(digests[0], digests[1])

    def test_d41_portable_paths_bind_head_os_source_and_target(self):
        for os_name in ("macos", "linux", "windows11"):
            source = "C:\\candidate" if os_name == "windows11" else "/candidate"
            target = (source + "\\.rmux-audit\\" + "a" * 40 + "\\" + "a" * 26 if os_name == "windows11"
                      else source + "/.rmux-audit/candidate-target/" + "a" * 40 + "/" + os_name + "/" + "b" * 32)
            record = {"source": source, "target": target, "path": target + ("\\binary.exe" if os_name == "windows11" else "/binary"),
                      "commit": "a" * 40, "sha256": "b" * 64, "os": "windows" if os_name == "windows11" else os_name}
            context = {"source": source, "target": target}
            check_portable_identity(record, context, "a" * 40, os_name)
            for field, value in (("commit", "b" * 40), ("source", source + "other"),
                                 ("path", source + "/foreign"), ("os", "foreign"), ("sha256", "false")):
                with self.subTest(os=os_name, field=field), self.assertRaises(ValueError):
                    check_portable_identity(dict(record, **{field: value}), context, "a" * 40, os_name)

    def test_false_embedded_identity_is_red_before_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            target = root / "target"
            target.mkdir()
            binary = target / "image"
            os_name = campaign.identity.host_os()
            source_id = hashlib.sha256(os.fsencode(str(root))).hexdigest()
            marker = "RMUX_BUILD_IDENTITY_V1:" + "a" * 40 + ":" + source_id + ":" + os_name + ":END"
            binary.write_bytes(marker.encode())
            record = {"path": str(binary), "sha256": campaign.file_hash(binary), "commit": "a" * 40,
                      "source": str(root), "target": str(target), "os": os_name}
            campaign.check_executable(record, root, "a" * 40, target, os_name)
            binary.write_bytes(marker.replace("a" * 40, "b" * 40).encode())
            record["sha256"] = campaign.file_hash(binary)
            with self.assertRaisesRegex(ValueError, "embedded"):
                campaign.check_executable(record, root, "a" * 40, target, os_name)

    def test_false_native_context_stops_before_any_scenario(self):
        audit = Path(__file__).resolve().parents[1] / ".rmux-audit"
        audit.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            root = Path(directory).resolve()
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("pub fn sample() {}\n")
            (root / "Cargo.toml").write_text('[package]\nname="qualification-fixture"\nversion="0.1.0"\nedition="2021"\n[workspace]\n')
            (root / ".gitignore").write_text("/.rmux-audit/\n")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True, capture_output=True)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Qualification Test", "-c", "user.email=test@example.invalid",
                            "commit", "-qm", "fixture"], cwd=root, check=True)
            head = campaign.identity.git(root, "rev-parse", "HEAD")
            manifest = default_manifest(root, head)
            self.authorize(manifest)
            old_cwd = Path.cwd()
            try:
                os.chdir(root)
                with patch.dict(os.environ, {"RMUX_IDENTITY_HEAD": "b" * 40,
                                             "RMUX_IDENTITY_SOURCE": str(root),
                                             "RMUX_IDENTITY_TARGET": str(root / "foreign")}):
                    result = campaign.run_campaign(root, manifest, root / ".rmux-audit/evidence")
            finally:
                os.chdir(old_cwd)
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(result["executed"], 0)
            self.assertEqual(len(json.loads((root / ".rmux-audit/evidence/receipts.json").read_text())), 1)

    def test_native_worktree_change_cannot_hide_an_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "first"
            root.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Qualification Test", "-c", "user.email=test@example.invalid",
                            "commit", "--allow-empty", "-qm", "fixture"], cwd=root, check=True)
            other = Path(directory) / "second"
            subprocess.run(["git", "worktree", "add", "--detach", str(other), "HEAD"],
                           cwd=root, check=True, capture_output=True)
            manifest = self.fixture([sys.executable, "-c", "print('ok')"])
            manifest["source_sha"] = campaign.identity.git(root, "rev-parse", "HEAD")
            self.authorize(manifest)
            campaign.claim_attempts(root, manifest, manifest["cells"], root / "evidence", "first", False)
            with self.assertRaisesRegex(ValueError, "already attempted"):
                campaign.claim_attempts(other, manifest, manifest["cells"], other / "evidence", "second", False)

    def test_manifest_population_is_rejected_before_execution(self):
        for field in ("scenarios", "cells"):
            for rows in ([], "duplicate"):
                manifest = self.fixture([sys.executable, "-c", "print('ok')"])
                manifest[field] = [] if rows == [] else manifest[field] * 2
                with self.subTest(field=field, rows=rows), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    with self.assertRaises(ValueError):
                        campaign.run_campaign(root, manifest, root / "evidence", synthetic=True)
                    self.assertFalse((root / "evidence").exists())

    def test_gate_entrypoints_have_help_and_reject_partial_qualification(self):
        root = Path(__file__).resolve().parents[1]
        gate = root / "scripts/release-review-gate.sh"
        help_result = subprocess.run(["bash", str(gate), "--help"], capture_output=True, text=True)
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("--qualification-dir", help_result.stdout)
        for arguments in (["--skip-package"], ["--no-tmux"], ["--section", "static"],
                          ["--evidence-mode", "candidate-delta"], ["--target-dir", "shared"]):
            result = subprocess.run(["bash", str(gate), "--qualification-dir", "unused", *arguments],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("complete recipe", result.stderr)

    def test_rustup_proxy_preserves_the_invoked_cargo_path(self):
        cargo = campaign.shutil.which("cargo")
        self.assertIsNotNone(cargo)
        argv = campaign.resolve_command(Path.cwd(), ["cargo", "--version"], campaign.native_os())
        self.assertEqual(argv[0], str(Path(cargo).absolute()))
        result = subprocess.run(argv, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith("cargo "))

    @patch("qualification_recipe.subprocess.check_output", return_value=METADATA)
    def test_candidate_recipe_is_path_independent_and_complete(self, metadata):
        root = Path(__file__).resolve().parents[1]
        manifest = default_manifest(root, "a" * 40)
        ids = [s["id"] for s in manifest["scenarios"]]
        self.assertTrue({"fmt", "clippy", "workspace", "doctests", "scoreboard", "hygiene", "regressions"} <= set(ids))
        self.assertEqual(manifest["cells"], campaign.default_manifest(root, "a" * 40)["cells"])
        self.assertEqual({c["native_os"] for c in manifest["cells"]}, {"macos", "linux", "windows11"})
        self.assertIn("powerset/rmux/subset:none", ids)
        for scenario in manifest["scenarios"]:
            self.assertNotIn(str(root), json.dumps(scenario))
            if scenario.get("tests"):
                self.assertIn("--include-ignored", scenario["command"])
                self.assertIn("--show-output", scenario["command"])
        workspace = next(s for s in manifest["scenarios"] if s["id"] == "workspace")["command"]
        libtest = workspace[workspace.index("--"):]
        # The ignored Darwin PTY salvos must stay out of every qualification cell.
        self.assertIn(["--skip", "backend::macos::tests::"],
                      [libtest[i:i + 2] for i in range(len(libtest) - 1)])
        self.assertEqual(campaign.listing_command(workspace)[:-1], workspace)


if __name__ == "__main__":
    unittest.main()
