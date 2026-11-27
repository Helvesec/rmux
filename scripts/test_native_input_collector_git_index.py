"""A native command's identity preparation must preserve its retained Git index."""
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import native_input_windows_command as recipe


class BeforeCargo(Exception):
    pass


class RetainedGitIndex(unittest.TestCase):
    def test_identity_preparation_cannot_refresh_the_before_use_index(self):
        audit = Path(__file__).resolve().parent.parent / '.rmux-audit' / 'git-index-046'
        audit.mkdir(parents=True, exist_ok=True)
        spec = importlib.util.spec_from_file_location(
            'index_guard', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        with tempfile.TemporaryDirectory(dir=audit) as temporary:
            source = Path(temporary).resolve()
            tracked = source / 'tracked'
            tracked.write_text('unchanged candidate bytes\n')
            git = ['git', '-C', str(source)]
            subprocess.run(git + ['init', '-q'], check=True)
            subprocess.run(git + ['add', '.'], check=True)
            subprocess.run(git + ['-c', 'user.name=Identity Test', '-c',
                                 'user.email=identity@example.invalid', 'commit',
                                 '-qm', 'fixture'], check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            index = source / '.git' / 'index'
            before = index.read_bytes(), guard.stamp(index.stat())
            # Same source bytes, a different cached stat: ordinary status refreshes
            # the index; read-only identity inspection must leave it untouched.
            stat = tracked.stat()
            os.utime(tracked, ns=(stat.st_atime_ns, stat.st_mtime_ns + 2_000_000_000))
            budget = source / 'budget.json'
            budget.write_text(json.dumps(dict(cell='sdk', head=head)))
            record = dict(cell='sdk', head=head, source=str(source),
                          audit=str(source), case='frozen-index', files={str(index): {}})
            options = SimpleNamespace(source=source, budget=budget,
                                      toolchain=source, cargo_home=source)
            original_run = subprocess.run

            def prepare():
                # This is the actual Git call made by candidate source_identity.
                self.assertEqual(guard.git(source, 'status', '--porcelain', '--', 'tracked'), '')
                raise BeforeCargo

            def external(args, **kwargs):
                if Path(args[0]).name.lower() == 'logman.exe':
                    return subprocess.CompletedProcess(args, 0)
                return original_run(args, **kwargs)

            def device(_drive, buffer, _length):
                buffer.value = 'native-test-device'
                return 1

            api = SimpleNamespace(QueryDosDeviceW=device)
            loader = SimpleNamespace(exec_module=lambda _module: None)
            previous = Path.cwd()
            try:
                with patch.dict(os.environ, dict(SystemRoot=str(source)), clear=False), \
                        patch.object(recipe, 'capacity'), patch.object(recipe, 'install'), \
                        patch.object(recipe, 'read_manifest', return_value=({}, 'digest')), \
                        patch.object(ctypes, 'WinDLL', return_value=api, create=True), \
                        patch.object(recipe.subprocess, 'run', side_effect=external), \
                        patch.object(recipe.importlib.util, 'spec_from_file_location',
                                     return_value=SimpleNamespace(name='candidate_identity', loader=loader)), \
                        patch.object(recipe.importlib.util, 'module_from_spec',
                                     return_value=SimpleNamespace(prepare=prepare)):
                    os.environ.pop('GIT_OPTIONAL_LOCKS', None)
                    with self.assertRaises(BeforeCargo):
                        recipe.run(record, options)
            finally:
                os.chdir(previous)
            self.assertEqual((index.read_bytes(), guard.stamp(index.stat())), before,
                             'identity preparation refreshed the retained Git index')


if __name__ == '__main__':
    unittest.main()
