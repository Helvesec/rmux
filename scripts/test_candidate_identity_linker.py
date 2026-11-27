"""Frozen INT590 regression: Python argv fixtures, never native build proof.

Zig 0.15.2/cargo-zigbuild 0.23.0 rejected --undefined in the real
glibc 2.31 package link. The qualified stderr and tool hashes are retained
with S5267's evidence. Exercise both actual injection sites before the
compiler boundary; native linking/strip remains a separate obligation.
"""
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('candidate_identity.py')
SYMBOLS = ('RMUX_BUILD_IDENTITY_V1', 'RMUX_BUILD_GRAPH_V1')
# This subprocess is an explicit argv fixture, NOT Zig or a linker emulator.
# It preserves the INT590 diagnostic for the unsupported spelling and requires
# each retained root. It produces neither object files nor executable images.
DRIVER = '''import json, sys
argv = json.loads(sys.argv[1])
if any(arg.startswith('-Wl,--undefined') for arg in argv):
    sys.stderr.write('error: unsupported linker arg: --undefined\\n')
    sys.exit(1)
expected = ['-Wl,-u,' + name for name in json.loads(sys.argv[2])]
if any(argv.count(arg) != 1 for arg in expected):
    sys.stderr.write('missing or repeated retention root\\n')
    sys.exit(2)
print(json.dumps(argv))
'''


class IdentityLinkerArguments(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('linker_guard', SCRIPT)
        self.guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.guard)
        self.units = self.guard.support('candidate_identity_units')
        audit = SCRIPT.resolve().parent.parent / '.rmux-audit' / 'identity-linker'
        audit.mkdir(parents=True, exist_ok=True)
        self.root = Path(tempfile.mkdtemp(dir=audit)).resolve()
        self.target = self.root / 'target'
        self.target.mkdir()
        (self.target / 'identity.o').write_bytes(b'identity object fixture')

    def graph_flags(self, platform):
        g = SimpleNamespace(host_os=lambda: platform, jobserver=lambda: {})
        with patch.object(self.units.subprocess, 'run') as compiler:
            # This fixture exercises the legacy V1 graph marker (no lock context).
            flags = self.units.graph_object(g, {'graph': {}, 'graph_sha256': 'a' * 64},
                                           self.root, 'b' * 40, self.target,
                                           'compiler-fixture-never-executed')
        self.assertEqual(compiler.call_count, 1)
        self.assertIn('--emit=obj', compiler.call_args.args[0])
        code = self.target / 'identity-graphs' / ('a' * 64 + '.rs')
        self.assertIn('RMUX_BUILD_GRAPH_V1:' + 'a' * 64 + ':END', code.read_text())
        self.assertIn('#[used]', code.read_text())
        return flags

    def wrapper_flags(self, platform, kind, doctest=False):
        g = self.guard
        args = ['compiler-fixture-never-executed', '--crate-name',
                'build_script_build' if kind == 'build-script' else 'fixture',
                '--crate-type', 'bin', 'fixture.rs']
        if kind == 'rust-test':
            args += ['--test']
        env = {'RMUX_IDENTITY_RUSTC': args[0]}
        if doctest:
            directory = self.target / 'doctest-tmp'
            directory.mkdir()
            response = directory / 'args'
            response.write_text('\n'.join([*args[1:], '-Zunstable-options']))
            args = [args[0], '@' + str(response)]
            env['RMUX_IDENTITY_DOC_ORIGIN'] = str(self.root / 'fixture.rs')
        with ExitStack() as stack:
            stack.enter_context(patch.dict(os.environ, env, clear=True))
            stack.enter_context(patch.object(g, 'host_os', return_value=platform))
            stack.enter_context(patch.object(g, 'context', return_value=(self.root, 'b' * 40, self.target)))
            for name in ('source_identity', 'target_identity', 'compiler_inputs', 'generation'):
                stack.enter_context(patch.object(g, name))
            stack.enter_context(patch.object(self.units, 'before', return_value={'kind': kind}))
            stack.enter_context(patch.object(self.units, 'with_depinfo', side_effect=lambda a, u: a))
            stack.enter_context(patch.object(self.units, 'graph_object', return_value=self.graph_flags(platform)))
            observed = stack.enter_context(patch.object(self.units, 'compile_observed', return_value=0))
            self.assertEqual(g.rustc_wrapper(args), 0)
            final = observed.call_args.args[1]
        self.assertNotIn('-Zunstable-options', final)
        return final

    def assert_retention(self, args, platform, symbols):
        linked = [arg.removeprefix('link-arg=') for arg in args if arg.startswith('link-arg=')]
        for symbol in symbols:
            expected = {'linux': '-Wl,-u,' + symbol,
                        'macos': '-Wl,-u,_' + symbol,
                        'windows': '/INCLUDE:' + symbol}[platform]
            self.assertEqual(linked.count(expected), 1, linked)
        if platform == 'linux':
            result = subprocess.run([sys.executable, '-c', DRIVER, json.dumps(linked),
                                     json.dumps(symbols)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_linux_graph_root(self):
        self.assert_retention(self.graph_flags('linux'), 'linux', SYMBOLS[1:])

    def test_linux_all_wrapper_link_roles(self):
        for kind, doctest in (('bin', False), ('rust-test', False),
                              ('build-script', False), ('rust-test', True)):
            with self.subTest(kind=kind, doctest=doctest):
                self.assert_retention(self.wrapper_flags('linux', kind, doctest), 'linux', SYMBOLS)

    def test_apple_and_msvc_spelling_unchanged(self):
        for platform in ('macos', 'windows'):
            for kind in ('bin', 'rust-test', 'build-script'):
                with self.subTest(platform=platform, kind=kind):
                    self.assert_retention(self.wrapper_flags(platform, kind), platform, SYMBOLS)

    def test_int590_unsupported_spelling_remains_a_negative_control(self):
        for symbol in SYMBOLS:
            result = subprocess.run([sys.executable, '-c', DRIVER,
                                     json.dumps(['-Wl,--undefined=' + symbol]),
                                     json.dumps([symbol])], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stderr, 'error: unsupported linker arg: --undefined\n')
        for args in ([], ['-Wl,-u,' + SYMBOLS[0]] * 2):
            result = subprocess.run([sys.executable, '-c', DRIVER, json.dumps(args),
                                     json.dumps(SYMBOLS)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)


if __name__ == '__main__':
    unittest.main()
