"""Runner boundary checks; Cargo and native product binaries are never invoked."""
import contextlib
import copy
import io
import itertools
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from run_references import execute, host_platform, run


LIST = ['--list', '--format', 'terse']
EXACT = ['module::queue_test', '--exact', '--nocapture', '--test-threads=1']
LISTING = 'module::queue_test: test\n'
PASSED = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'


def profiles(target='lib'):
    return {'q': {'tests': [{'package': 'example', 'target': target,
                            'symbol': 'queue_test',
                            'platforms': ['linux', 'macos', 'windows']}]}}


def artifact(binary):
    return json.dumps({'reason': 'compiler-artifact', 'executable': str(binary),
                       'profile': {'test': True}})


class NativeRunnerBoundary(unittest.TestCase):
    def test_target_argv_and_deadlines_with_and_without_identity(self):
        root = Path('/fixture')
        prefix = ['/fixture/python with spaces', '/fixture/identity with spaces.py', 'run']
        for platform, target, cargo_target, identity in itertools.product(
                ('linux', 'macos', 'windows'), ('lib', 'bin:example', 'test:queue'),
                (None, 'native-triple'), (False, True)):
            commands = []

            def fixture_run(command, cwd, timeout=None):
                self.assertEqual(cwd, root)
                commands.append((command, timeout))
                if command[0] == 'cargo':
                    return artifact('/fixture/test image')
                return LISTING if '--list' in command else PASSED

            # A non-identity Cargo runner alone does not create an identity context.
            env = {'CARGO_TARGET_OTHER_RUNNER': 'unrelated-runner'}
            if identity:
                env.update(RMUX_IDENTITY_PYTHON=prefix[0], RMUX_IDENTITY_SCRIPT=prefix[1])
            with self.subTest(platform=platform, target=target, cargo_target=cargo_target,
                              identity=identity), patch.dict(os.environ, env, clear=True), \
                    patch('run_references.host_platform', return_value=platform), \
                    patch('run_references.run', fixture_run), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(execute(root, profiles(target), cargo_target)['passed'], 1)
            flags = ['--lib'] if target == 'lib' else ['--' + target.split(':')[0], target.split(':')[1]]
            cargo = ['cargo', 'test', '-p', 'example', '--offline', '--locked', *flags,
                     '--no-run', '--message-format=json']
            if cargo_target:
                cargo += ['--target', cargo_target]
            selected = (prefix if identity else []) + ['/fixture/test image']
            # Only a Windows identity runner proves the image before libtest;
            # that cold proof must never run under the 60 s product deadline.
            qualify = ([(prefix[:-1] + ['check', '/fixture/test image'], None)]
                       if identity and platform == 'windows' else [])
            self.assertEqual(commands, [(cargo, None), *qualify, (selected + LIST, 60),
                                        (selected + EXACT, 60)])

    def test_windows_identity_refusal_stops_before_any_image_launch(self):
        commands = []

        def fixture_run(command, cwd, timeout=None):
            commands.append((command, timeout))
            if command[0] == 'cargo':
                return artifact('C:/fixture/test.exe')
            if 'check' in command:
                raise ValueError('candidate identity FAIL: executable outside candidate target')
            return LISTING if '--list' in command else PASSED

        env = {'RMUX_IDENTITY_PYTHON': 'C:/Python/python.exe',
               'RMUX_IDENTITY_SCRIPT': 'C:/fixture/identity.py'}
        with patch.dict(os.environ, env, clear=True), \
                patch('run_references.host_platform', return_value='windows'), \
                patch('run_references.run', fixture_run), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(ValueError, 'candidate identity FAIL'):
                execute(Path('/fixture'), profiles(), 'x86_64-pc-windows-msvc')
        self.assertEqual(commands[1:], [(['C:/Python/python.exe', 'C:/fixture/identity.py',
                                          'check', 'C:/fixture/test.exe'], None)])

    def test_sdk_fixture_build_stays_outside_exact_deadline(self):
        commands = []

        def fixture_run(command, cwd, timeout=None):
            commands.append((command, timeout))
            if command[0] == 'cargo':
                return artifact('/fixture/test')
            return LISTING if '--list' in command else PASSED

        selected = profiles('test:terminal_automation')
        selected['q']['tests'][0]['package'] = 'rmux-sdk'
        env = {'RMUX_IDENTITY_PYTHON': '/fixture/python',
               'RMUX_IDENTITY_SCRIPT': '/fixture/identity.py'}
        with patch.dict(os.environ, env, clear=True), patch('run_references.run', fixture_run), \
                contextlib.redirect_stdout(io.StringIO()):
            execute(Path('/fixture'), selected, 'native-triple')
        self.assertEqual(commands[-2], (['cargo', 'build', '-p', 'rmux', '--bin', 'rmux',
                                        '--offline', '--locked', '--target', 'native-triple'], None))
        self.assertEqual(commands[-1], (['/fixture/python', '/fixture/identity.py',
                                        'run', '/fixture/test', *EXACT], 60))

    def test_incomplete_identity_is_refused_before_any_command(self):
        contexts = [{key: value} for key, value in (
            ('RMUX_IDENTITY_PYTHON', '/fixture/python'),
            ('RMUX_IDENTITY_SCRIPT', '/fixture/identity.py'),
            ('RMUX_IDENTITY_HEAD', 'a' * 40),
            ('RMUX_IDENTITY_SOURCE', '/fixture/source'),
            ('RMUX_IDENTITY_TARGET', '/fixture/target'))]
        contexts += [
            {'RMUX_IDENTITY_PYTHON': '', 'RMUX_IDENTITY_SCRIPT': '/fixture/identity.py'},
            {'RMUX_IDENTITY_PYTHON': '/fixture/python', 'RMUX_IDENTITY_SCRIPT': ''},
            {'RMUX_IDENTITY_PYTHON': '', 'RMUX_IDENTITY_SCRIPT': ''}]
        for env in contexts:
            with self.subTest(env=env), patch.dict(os.environ, env, clear=True), \
                    patch('run_references.run') as launch:
                with self.assertRaisesRegex(ValueError, 'incomplete.*identity'):
                    execute(Path('/fixture'), profiles())
                launch.assert_not_called()

    def test_identity_selection_still_qualifies_every_name_before_exact(self):
        env = {'RMUX_IDENTITY_PYTHON': '/fixture/python',
               'RMUX_IDENTITY_SCRIPT': '/fixture/identity.py'}
        for listing, excluded, message in (
                ('other::test: test\n', False, 'expected one native test'),
                (LISTING + 'other::queue_test: test\n', False, 'expected one native test'),
                (LISTING, True, 'incorrectly excluded native test')):
            commands = []
            selected = copy.deepcopy(profiles())
            if excluded:
                selected['q']['tests'][0]['platforms'] = ['windows']

            def fixture_run(command, cwd, timeout=None):
                commands.append(command)
                return artifact('/fixture/test') if command[0] == 'cargo' else listing

            with self.subTest(listing=listing, excluded=excluded), \
                    patch.dict(os.environ, env, clear=True), \
                    patch('run_references.host_platform', return_value='linux'), \
                    patch('run_references.run', fixture_run):
                with self.assertRaisesRegex(ValueError, message):
                    execute(Path('/fixture'), selected)
            self.assertFalse(any('--exact' in command for command in commands))
            self.assertEqual(commands[-1], ['/fixture/python', '/fixture/identity.py',
                                           'run', '/fixture/test', *LIST])

    def test_windows_native_input_keeps_isolated_python_startup(self):
        root = Path('/fixture')
        commands = []

        def fixture_run(command, cwd, timeout=None):
            commands.append(command)
            if command[0] == 'cargo':
                return artifact('C:/fixture/test.exe')
            return LISTING if '--list' in command else PASSED

        env = {'RMUX_IDENTITY_PYTHON': 'C:/Python/python.exe',
               'RMUX_IDENTITY_SCRIPT': 'C:/fixture/identity.py',
               'RMUX_NATIVE_INPUT_MANIFEST': 'C:/fixture/input.json'}
        with patch.dict(os.environ, env, clear=True), \
                patch('run_references.host_platform', return_value='windows'), \
                patch('run_references.run', fixture_run), contextlib.redirect_stdout(io.StringIO()):
            execute(root, profiles())
        startup = [env['RMUX_IDENTITY_PYTHON'], '-I', '-S', '-B', env['RMUX_IDENTITY_SCRIPT']]
        prefix = [*startup, 'run', 'C:/fixture/test.exe']
        self.assertEqual(commands[1:], [[*startup, 'check', 'C:/fixture/test.exe'],
                                        prefix + LIST, prefix + EXACT])


RECORD = '''import json, os, sys
from pathlib import Path
def record(role):
    with open(os.environ['QUEUE_FIXTURE_LOG'], 'a', encoding='utf-8') as log:
        log.write(json.dumps({'role': role, 'argv': sys.argv[1:], 'cwd': os.getcwd(),
                              'token': os.environ.get('QUEUE_FIXTURE_TOKEN'),
                              'stack': os.environ.get('RUST_MIN_STACK')}) + '\\n')
'''
RUNNER = RECORD + '''import subprocess
record('runner')
if sys.argv[1] == 'check':
    sys.exit(0)
assert sys.argv[1] == 'run'
if os.environ.get('QUEUE_FIXTURE_REFUSE') in ('all', 'list' if '--list' in sys.argv else 'exact'):
    print('fixture runner refused', flush=True)
    print('fixture runner stderr', file=sys.stderr, flush=True)
    sys.exit(86)
sys.exit(subprocess.call([sys.executable, *sys.argv[2:]]))
'''
IMAGE = RECORD + '''record('image')
if sys.argv[1:] == ['--list', '--format', 'terse']:
    print('module::queue_test: test')
else:
    assert sys.argv[1:] == ['module::queue_test', '--exact', '--nocapture', '--test-threads=1']
    print('fixture stdout', flush=True)
    print('fixture stderr', file=sys.stderr, flush=True)
    print('test result: ok. 1 passed; 0 failed; 0 ignored;', flush=True)
    sys.exit(int(os.environ.get('QUEUE_FIXTURE_EXIT', '0')))
'''


class LocalFixtureProcesses(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='queue runner boundary ')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.runner, self.image = self.root / 'identity fixture.py', self.root / 'test image.py'
        self.runner.write_text(RUNNER, encoding='utf-8')
        self.image.write_text(IMAGE, encoding='utf-8')
        self.log = self.root / 'launches.jsonl'
        self.env = {'RMUX_IDENTITY_PYTHON': sys.executable,
                    'RMUX_IDENTITY_SCRIPT': str(self.runner),
                    'QUEUE_FIXTURE_LOG': str(self.log), 'QUEUE_FIXTURE_TOKEN': 'unchanged',
                    'RUST_MIN_STACK': '1024'}
        if 'SystemRoot' in os.environ:
            self.env['SystemRoot'] = os.environ['SystemRoot']

    def launches(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def execute_fixture(self):
        self.outputs = []

        def fixture_run(command, cwd, timeout=None):
            if command[0] == 'cargo':
                return artifact(self.image)
            output = run(command, cwd, timeout)
            self.outputs.append(output)
            return output

        with patch.dict(os.environ, self.env, clear=True), \
                patch('run_references.run', fixture_run), contextlib.redirect_stdout(io.StringIO()):
            return execute(self.root, profiles())

    def test_real_fixture_runner_preserves_argv_environment_cwd_and_output(self):
        self.assertEqual(self.execute_fixture()['passed'], 1)
        records = self.launches()
        # Real child processes keep the host's own Windows qualification step.
        qualify = [['check', str(self.image)]] if host_platform() == 'windows' else []
        self.assertEqual([row['role'] for row in records],
                         ['runner'] * len(qualify) + ['runner', 'image', 'runner', 'image'])
        self.assertEqual([row['argv'] for row in records], [
            *qualify, ['run', str(self.image), *LIST], LIST, ['run', str(self.image), *EXACT], EXACT])
        for row in records:
            self.assertEqual(row['cwd'], str(self.root))
            self.assertEqual(row['token'], 'unchanged')
            self.assertEqual(row['stack'], '8388608')
        self.assertEqual(self.outputs, [LISTING, 'fixture stdout\nfixture stderr\n' + PASSED])

    def test_runner_refusal_has_no_raw_fallback(self):
        for phase in ('list', 'exact'):
            self.env['QUEUE_FIXTURE_REFUSE'] = phase
            self.env['QUEUE_FIXTURE_LOG'] = str(self.root / (phase + '.jsonl'))
            with self.subTest(phase=phase), self.assertRaisesRegex(
                    ValueError, 'fixture runner refused\nfixture runner stderr'):
                self.execute_fixture()
            records = [json.loads(line) for line in Path(self.env['QUEUE_FIXTURE_LOG']).read_text().splitlines()]
            qualify = ['runner'] if host_platform() == 'windows' else []
            self.assertEqual([row['role'] for row in records], qualify + (
                ['runner'] if phase == 'list' else ['runner', 'image', 'runner']))

    def test_missing_runner_never_launches_the_image(self):
        self.env['RMUX_IDENTITY_SCRIPT'] = str(self.root / 'missing runner.py')
        with self.assertRaises(ValueError):
            self.execute_fixture()
        self.assertEqual(self.launches(), [])
        self.env['RMUX_IDENTITY_PYTHON'] = str(self.root / 'missing interpreter')
        with self.assertRaises(OSError):
            self.execute_fixture()
        self.assertEqual(self.launches(), [])

    def test_nonzero_image_exit_is_not_hidden_by_passing_output(self):
        self.env['QUEUE_FIXTURE_EXIT'] = '37'
        with self.assertRaisesRegex(ValueError, PASSED.strip()):
            self.execute_fixture()
        self.assertEqual([row['role'] for row in self.launches()],
                         ['runner'] * (host_platform() == 'windows') + ['runner', 'image', 'runner', 'image'])


if __name__ == '__main__':
    unittest.main()
