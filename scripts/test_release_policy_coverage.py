"""The shipped release policy must cover every tracked script, and only those it lists."""
import importlib.util
import json
import re
import os
import shutil
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

    def ci_job(self, name):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        return re.search(r"^  " + name + r":\n(.*?)(?=^  [\w-]+:|\Z)",
                         workflow, re.M | re.S).group(1)

    def test_source_gate_runner_matches_the_release_verifier_contract(self):
        contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
        job = self.ci_job("linux-source-gates")
        label = re.search(r"^    runs-on: (.+)$", job, re.M).group(1)
        name = re.search(r"^    name: (.+)$", job, re.M).group(1)
        self.assertIn(name, contract["runner_policy"]["jobs_by_label"][label])
        spec = importlib.util.spec_from_file_location(
            "verify_fast_run", ROOT / "scripts/release/verify-fast-run.py")
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        _, labels, _ = verifier.contracted_runner_labels(contract, {name})
        self.assertEqual(labels[name], label)

    def test_bootstrap_runs_in_both_unix_suites(self):
        linux = self.ci_job("linux-tests")
        self.assertRegex(linux, r"packages: [^\n]*-p rmux-bootstrap\b")
        self.assertIn("cargo test --locked --no-fail-fast ${{ matrix.packages }}", linux)
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        macos = workflow[workflow.index("      - name: cargo test parallel-safe portable crates"):]
        self.assertRegex(macos, r"run: cargo test [^\n]*-p rmux-bootstrap\b[^\n]*--test-threads=1")

    def test_windows_archive_exclusions_have_active_candidate_build_commands(self):
        build = self.ci_job("windows-candidate-source")
        self.assertNotIn("needs:", build)
        self.assertIn("runs-on: windows-latest", build)
        step = build.split("      - name: Windows candidate identity and final-sink tests\n", 1)[1]
        step = step.split("      - name:", 1)[0]
        self.assertNotRegex(step, r"(?m)^\s+if:")
        self.assertIn('RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB: "1"', step)
        self.assertIn("run: python scripts/candidate_identity.py with -- pwsh -NoProfile -File scripts/test-windows-candidate-context.ps1", step)
        self.assertEqual(step.count("candidate_identity.py with"), 1)
        driver = (ROOT / "scripts/test-windows-candidate-context.ps1").read_text()
        commands = re.findall(r"^cargo (.+)$", driver, re.M)
        self.assertEqual(len(commands), 4)
        self.assertIn("validate-gate", driver)
        self.assertEqual(driver.count("if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }"), 5)
        for selection in (
            "-p rmux --test candidate_identity_children",
            "-p rmux-server --lib bracketed_paste_final_sink",
            "-p rmux-server --lib handler::pane_command_tests::respawn_pane_rejects_an_interior_nul_and_the_same_child_keeps_reading",
            "-p rmux-server --lib test_shell::final_sink::windows_byte_child::tests::",
        ):
            self.assertTrue(any(selection in command for command in commands), selection)
        coverage = json.loads((ROOT / ".github/release/windows-coverage-contract.json").read_text())
        server = next(row for row in coverage["steps"] if row["name"] == "server lib tests")
        self.assertIn("Windows candidate identity and final-sink tests", server["evidence"])

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell is required for the driver fixture")
    def test_windows_candidate_driver_propagates_failures_and_keeps_one_context(self):
        harness = r'''function global:Check-Identity {
          if ($args[-1] -ne 'validate-gate') { throw 'missing gate validation' }
          $global:LASTEXITCODE = [int]$env:IDENTITY_EXIT
        }
        $global:Calls = 0
        function global:cargo {
          $global:Calls += 1
          [pscustomobject]@{ argv = $args; target = $env:CARGO_TARGET_DIR } | ConvertTo-Json -Compress
          $global:LASTEXITCODE = if ($global:Calls -eq [int]$env:FAIL_CALL) { 17 } else { 0 }
        }
        & $env:CANDIDATE_DRIVER
        exit $LASTEXITCODE
        '''
        for identity_exit, fail_call, expected_exit, calls in ((0, 0, 0, 4), (0, 2, 17, 2), (23, 0, 23, 0)):
            with self.subTest(identity_exit=identity_exit, fail_call=fail_call):
                env = dict(os.environ, RMUX_IDENTITY_PYTHON="Check-Identity", RMUX_IDENTITY_SCRIPT="fixture",
                           CARGO_TARGET_DIR="one-private-candidate-target", IDENTITY_EXIT=str(identity_exit),
                           FAIL_CALL=str(fail_call), CANDIDATE_DRIVER=str(ROOT / "scripts/test-windows-candidate-context.ps1"))
                result = subprocess.run([shutil.which("pwsh"), "-NoProfile", "-Command", harness],
                                        env=env, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, expected_exit, result.stderr)
                rows = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertEqual(len(rows), calls)
                self.assertTrue(all(row["target"] == env["CARGO_TARGET_DIR"] for row in rows))
                self.assertTrue(all(row["argv"][0] == "test" for row in rows))

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
