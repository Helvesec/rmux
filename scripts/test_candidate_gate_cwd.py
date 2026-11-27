"""Exercise the unmodified Unix entrypoints before any build or scenario work."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
HEAD = '0123456789abcdef' * 2 + '01234567'
REACHED_VALIDATION = 73
IDENTITY_FIXTURE = r'''
import json
import os
from pathlib import Path
import subprocess
import sys

with open(os.environ['CWD_FIXTURE_LOG'], 'a') as stream:
    stream.write(json.dumps(dict(argv=sys.argv[1:], cwd=str(Path.cwd()),
                                head=os.environ.get('RMUX_IDENTITY_HEAD'))) + '\n')
if sys.argv[1:3] == ['with', '--']:
    if os.environ.get('CWD_FIXTURE_BOOTSTRAP_REFUSE'):
        sys.exit(86)
    env = dict(os.environ, RMUX_IDENTITY_HEAD=os.environ['CWD_FIXTURE_HEAD'])
    sys.exit(subprocess.run(sys.argv[3:], env=env, check=False).returncode)
if sys.argv[1:] in (['validate'], ['validate-gate']):
    sys.exit(86 if os.environ.get('CWD_FIXTURE_VALIDATE_REFUSE') else 73)
sys.exit('unexpected identity fixture command')
'''


@unittest.skipUnless(os.name == 'posix', 'Unix shell entrypoints')
class GateCwdRegression(unittest.TestCase):
    def setUp(self):
        audit = SCRIPTS.parent / '.test-artifacts' / 'gate-cwd'
        audit.mkdir(parents=True, exist_ok=True)
        # Spaces also exercise quoting of every derived absolute path.
        self.repo = Path(tempfile.mkdtemp(prefix='checkout with spaces-', dir=audit))
        self.addCleanup(shutil.rmtree, self.repo)
        (self.repo / 'scripts').mkdir()
        (self.repo / 'elsewhere').mkdir()
        for name in ('gate-unix-fast.sh', 'tiny-cli-cwd-routes.sh'):
            shutil.copyfile(SCRIPTS / name, self.repo / 'scripts' / name)
        (self.repo / 'scripts' / 'candidate_identity.py').write_text(IDENTITY_FIXTURE)
        self.log = self.repo / 'events.jsonl'
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(('RMUX_IDENTITY_', 'CWD_FIXTURE_'))}
        self.env.update(RMUX_PYTHON=sys.executable, CWD_FIXTURE_LOG=str(self.log),
                        CWD_FIXTURE_HEAD=HEAD)

    def invoke(self, script, route, inherited=False, args=(), **extra):
        self.log.unlink(missing_ok=True)
        cwd, entry = {
            'root': (self.repo, 'scripts/' + script),
            'scripts': (self.repo / 'scripts', './' + script),
            'absolute': (self.repo / 'elsewhere', str(self.repo / 'scripts' / script)),
        }[route]
        env = dict(self.env, **extra)
        if inherited:
            env['RMUX_IDENTITY_HEAD'] = HEAD
        result = subprocess.run(['/bin/bash', entry, *args], cwd=cwd, env=env,
                                capture_output=True, text=True, timeout=10)
        lines = self.log.read_text().splitlines() if self.log.exists() else []
        events = [json.loads(line) for line in lines]
        return result, events

    def check_routes(self, script, validation, args):
        for route in ('root', 'scripts', 'absolute'):
            for inherited in (False, True):
                with self.subTest(script=script, route=route, inherited=inherited):
                    result, events = self.invoke(script, route, inherited, args)
                    self.assertEqual(result.returncode, REACHED_VALIDATION, result.stderr)
                    self.assertEqual(len(events), 1 if inherited else 2)
                    if not inherited:
                        self.assertEqual(events[0], dict(
                            argv=['with', '--', '/bin/bash',
                                  str(self.repo / 'scripts' / script), *args],
                            cwd=str(self.repo), head=None))
                    self.assertEqual(events[-1], dict(argv=[validation],
                                                     cwd=str(self.repo), head=HEAD))

    def test_gate_routes_fresh_and_inherited_identity(self):
        self.check_routes('gate-unix-fast.sh', 'validate-gate',
                          ['--platform', 'macos', '--test-threads', '1', '--skip-doc',
                           '--skip-source-gates', '--skip-tiny-routes', '--nextest',
                           '--install-nextest'])

    def test_tiny_routes_fresh_and_inherited_identity(self):
        self.check_routes('tiny-cli-cwd-routes.sh', 'validate',
                          ['', 'argument with spaces', 'literal*$HOME', '--flag'])

    def test_identity_refusals_stop_before_gate_or_scenario(self):
        for script in ('gate-unix-fast.sh', 'tiny-cli-cwd-routes.sh'):
            for inherited in (False, True):
                for refusal in ('BOOTSTRAP', 'VALIDATE'):
                    if inherited and refusal == 'BOOTSTRAP':
                        continue
                    with self.subTest(script=script, inherited=inherited, refusal=refusal):
                        result, events = self.invoke(
                            script, 'absolute', inherited, ['--platform', 'macos'],
                            **{'CWD_FIXTURE_' + refusal + '_REFUSE': '1'})
                        self.assertEqual(result.returncode, 86, result.stderr)
                        self.assertEqual(len(events),
                                         2 if not inherited and refusal == 'VALIDATE' else 1)

    def test_gate_help_and_invalid_arguments_do_not_bootstrap(self):
        for args, status in ((['--help'], 0), (['--unknown'], 1),
                             (['--platform', 'other'], 1),
                             (['--test-threads', '0'], 1),
                             (['--test-threads', 'NaN'], 1), (['--platform'], 1)):
            with self.subTest(args=args):
                result = subprocess.run(
                    ['/bin/bash', './gate-unix-fast.sh', *args],
                    cwd=self.repo / 'scripts', env=self.env,
                    capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertFalse(self.log.exists())


if __name__ == '__main__':
    unittest.main()
