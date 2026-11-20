"""The shipped release policy must cover every tracked script, and only those it lists."""
import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / ".github/release/candidate-contract.json"
PARSER_ENVIRONMENT_TEST = "scripts/source-inventory/test_parser_environment.py"
THIS_TEST = "scripts/test_release_policy_coverage.py"
REQUIRED_DEPENDENCIES = (
    PARSER_ENVIRONMENT_TEST,
    THIS_TEST,
    "scripts/source-inventory/population-additions.jsonl",
    "scripts/source-inventory/proof-epochs-d570.json",
    "scripts/source-inventory/relocation-proofs/pass263.json",
    "scripts/source-inventory/relocation-proofs/pass264.json",
    "scripts/source-inventory/relocation-proofs/review263.txt",
    "scripts/source-inventory/relocation-proofs/review264.txt",
    "scripts/source-inventory/relocation-revisions.json",
    "scripts/source-inventory/relocation_chain.py",
    "scripts/source-inventory/test_d570_epochs.py",
    "scripts/source-inventory/test_descendant_transport.py",
    "scripts/source-inventory/test_relocation_transport.py",
    "scripts/test_bench_collect.py",
)
LS_FILES = ["git", "ls-files", "-z"]


def load_validator():
    sys.path.insert(0, str(ROOT / "scripts/release"))
    spec = importlib.util.spec_from_file_location(
        "validate_contracts", ROOT / "scripts/release/validate-contracts.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ReleasePolicyCoverage(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.validator = load_validator()

    def policy_paths(self):
        return json.loads(CONTRACT.read_text(encoding="utf-8"))["policy_paths"]

    def test_shipped_policy_covers_every_workflow_reachable_file(self):
        self.validator.validate_candidate()

    def test_inventory_and_benchmark_dependencies_are_listed(self):
        paths = self.policy_paths()
        for path in REQUIRED_DEPENDENCIES:
            with self.subTest(path=path):
                self.assertIn(path, paths)

    def test_unknown_tracked_scripts_are_still_refused(self):
        unknown = ["scripts/source-inventory/test_unlisted_probe.py", "scripts/unlisted_probe.py"]
        real_run = subprocess.run

        def ls_files_with_unknown(argv, *args, **kwargs):
            result = real_run(argv, *args, **kwargs)
            if list(argv) == LS_FILES:
                extra = b"".join(path.encode() + b"\x00" for path in unknown)
                result = subprocess.CompletedProcess(argv, result.returncode,
                                                     result.stdout + extra, result.stderr)
            return result

        with patch.object(self.validator.subprocess, "run", ls_files_with_unknown):
            with self.assertRaises(ValueError) as raised:
                self.validator.validate_candidate()
        self.assertEqual(
            str(raised.exception),
            f"release policy does not cover workflow-reachable files: {sorted(unknown)}")

    def test_dropping_each_required_dependency_from_the_policy_is_refused(self):
        real_load = self.validator.load

        def load_without_dependency(path):
            contract = real_load(path)
            if Path(path) == CONTRACT:
                contract["policy_paths"] = [
                    p for p in contract["policy_paths"] if p != dependency]
            return contract

        for dependency in REQUIRED_DEPENDENCIES:
            with self.subTest(path=dependency), \
                    patch.object(self.validator, "load", load_without_dependency):
                with self.assertRaises(ValueError) as raised:
                    self.validator.validate_candidate()
                self.assertEqual(
                    str(raised.exception),
                    f"release policy does not cover workflow-reachable files: {[dependency]}")


if __name__ == "__main__":
    unittest.main()
