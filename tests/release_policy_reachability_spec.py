"""The committed release root must bind the workflow dependency closure."""

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/release"))
QUEUE_INVENTORY_DEPENDENCIES = (
    "scripts/queue-inventory/check_inventory.py",
    "scripts/queue-inventory/profiles.json",
    "scripts/queue-inventory/run_references.py",
    "scripts/queue-inventory/sites.json",
    "scripts/queue-inventory/source_scan.py",
    "scripts/queue-inventory/stores.json",
    "scripts/queue-inventory/test_control_profiles.py",
    "scripts/queue-inventory/test_inventory.py",
    "scripts/queue-inventory/validation.py",
)


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts/release" / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


class ReleasePolicyReachability(unittest.TestCase):
    def test_committed_root_binds_queue_inventory_dependencies(self):
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        report = module("queue_policy_root", "policy-root.py").calculate(ROOT, head)
        records = {entry["path"]: entry for entry in report["records"]}
        for path in QUEUE_INVENTORY_DEPENDENCIES:
            with self.subTest(queue_dependency=path):
                committed = subprocess.check_output(["git", "show", f"{head}:{path}"], cwd=ROOT)
                self.assertTrue(path in records, f"committed release root omits {path}")
                self.assertEqual(records[path]["sha256"], hashlib.sha256(committed).hexdigest())

    def test_final_smoke_scripts_are_bound_by_release_policy(self):
        contract = json.loads((ROOT / ".github/release/candidate-contract.json").read_text())
        for path in ("scripts/qualification_final_smokes.py",
                     "scripts/test_qualification_final_smokes.py",
                     "scripts/test_qualification_final_smokes_required.py"):
            with self.subTest(path=path):
                self.assertIn(path, contract["policy_paths"])

    def test_committed_root_binds_regression_registry_options(self):
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        report = module("policy_root", "policy-root.py").calculate(ROOT, head)
        records = {entry["path"]: entry for entry in report["records"]}
        for path in ("scripts/regression_registry_options.py",
                     "scripts/regression_registry_options_flags.json",
                     "scripts/test_regression_registry_options.py"):
            with self.subTest(path=path):
                self.assertIn(path, records)
                committed = subprocess.check_output(["git", "show", f"{head}:{path}"], cwd=ROOT)
                self.assertEqual(records[path]["sha256"], hashlib.sha256(committed).hexdigest())

    def test_committed_root_binds_workflow_dependencies(self):
        validator = module("validate_contracts", "validate-contracts.py")
        validator.validate_candidate()
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        report = module("policy_root", "policy-root.py").calculate(ROOT, head)
        records = {entry["path"]: entry for entry in report["records"]}
        tracked = subprocess.check_output(
            ["git", "ls-files", "-z", "scripts/", ".github/workflows/", ".github/actions/"],
            cwd=ROOT,
        ).decode().rstrip("\0").split("\0")
        for path in tracked:
            with self.subTest(path=path):
                self.assertIn(path, records)
                committed = subprocess.check_output(["git", "show", f"{head}:{path}"], cwd=ROOT)
                self.assertEqual(records[path]["sha256"], hashlib.sha256(committed).hexdigest())

        # Reachable through the gate, release verifier, and native packaging action.
        contract = json.loads((ROOT / ".github/release/candidate-contract.json").read_text())
        for path in ("scripts/candidate_identity_consumers.py",
                     "scripts/release/release_sbom.py", "scripts/conpty/candidate-runtime.ps1",
                     *QUEUE_INVENTORY_DEPENDENCIES,
                     "scripts/regression_registry_options.py",
                     "scripts/regression_registry_options_flags.json",
                     "scripts/test_regression_registry_options.py",
                     "scripts/test_web_no_listener_preflight.py",
                     "scripts/web_no_listener_preflight.py"):
            with self.subTest(omitted_dependency=path):
                omitted = dict(contract, policy_paths=[p for p in contract["policy_paths"] if p != path])
                with patch.object(validator, "load", return_value=omitted):
                    with self.assertRaisesRegex(ValueError, "workflow-reachable files"):
                        validator.validate_candidate()


if __name__ == "__main__":
    unittest.main()
