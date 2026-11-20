"""Observed Linux bootstrap inputs must survive before their consumers start."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class NativeInputCollector(unittest.TestCase):
    def test_observed_bootstrap_is_retained_before_consumption(self):
        scripts = Path(__file__).resolve().parent
        concrete_path = type(scripts)
        with __import__('tempfile').TemporaryDirectory(
                dir=scripts.parent / '.rmux-audit') as temporary:
            root = Path(temporary)
            source, home, system = root / 'source', root / 'home', root / 'system'
            audit, toolchain = root / 'audit', root / 'toolchain'
            audit.mkdir()
            toolchain.mkdir()
            source.mkdir()
            (source / 'Cargo.lock').write_text('package = []\n')
            for name in ('wire_compat', 'wire_v8_v9', 'wire_ledger_v1', 'wire_runtime_v2'):
                p = source / 'crates/rmux-proto/tests' / (name + '.rs')
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text('// unchanged fixture source\n')
            original = json.loads((scripts / 'fixtures/native_input_observed_opens.json').read_text())
            logical_paths = [p.replace('$SOURCE', str(source)).replace('$HOME', str(home))
                             for p in original]

            def physical(path):
                p = concrete_path(path)
                if p.is_relative_to('/home/example'):
                    return home / p.relative_to('/home/example')
                if p.is_relative_to('/usr') or p.is_relative_to('/etc'):
                    return system / p.relative_to('/')
                return p

            inputs = {}
            for path in logical_paths:
                p = physical(path)
                p.parent.mkdir(parents=True, exist_ok=True)
                data = ('observed bytes: ' + path).encode()
                p.write_bytes(data)
                inputs[str(p)] = data
            credential = home / '.cargo/credentials.toml'
            credential.write_text('must never be retained\n')
            mutable = {str(home / '.cargo' / name) for name in
                       ('.package-cache', '.global-cache', '.package-cache-mutate')}
            stable = set(inputs) - mutable
            git_snapshots = []

            def git(argv, **kwargs):
                if argv[0] != '/usr/bin/git':
                    raise AssertionError(argv)
                objects = audit / 'before-objects'
                git_snapshots.append({p.name for p in objects.iterdir()})
                args = argv[3:]
                if args == ['rev-parse', 'HEAD']:
                    return 'a' * 40
                if args == ['ls-files', '-z']:
                    return 'Cargo.lock\0'
                return ''

            env = dict(NATIVE_INPUT_SOURCE=str(source), NATIVE_INPUT_AUDIT=str(audit),
                       NATIVE_INPUT_HEAD='a' * 40, NATIVE_INPUT_TOOLCHAIN=str(toolchain),
                       NATIVE_INPUT_HOME=str(home))
            with patch('pathlib.Path', side_effect=physical), \
                    patch.dict(os.environ, env), \
                    patch('subprocess.check_output', side_effect=git), \
                    patch('shutil.which', return_value=None), \
                    patch('shutil.disk_usage', return_value=SimpleNamespace(
                        free=2**40, _asdict=lambda: {'free': 2**40})):
                runpy.run_path(str(scripts / 'native_input_collector.py'))
            before_consumer = datetime.datetime.now(datetime.timezone.utc)
            record = json.loads((audit / 'before-inputs.json').read_text())
            self.assertEqual(len(inputs), 29)
            self.assertEqual(stable - set(record['files']), set(),
                             'observed loader/locale/Git/Python inputs missing before use')
            expected_hashes = {hashlib.sha256(inputs[p]).hexdigest() for p in stable}
            self.assertTrue(all(expected_hashes <= snapshot for snapshot in git_snapshots),
                            'Git consumed inputs before their bytes were retained')
            self.assertEqual(set(record['mutable_inputs']), mutable)
            self.assertFalse(mutable & set(record['files']))
            self.assertNotIn(str(credential), record['files'])
            for path in stable:
                row = record['files'][path]
                self.assertEqual(Path(row['resolved']), physical(path).resolve())
                self.assertLess(datetime.datetime.fromisoformat(row['retained_at']), before_consumer)
                self.assertEqual(Path(row['object']).read_bytes(), inputs[path])
                physical(path).write_bytes(b'changed after consumer')
                self.assertEqual(Path(row['object']).read_bytes(), inputs[path])


if __name__ == '__main__':
    unittest.main()
