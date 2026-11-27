"""Selected Unix images must retain the caller's argv byte for byte."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch


@unittest.skipIf(os.name == 'nt', 'Unix argv[0] contract')
class CandidateIdentityArgv0(unittest.TestCase):
    def test_selected_launcher_and_producer_keep_cargo_program_name(self):
        # rmux keys its tmux personality on argv[0] and its tmux shim on
        # current_exe; neither may become a digest or a generic object name.
        code = 'fn main() { println!("{}", std::env::current_exe().unwrap().display()); }\n'
        env, script, target = self._identity_fixture(code, 'name-probe-0123456789abcdef')
        selected = Path(subprocess.check_output(
            [env['RMUX_IDENTITY_PYTHON'], str(script), 'check', str(target / 'name-probe-0123456789abcdef')],
            env=env, text=True).strip())
        self.assertEqual(selected.name, 'name-probe')
        result = subprocess.run([str(selected)], env={'PATH': os.defpath}, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        executed = Path(result.stdout.strip())
        self.assertEqual(executed.name, 'name-probe')
        self.assertNotEqual(executed.resolve(), selected.resolve())

    def test_selected_launcher_preserves_argv0_and_arguments(self):
        code = 'use std::os::unix::ffi::OsStrExt;\nfn main() { for a in std::env::args_os() { for b in a.as_bytes() { print!("{b:02x}"); } println!(); } }\n'
        env, script, target = self._identity_fixture(code, 'argv-sentinel')
        binary = target / 'argv-sentinel'
        selected = subprocess.check_output([env['RMUX_IDENTITY_PYTHON'], str(script),
                                            'check', str(binary)], env=env, text=True).strip()
        arguments = [b'', b'with spaces', b'--help', b'--', b'"$`\\\n', b'nonutf8-\xff']
        for arg0 in (b'rmux', b'tmux', b'/path with spaces/tmux', b'helper-\xff'):
            with self.subTest(arg0=arg0):
                argv = [arg0, *arguments]
                result = subprocess.run(argv, executable=selected, env={'PATH': os.defpath},
                                        capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), [a.hex().encode() for a in argv])
                self.assertEqual(result.stderr, b'')

    def _identity_fixture(self, program, output_name):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = script.parent.parent / '.rmux-audit' / 's3-185' / 'fixtures'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='argv0-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.toml').write_text('[package]\nname="argv0"\nversion="0.1.0"\nedition="2021"\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        code = source / 'main.rs'
        code.write_text(program)
        git = ['git', '-C', str(source)]
        for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                     '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
            subprocess.run(git + args, check=True)
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if (key.startswith('RMUX_IDENTITY_') or
                    (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER')) or
                    key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')):
                env.pop(key)
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                env = guard.prepare()
        finally:
            os.chdir(previous)
        target = Path(env['RMUX_IDENTITY_TARGET'])
        env['CARGO_MANIFEST_DIR'] = str(source)
        with patch.dict(os.environ, env, clear=True):
            self.assertEqual(guard.rustc_wrapper([str(rustc), '--crate-name', 'argv_sentinel',
                             '--crate-type', 'bin', str(code), '-o', str(target / output_name)]), 0)
        return env, script, target


# Records what the identity runner of a certified fixture receives. It stands in
# for candidate_identity.py, so the fixture under test is the certified one.
RECEIVER = '''import json, os, signal, sys
from pathlib import Path
here = Path(__file__).parent
null = os.stat('/dev/null').st_rdev
seen = dict(argv=[os.fsencode(a).hex() for a in sys.argv[1:]], cwd=os.getcwd(), pid=os.getpid(),
            env={k.hex(): v.hex() for k, v in os.environb.items()},
            ignored=[signal.getsignal(s) == signal.SIG_IGN for s in (signal.SIGINT, signal.SIGQUIT)],
            null=[os.fstat(fd).st_rdev == null for fd in (0, 1, 2)])
(here / 'partial').write_text(json.dumps(seen))
os.replace(here / 'partial', here / 'seen.json')
'''


@unittest.skipIf(os.name == 'nt', 'Unix generated fixture launchers')
class GeneratedFixtureLaunchers(unittest.TestCase):
    """SDK and CLI test launchers must hand the runner the caller's env and argv byte for byte."""

    # dash drops the names that are not shell identifiers; shells also rewrite
    # PWD, OLDPWD, SHLVL, _ and IFS. Cargo exports CARGO_BIN_EXE_rmux-daemon.
    ENV = {b'PATH': b'/usr/bin:/bin', b'R2970_PLAIN': b'1', b'R2970-DASH': b'1',
           b'R2970.DOT': b'1', b'R2970 SPACE': b'1', b'CARGO_BIN_EXE_rmux-daemon': b'/x/rmux-daemon',
           b'PWD': b'/stale-pwd', b'OLDPWD': b'/old', b'SHLVL': b'7', b'_': b'/stale-underscore',
           b'R2970_NL': b'a\nb', b'R2970_EMPTY': b'', b'R2970_HIGH': b'\xff\xfe', b'IFS': b'x',
           b'ENV': b'/nonexistent-env', b'CDPATH': b'/'}
    ARGS = [b'a b', b'', b'$HOME', b"it's", b'x\ny', b'--flag=*', b'-', b'nonutf8-\xff']

    def test_sdk_and_cli_launchers_keep_env_and_argv(self):
        fixtures = importlib.util.spec_from_file_location(
            'fixtures', Path(__file__).with_name('candidate_identity_fixtures.py'))
        module = importlib.util.module_from_spec(fixtures)
        fixtures.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            env = {**self.ENV, b'HOME': os.environb.get(b'HOME', b'/'),
                   b'LANG': os.environb.get(b'LANG', b'C.UTF-8'),
                   b'RMUX_SDK_REAL_DAEMON': os.fsencode(root / "real daemon-'é")}
            caller = root / 'caller dir'
            caller.mkdir()
            daemon = root / 'rmux bin-é'
            for kind, template, cwd in (
                    ('sdk', b'#!/bin/sh\ncd / || exit 1\nexec "$RMUX_SDK_REAL_DAEMON" "$@"\n', '/'),
                    ('cli', b"#!/bin/sh\n'" + os.fsencode(daemon) + b"' \"$@\" >/dev/null 2>&1 &\necho $! > '"
                     + os.fsencode(root / 'cli pid') + b"'\nexit 0\n", str(caller))):
                with self.subTest(kind=kind):
                    fixture, receiver = self._certify(module, root / kind, template)
                    seen = receiver.with_name('seen.json')
                    subprocess.run([sys.executable, str(receiver), 'exec'], env=env, cwd=caller, check=True)
                    direct = json.loads(seen.read_bytes())
                    seen.unlink()
                    launcher = subprocess.Popen([fixture, *self.ARGS], env=env, cwd=caller,
                                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    stdout, stderr = launcher.communicate()
                    self.assertEqual((launcher.returncode, stdout, stderr), (0, b'', b''))
                    deadline = time.monotonic() + 5
                    while not seen.exists() and time.monotonic() < deadline:
                        time.sleep(0.005)
                    received = json.loads(seen.read_bytes())
                    names = lambda record: {bytes.fromhex(k) for k in record['env']}
                    self.assertEqual(names(direct) - names(received), set(), 'names lost by the launcher')
                    self.assertEqual(names(received) - names(direct), set(), 'names added by the launcher')
                    self.assertEqual(received['env'], direct['env'], 'values rewritten by the launcher')
                    selected = env[b'RMUX_SDK_REAL_DAEMON'] if kind == 'sdk' else os.fsencode(daemon)
                    self.assertEqual([bytes.fromhex(a) for a in received['argv']],
                                     [b'exec', selected, *self.ARGS])
                    self.assertEqual(received['cwd'], cwd)
                    if kind == 'sdk':
                        # The template's exec: the runner keeps the PID its caller spawned.
                        self.assertEqual(received['pid'], launcher.pid,
                                         'the SDK launcher must exec the runner in its own PID')
                    if kind == 'cli':
                        # A background job of a non-interactive shell.
                        self.assertEqual((root / 'cli pid').read_text(), f"{received['pid']}\n")
                        self.assertEqual(received['ignored'], [True, True])
                        self.assertEqual(received['null'], [True, True, True])
                    shebang = fixture.resolve().read_bytes().split(b'\n', 1)[0]
                    self.assertEqual(shebang, b'#!' + os.fsencode(sys.executable),
                                     'a #!/bin/sh launcher depends on the host shell')

    def _certify(self, module, directory, template):
        directory.mkdir()
        receiver = directory / 'receiver.py'
        receiver.write_text(RECEIVER)
        producer = directory / 'producer'
        producer.write_bytes(b'producer')
        build = directory / 'producer.json'
        build.write_text(json.dumps(dict(kind='rust-test', input='i', input_sha256='0' * 64,
                                         compiler='c', compiler_sha256='0' * 64)))
        retention = SimpleNamespace(retain=lambda g, binary, target, sha: binary,
                                    publish=lambda *args: None)
        guard = SimpleNamespace(
            __file__=str(receiver), IdentityError=RuntimeError,
            context=lambda: (directory, '0' * 40, directory),
            source_identity=lambda source, head: (source, '0' * 64), host_os=lambda: 'test',
            verify_built=lambda path, *args: dict(path=str(path), sha256='1' * 64, build=str(build)),
            verify=lambda binary, *args: dict(path=str(binary), sha256='2' * 64),
            stamp=lambda st: (st.st_ino, st.st_size, st.st_mtime_ns),
            support=lambda name: retention if name == 'candidate_identity_retention' else module_named(name))
        fixture = directory / 'fixture'
        fixture.write_bytes(template)
        fixture.chmod(0o700)
        with patch.object(module, 'parent_executable', return_value=producer):
            self.assertEqual(module.certify([str(fixture), directory.name], guard), 0)
        return fixture, receiver


def module_named(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


if __name__ == '__main__':
    unittest.main()
