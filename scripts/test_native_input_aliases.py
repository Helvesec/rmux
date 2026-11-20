"""Four measured open paths must stay bound before every Git/ldd consumer."""
from contextlib import ExitStack, redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import native_input_collector as collector


ALIASES = {
    '/usr/lib/ssl/cert.pem': '/etc/ssl/certs/ca-certificates.crt',
    '/usr/share/locale/locale.alias': '/etc/locale.alias',
    '/etc/localtime': '/usr/share/zoneinfo/Europe/Zurich',
    '/usr/lib/ssl/openssl.cnf': '/etc/ssl/openssl.cnf',
}
# Exact ordered consumer boundaries in the bounded fixture; no native subprocess.
BOUNDARIES = ('git-1', 'git-2', 'git-3', 'ldd-1', 'ldd-2',
              'git-4', 'git-5', 'git-6', 'git-7', 'git-8', 'manifest')


class CollectorFixture:
    def __init__(self, root):
        self.root = root
        self.source, self.home = root / 'source', root / 'home'
        self.audit, self.toolchain = root / 'audit', root / 'toolchain'
        for directory in (self.source, self.home, self.audit, self.toolchain):
            directory.mkdir()
        self.profile = Path(__file__).with_name('fixtures') / 'native_input_observed_opens.json'
        self.entries = json.loads(self.profile.read_text())
        self.data = {}
        for name in self.entries:
            path = self.physical(name.replace('$SOURCE', str(self.source))
                                 .replace('$HOME', str(self.home)))
            path.parent.mkdir(parents=True, exist_ok=True)
            self.data[str(path)] = ('retained: ' + name).encode()
            path.write_bytes(self.data[str(path)])
        self.bindings = {self.physical(a): self.physical(b) for a, b in ALIASES.items()}
        for alias, target in self.bindings.items():
            alias.parent.mkdir(parents=True, exist_ok=True)
            alias.symlink_to(target)
        (self.source / 'Cargo.lock').write_text('package = []\n')
        for name in ('wire_compat', 'wire_v8_v9', 'wire_ledger_v1', 'wire_runtime_v2'):
            path = self.source / 'crates/rmux-proto/tests' / (name + '.rs')
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('// fixture\n')
        self.tool = root / 'tool'
        self.tool.write_text('fixture tool\n')
        self.calls, self.snapshot = [], None
        self.manifest = self.audit / 'before-inputs.json'

    def physical(self, name):
        path = Path(name)
        if path.is_relative_to('/usr') or path.is_relative_to('/etc'):
            return self.root / 'system' / path.relative_to('/')
        return path

    def redirect(self, alias, same_bytes):
        replacement = self.root / 'replacement'
        replacement.write_bytes(self.bindings[alias].read_bytes() if same_bytes else b'changed')
        alias.unlink()
        alias.symlink_to(replacement)

    def run(self, before=None, mutate=lambda: None, observe=lambda snapshot: None):
        original_bootstrap = collector.observed_bootstrap
        counts = {'git': 0, 'ldd': 0}

        def after_boundary():
            if BOUNDARIES[len(self.calls)] == before:
                mutate()

        def bootstrap(snapshot, *args):
            result = original_bootstrap(snapshot, *args)
            self.snapshot = snapshot
            after_boundary()
            return result

        def consume(kind):
            counts[kind] += 1
            self.calls.append(f'{kind}-{counts[kind]}')
            observe(self.snapshot)
            after_boundary()

        def git(argv, **kwargs):
            if argv[:3] != ['/usr/bin/git', '-C', str(self.source)]:
                raise AssertionError(argv)
            consume('git')
            if argv[3:] == ['rev-parse', 'HEAD']:
                return 'a' * 40
            if argv[3:] == ['ls-files', '-z']:
                return 'Cargo.lock\0'
            return ''

        def ldd(argv, **kwargs):
            if argv != ['/usr/bin/ldd', str(self.tool)]:
                raise AssertionError(argv)
            consume('ldd')
            return SimpleNamespace(stdout='')

        env = dict(NATIVE_INPUT_SOURCE=str(self.source), NATIVE_INPUT_HOME=str(self.home),
                   NATIVE_INPUT_AUDIT=str(self.audit), NATIVE_INPUT_HEAD='a' * 40,
                   NATIVE_INPUT_TOOLCHAIN=str(self.toolchain),
                   NATIVE_INPUT_OBSERVED_OPEN_PROFILE=str(self.profile))
        with ExitStack() as stack:
            stack.enter_context(patch.object(collector, 'Path', side_effect=self.physical))
            stack.enter_context(patch.object(collector, 'observed_bootstrap', side_effect=bootstrap))
            stack.enter_context(patch.dict(os.environ, env))
            stack.enter_context(patch('subprocess.check_output', side_effect=git))
            stack.enter_context(patch('subprocess.run', side_effect=ldd))
            stack.enter_context(patch('shutil.which', side_effect=lambda name:
                                     str(self.tool) if name in ('git', 'python3') else None))
            stack.enter_context(patch('shutil.disk_usage', return_value=SimpleNamespace(
                free=2**40, _asdict=lambda: {'free': 2**40})))
            stack.enter_context(redirect_stdout(io.StringIO()))
            collector.main()


class NativeInputAliases(unittest.TestCase):
    def fixture(self):
        audit = Path(__file__).resolve().parents[1] / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(temporary.cleanup)
        return CollectorFixture(Path(temporary.name))

    def test_four_aliases_retained_before_every_consumer_and_manifest(self):
        fixture = self.fixture()
        mutable = {str(fixture.home / '.cargo' / n) for n in collector.MUTABLE_CARGO}
        stable = set(fixture.data) - mutable
        self.assertEqual((len(fixture.entries), len(stable), len(mutable)), (29, 26, 3))

        def retained(snapshot):
            self.assertTrue(stable <= snapshot.files.keys())
            self.assertEqual(set(snapshot.mutable_inputs), mutable)
            self.assertFalse(mutable & snapshot.files.keys())
            self.assertEqual(len({snapshot.files[p]['object'] for p in stable}), 26)
            for alias, target in fixture.bindings.items():
                self.assertIn(str(alias), snapshot.files, 'observed alias binding missing')
                row, canonical = snapshot.files[str(alias)], snapshot.files[str(target)]
                self.assertEqual(row['resolved'], str(target.resolve()))
                self.assertEqual(row['object'], canonical['object'])
                obj = Path(row['object'])
                self.assertNotEqual(obj.stat().st_ino, target.stat().st_ino)
                self.assertEqual(obj.read_bytes(), fixture.data[str(target)])
                self.assertEqual(row['sha256'], hashlib.sha256(obj.read_bytes()).hexdigest())

        fixture.run(observe=retained)
        self.assertEqual(fixture.calls, list(BOUNDARIES[:-1]))
        record = json.loads(fixture.manifest.read_text())
        self.assertFalse(record['command_qualified_before_execution'])
        self.assertEqual(record['files'], fixture.snapshot.files)
        retained(fixture.snapshot)
        for alias, target in fixture.bindings.items():
            target.write_bytes(b'mutated after consumers')
            self.assertEqual(Path(record['files'][str(alias)]['object']).read_bytes(),
                             fixture.data[str(target)])

    def test_four_alias_redirections_refused_before_each_consumer_and_manifest(self):
        for name in ALIASES:
            for same_bytes in (False, True):
                for boundary in BOUNDARIES:
                    with self.subTest(alias=name, same_bytes=same_bytes, boundary=boundary):
                        fixture = self.fixture()
                        alias = fixture.physical(name)
                        with self.assertRaisesRegex(RuntimeError, 'native input'):
                            fixture.run(before=boundary, mutate=lambda:
                                        fixture.redirect(alias, same_bytes))
                        self.assertEqual(fixture.calls, list(BOUNDARIES[:BOUNDARIES.index(boundary)]))
                        self.assertFalse(fixture.manifest.exists())


if __name__ == '__main__':
    unittest.main()
