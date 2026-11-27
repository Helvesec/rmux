"""Identity acceptance using inert bytes; never runs a measured RMUX image."""
import copy
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROLES = ('public', 'helper', 'daemon')
FAULTS = ('missing', 'substituted', 'relative', 'topology', 'size', 'hash')


class PerfThreeImagesTests(unittest.TestCase):
    def test_direct_gate_and_portable_identity(self):
        audit = ROOT / '.rmux-audit' / 'perf-three-images'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            scripts = fixture / 'scripts'
            scripts.mkdir()
            for name in ('perf-diff.py', 'perf_artifact.py', 'check-perf-current.py',
                         'check-perf-baseline.py', 'release-review-gate.sh'):
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            shutil.copy2(ROOT / 'Cargo.toml', fixture / 'Cargo.toml')
            baseline = json.loads((ROOT / 'benches/perf/baselines/release-0.9.0.json').read_text())
            host_platform = platform.system().lower()
            environment = dict(platform=host_platform, machine=platform.machine().lower(),
                               host_fingerprint='synthetic-three-images')
            baseline['environment'] = environment
            baseline['source']['environment'] = environment
            baseline['source']['platform'] = host_platform
            baseline['source']['provenance']['expected_platform'] = host_platform
            baselines = fixture / 'benches/perf/baselines'
            baselines.mkdir(parents=True)
            darwin = copy.deepcopy(baseline)
            darwin['environment'] = dict(environment, platform='darwin')
            darwin['source']['environment'] = darwin['environment']
            darwin['source']['platform'] = 'darwin'
            darwin['source']['provenance']['expected_platform'] = 'darwin'
            (baselines / 'release-0.9.0.json').write_text(json.dumps(darwin))
            baseline_path = baselines / ('release-0.9.0.json' if host_platform == 'darwin'
                                         else 'release-0.9.0-' + host_platform + '.json')
            baseline_path.write_text(json.dumps(baseline))
            head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
            version = next(line.split('"')[1] for line in (ROOT / 'Cargo.toml').read_text().splitlines()
                           if line.startswith('version = '))
            paths = dict(public=fixture / 'release/rmux',
                         helper=fixture / 'release/libexec/rmux/rmux',
                         daemon=fixture / 'release/rmux-daemon')
            contents = dict(public=b'cli', helper=b'inert-helper', daemon=b'inert-daemon')
            for role, path in paths.items():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(contents[role])
            def member(role):
                return dict(path=str(paths[role]), size_bytes=len(contents[role]),
                            sha256=hashlib.sha256(contents[role]).hexdigest())
            current = copy.deepcopy(baseline['source'])
            current.update(schema=2, kind='rmux-perf-current',
                           timestamp=datetime.now(timezone.utc).isoformat(),
                           git=dict(commit=head, describe='identity-fixture', dirty=False),
                           environment=environment,
                           binary=dict(member('public'), version='rmux ' + version,
                                       configuration='release'),
                           layout=dict(schema=1, **{role: member(role) for role in ROLES}),
                           provenance=dict(generator='scripts/perf-bench.sh', invocation='identity-fixture',
                                           expected_git_commit=head, expected_platform=host_platform,
                                           build_mode='rebuilt'))
            for metric in current['metrics']:
                metric.update(status='pass', budget_p95_ms=max(metric['p95_ms'] * 2, 1))
            current_path = fixture / 'current.json'
            options = ['--expected-current-commit', head, '--expected-current-platform', host_platform,
                       '--expected-current-machine', environment['machine'],
                       '--expected-current-host-fingerprint', environment['host_fingerprint'],
                       '--expected-current-provenance', 'identity-fixture',
                       '--expected-current-binary-version', 'rmux ' + version,
                       '--max-current-age-seconds', '600']
            env = dict(os.environ, RMUX_PERF_CURRENT_JSON=str(current_path),
                       RMUX_PERF_EXPECTED_PROVENANCE='identity-fixture',
                       RMUX_PERF_HOST_FINGERPRINT=environment['host_fingerprint'],
                       RMUX_PERF_EXPECTED_GIT_SHA=head, RMUX_PERF_EXPECTED_PLATFORM=host_platform,
                       PYTHONDONTWRITEBYTECODE='1')
            commands = {
                'direct': [sys.executable, str(scripts / 'perf-diff.py'), str(baseline_path),
                           str(current_path), *options, '--fail-on-regression'],
                'gate': ['bash', str(scripts / 'release-review-gate.sh'), '--section', 'perf',
                         '--target-dir', str(fixture / 'gate-output')],
                'portable': [sys.executable, str(scripts / 'check-perf-current.py'), str(current_path),
                             *options, '--require-absolute-budgets'],
            }
            for fault in ('valid', *FAULTS):
                for role in (ROLES if fault != 'valid' else ('public',)):
                    for key, command in commands.items():
                        with self.subTest(route=key, role=role, fault=fault):
                            for name, path in paths.items():
                                path.write_bytes(contents[name])
                            payload = copy.deepcopy(current)
                            image = payload['layout'][role]
                            if fault == 'missing':
                                paths[role].unlink()
                            elif fault == 'substituted':
                                paths[role].write_bytes(b'X' * len(contents[role]))
                            elif fault == 'relative':
                                image['path'] = 'relative/rmux'
                            elif fault == 'topology':
                                alternate = fixture / ('alternate-' + role)
                                alternate.write_bytes(contents[role])
                                image['path'] = str(alternate)
                            elif fault == 'size':
                                image['size_bytes'] += 1
                            elif fault == 'hash':
                                image['sha256'] = '0' * 64
                            current_path.write_text(json.dumps(payload))
                            result = subprocess.run(command, cwd=fixture, env=env,
                                                    capture_output=True, text=True, timeout=20)
                            output = result.stdout + result.stderr
                            if fault == 'valid':
                                self.assertEqual(result.returncode, 0, output)
                            else:
                                self.assertNotEqual(result.returncode, 0, output)
                                self.assertNotIn('release-review-gate=ok', output)
                                self.assertNotIn('metric baseline_mean', output)
                            print(f'identity-case route={key} role={role} fault={fault} exit={result.returncode}',
                                  flush=True)


if __name__ == '__main__':
    unittest.main()
