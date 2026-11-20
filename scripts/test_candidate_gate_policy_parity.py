"""Every gate runs the policy checks of the fast Unix gate, Windows natively included.

The 043 alternates file was written with a CR on Windows; only a native Windows
run of the candidate suite could see it. The Windows fast gate and CI must
therefore run the same policy discovers as scripts/gate-unix-fast.sh.
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
UNIX_GATE = ROOT / 'scripts/gate-unix-fast.sh'
WINDOWS_GATE = ROOT / 'scripts/gate-windows-fast.ps1'
CI = ROOT / '.github/workflows/ci.yml'
POLICY_STEPS = ('candidate identity regression', 'release policy coverage')
DISCOVER = re.compile(r"-m unittest discover -s (\S+) -p (\S+)")


def discover_args(command):
    found = DISCOVER.search(command)
    if found is None:
        return None
    return tuple(part.strip('\'"') for part in found.groups())


def unix_policy_discovers():
    steps = {}
    for line in UNIX_GATE.read_text(encoding='utf-8').splitlines():
        step = re.match(r'\s*run_step "([^"]+)" (.*)$', line)
        if step and step.group(1) in POLICY_STEPS:
            steps[step.group(1)] = discover_args(step.group(2))
    return steps


def ci_jobs():
    jobs = CI.read_text(encoding='utf-8').split('\njobs:\n', 1)[1]
    blocks = re.split(r'(?m)^  (?=[A-Za-z0-9_-]+:\s*$)', jobs)
    return {block.split(':', 1)[0]: block for block in blocks if block.strip()}


class GatePolicyParity(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.unix = unix_policy_discovers()

    def test_unix_gate_declares_each_policy_discover(self):
        for label in POLICY_STEPS:
            with self.subTest(label=label):
                self.assertIsNotNone(self.unix.get(label), label)

    def test_windows_gate_runs_each_policy_discover_before_cargo(self):
        lines = WINDOWS_GATE.read_text(encoding='utf-8').splitlines()
        cargo = next(index for index, line in enumerate(lines)
                     if line.startswith('Run-Step "cargo fmt"'))
        for label in POLICY_STEPS:
            with self.subTest(label=label):
                # Top-level and before Cargo: no switch can drop it, and an
                # unlisted file fails in seconds, as in the Unix gate.
                steps = [(index, line) for index, line in enumerate(lines)
                         if line.startswith(f"Run-Step '{label}' ")]
                self.assertEqual(len(steps), 1, label)
                index, line = steps[0]
                self.assertLess(index, cargo, label)
                self.assertEqual(discover_args(line), self.unix[label])

    def test_ci_runs_each_policy_discover_on_linux_and_windows(self):
        jobs = ci_jobs()
        for runner in ('ubuntu', 'windows'):
            for label in POLICY_STEPS:
                with self.subTest(runner=runner, label=label):
                    running = [
                        name for name, block in jobs.items()
                        if re.search(rf'(?m)^    runs-on: {runner}-', block)
                        and self.unix[label] in {discover_args(line)
                                                 for line in block.splitlines()}]
                    self.assertTrue(running, f'no {runner} CI job runs {label}')
                    for name in running:
                        # HARNESS043.bundle needs trunk commit b4fe38d082af.
                        self.assertIn('fetch-depth: 0', jobs[name], name)


if __name__ == '__main__':
    unittest.main()
