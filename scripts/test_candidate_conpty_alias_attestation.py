"""Cargo aliases and libtest selection must retain the pinned VT contract."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import struct
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CargoAliasAttestation(unittest.TestCase):
    def test_alias_publication_selection_and_vt_execution(self):
        runtime, guard = load('candidate_conpty_runtime'), load('candidate_identity')
        audit = Path(__file__).resolve().parent.parent / '.rmux-audit/conpty-alias-acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as root:
            target = Path(root).resolve()
            store = target / 'runtime'
            store.mkdir()
            files = []
            for name in ('conpty.dll', 'OpenConsole.exe'):
                (store / name).write_bytes(name.encode())
                files.append(dict(name=name, sha256=runtime.digest(store / name)))
            manifest = dict(files=files, pe_machine=0x8664, package='pinned', version='pinned')
            native = SimpleNamespace(name='nt')
            for profile in ('debug', 'x86_64-pc-windows-msvc/debug'):
                with self.subTest(publication=profile):
                    deps = target / profile / 'deps'
                    deps.mkdir(parents=True)
                    image = deps / 'rmux-123.exe'
                    pe = bytearray(128)
                    pe[:2] = b'MZ'
                    struct.pack_into('<I', pe, 0x3c, 64)
                    pe[64:68] = b'PE\0\0'
                    struct.pack_into('<H', pe, 68, 0x8664)
                    image.write_bytes(pe)
                    with patch.object(runtime, 'manifest', return_value=(manifest, 'hash')), \
                            patch.object(runtime, 'initialize', return_value=store):
                        runtime.stage(guard, image, target)
                    # Cargo publishes the alias after the rustc wrapper returns.
                    alias = deps.parent / 'rmux.exe'
                    shutil.copyfile(image, alias)
                    for item in files:
                        self.assertTrue(alias.with_name(item['name']).is_file(),
                                        'Cargo alias directory omitted the pinned pair')
                        alias.with_name(item['name']).unlink(missing_ok=True)
                    record = dict(path=str(alias), target=str(target), commit='a' * 40)
                    with patch.object(guard, 'os', native), \
                            patch.object(guard.sys, 'argv', ['identity', 'check', str(alias)]), \
                            patch.object(guard, 'context', return_value=(target, 'a' * 40, target)), \
                            patch.object(guard, 'verify_built', return_value=record), \
                            patch.object(guard, 'record_launch'), \
                            patch.object(guard, 'support', return_value=runtime), \
                            patch.object(runtime, 'manifest', return_value=(manifest, 'hash')), \
                            patch.object(runtime, 'initialize', return_value=store):
                        self.assertEqual(guard.main(), 0)
                    for item in files:
                        self.assertEqual(runtime.digest(alias.with_name(item['name'])), item['sha256'])

            image = target / 'vt-tests.exe'
            image.write_bytes(b'qualified libtest')
            build = target / 'build.json'
            build.write_text(json.dumps(dict(kind='rust-test')))
            record = dict(path=str(image), target=str(target), commit='a' * 40,
                          sha256=hashlib.sha256(image.read_bytes()).hexdigest(), build=str(build))
            for args, selected, loaded, rejected in (
                    (['wrap1::', '--list'], True, False, False),
                    (['--test-threads=1'], True, False, True),
                    (['--test-threads=1'], True, True, False),
                    (['handler::'], True, False, True),
                    (['wrap1::', '--skip', 'wrap1::'], False, False, False),
                    (['unrelated'], False, False, False)):
                with self.subTest(args=args, loaded=loaded):
                    def popen(argv, **kwargs):
                        self.assertEqual(kwargs.pop('creation_image'), image)
                        self.assertTrue(kwargs.pop('capture_creation'))
                        if '--list' in argv and kwargs.get('stdout') is not None:
                            data = ('handler::display_message_tests::wrap1::vt_case: test\n'
                                    if selected else 'other::non_vt: test\n')
                            kwargs['stdout'].write(data.encode())
                            kwargs['stdout'].flush()
                        process = Mock(_handle=7, creation_identity={})
                        process.wait.return_value = 0
                        process.__enter__ = Mock(return_value=process)
                        process.__exit__ = Mock(return_value=False)
                        return process
                    observer = SimpleNamespace(observe=Mock(return_value=dict(pair_loaded=loaded, errors=[])))
                    process_api = SimpleNamespace(current=lambda: {}, popen=popen,
                                                  wait_receipt=lambda *a: dict(exit=0))
                    g = SimpleNamespace(IdentityError=guard.IdentityError,
                                        support=lambda name: process_api if name == 'native_input_windows_process' else observer)
                    with patch.object(runtime, 'stage', return_value=dict(configuration='pinned')):
                        if rejected:
                            with self.assertRaisesRegex(guard.IdentityError, 'VT cell'):
                                runtime.launch(g, record, args)
                        else:
                            self.assertEqual(runtime.launch(g, record, args), 0)


if __name__ == '__main__':
    unittest.main()
