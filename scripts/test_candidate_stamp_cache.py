"""Changed evidence, concurrent publishers and races invalidate a gate proof."""
import importlib.util
import hashlib
from contextlib import nullcontext
import ntpath
import os
from pathlib import Path, PureWindowsPath
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).with_name('candidate_identity.py')


def relative_binary_path(binary):
    # The cache rejects relative inputs before resolving them against the cwd.
    return binary.relative_to(binary.parent)


class StampCache(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        self.guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.guard)
        self.cache = self.guard.support('candidate_identity_cache')

    def test_relative_binary_fixture_across_windows_drives(self):
        windows_relpath = ntpath.relpath
        for cwd, location in (('D:/checkout', 'C:/Temp/gate/binary'),
                              ('C:/checkout', 'D:/Temp/gate/binary'),
                              ('C:/Temp/gate', 'C:/Temp/gate/binary')):
            with self.subTest(cwd=cwd, binary=location):
                binary = PureWindowsPath(location)
                with patch.object(os.path, 'relpath', side_effect=lambda path: windows_relpath(path, start=cwd)):
                    relative = PureWindowsPath(relative_binary_path(binary))
                self.assertEqual(relative, PureWindowsPath('binary'))
                self.assertFalse(relative.is_absolute())
                self.assertEqual(binary.parent / relative, binary)

    def test_dependencies_context_and_each_stamp_component(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            target = root / 'gate'
            target.mkdir()
            binary, dependency = target / 'binary', target / 'dependency'
            binary.write_bytes(b'binary')
            dependency.write_bytes(b'linked')
            calls = []

            def calculate():
                calls.append(None)
                return {'bytes': self.cache.read_bytes(self.guard, binary).decode(),
                        'dependency': self.cache.read_bytes(self.guard, dependency).decode()}

            def verify(head='a' * 40, directory=target):
                return self.cache.cached(self.guard, 'built', binary, root, head, directory, calculate)

            first = verify()
            self.assertEqual(verify(), first)
            self.assertEqual(len(calls), 1)
            with self.assertRaises(self.guard.IdentityError):
                self.cache.cached(self.guard, 'built', Path(relative_binary_path(binary)),
                                  root, 'a' * 40, target, calculate)
            original_stamp = self.guard.stamp
            windows = self.guard.support('candidate_identity_windows')
            native_change_time = windows.change_time
            for index in range(6):
                def changed(info):
                    values = list(original_stamp(info))
                    values[index] += 1
                    return tuple(values)
                native_time = (patch.object(windows, 'change_time', side_effect=lambda p: native_change_time(p) + 1)
                               if sys.platform == 'win32' and index == 5 else nullcontext())
                with self.subTest(component=index), patch.object(self.guard, 'stamp', changed), native_time:
                    count = len(calls)
                    verify()
                    self.assertEqual(len(calls), count + 1)
                verify()
            saved_mtime = dependency.stat().st_mtime_ns
            dependency.write_bytes(b'edited')
            os.utime(dependency, ns=(dependency.stat().st_atime_ns, saved_mtime))
            self.assertEqual(verify()['dependency'], 'edited')
            count = len(calls)
            verify('b' * 40)
            another = root / 'another-gate'
            another.mkdir()
            verify(directory=another)
            self.assertEqual(len(calls), count + 2)
            # A redirected alias with identical bytes still needs a fresh proof.
            replacement = target / 'replacement'
            replacement.write_bytes(binary.read_bytes())
            binary.unlink()
            binary.symlink_to(replacement)
            count = len(calls)
            verify()
            self.assertEqual(len(calls), count + 1)

    def test_changed_evidence_during_read_or_before_publication_is_refused(self):
        for phase in ('read', 'publish'):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                binary = root / 'binary'
                binary.write_bytes(b'before')
                read = Path.read_bytes

                def mutate(path):
                    value = read(path)
                    if path == binary:
                        path.write_bytes(b'after!')
                    return value

                def calculate():
                    if phase == 'read':
                        with patch.object(Path, 'read_bytes', mutate):
                            self.cache.read_bytes(self.guard, binary)
                    else:
                        self.cache.read_bytes(self.guard, binary)
                        binary.write_bytes(b'after!')
                    return {}

                with self.assertRaises(self.guard.IdentityError):
                    self.cache.cached(self.guard, 'built', binary, root, 'a' * 40, root, calculate)
                self.assertEqual(list((root / 'identity-cache').glob('*.json')), [])

    def test_concurrent_cold_publishers_leave_a_complete_reusable_proof(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            program = '''
import importlib.util, sys
from pathlib import Path
s = importlib.util.spec_from_file_location('guard', sys.argv[1])
g = importlib.util.module_from_spec(s)
s.loader.exec_module(g)
c = g.support('candidate_identity_cache')
r = Path(sys.argv[2])
result = c.cached(g, 'built', r / 'binary', r, 'a'*40, r,
                  lambda: {'bytes': c.read_bytes(g, r / 'binary').decode()})
assert result == {'bytes': 'qualified'}
'''
            children = [subprocess.Popen([sys.executable, '-c', program, str(SCRIPT.resolve()), str(root)],
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(4)]
            for child in children:
                _, error = child.communicate(timeout=30)
                self.assertEqual(child.returncode, 0, error)
            self.assertEqual(len(list((root / 'identity-cache').glob('*.json'))), 1)
            result = self.cache.cached(self.guard, 'built', binary, root, 'a' * 40, root,
                                       lambda: self.fail('concurrent publication was not reusable'))
            self.assertEqual(result, {'bytes': 'qualified'})

    def test_winerror5_publish_race_reuses_complete_proof(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            real_replace = os.replace

            def publish_then_refuse(temporary, cache):
                real_replace(temporary, cache)
                error = PermissionError(13, 'Access is denied', str(cache))
                error.winerror = 5
                raise error

            with patch.object(self.cache.os, 'replace', side_effect=publish_then_refuse):
                result = self.cache.cached(
                    self.guard, 'built', binary, root, 'a' * 40, root,
                    lambda: {'bytes': self.cache.read_bytes(self.guard, binary).decode()})
            self.assertEqual(result, {'bytes': 'qualified'})
            self.assertEqual(len(list((root / 'identity-cache').glob('*.json'))), 1)
            self.assertEqual(len(list((root / 'identity-cache').glob('*.marshal'))), 1)

    def test_winerror5_marshal_publish_race_reuses_complete_proof(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            real_replace = os.replace

            def publish_then_refuse_marshal(temporary, cache):
                real_replace(temporary, cache)
                if str(cache).endswith('.marshal'):
                    error = PermissionError(13, 'Access is denied', str(cache))
                    error.winerror = 5
                    raise error

            with patch.object(self.cache.os, 'replace', side_effect=publish_then_refuse_marshal):
                result = self.cache.cached(
                    self.guard, 'built', binary, root, 'a' * 40, root,
                    lambda: {'bytes': self.cache.read_bytes(self.guard, binary).decode()})
            self.assertEqual(result, {'bytes': 'qualified'})
            self.assertEqual(len(list((root / 'identity-cache').glob('*.json'))), 1)
            self.assertEqual(len(list((root / 'identity-cache').glob('*.marshal'))), 1)

    def test_winerror5_behind_an_uncommitted_publisher_returns_the_verified_record(self):
        # CI r8 on Windows: a reader held the JSON proof of a publisher that had
        # not yet committed, so a second publisher's replace got WinError 5 and
        # found no reusable proof. Pause the first publisher there, then run the
        # second one in its own process against that held JSON.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            binary = root / 'binary'
            binary.write_bytes(b'qualified')
            program = '''
import importlib.util, os, sys
from pathlib import Path
s = importlib.util.spec_from_file_location('guard', sys.argv[1])
g = importlib.util.module_from_spec(s)
s.loader.exec_module(g)
c = g.support('candidate_identity_cache')
r, held, replace = Path(sys.argv[2]), os.path.realpath(sys.argv[3]), os.replace
refused = []
def windows_replace(source, destination):
    if os.path.realpath(c.stable_path(destination)) != held:
        return replace(source, destination)
    # Native Windows refuses this replace while the parent holds the file;
    # elsewhere, inject that sharing rule for the same path.
    if os.name != 'nt':
        error = PermissionError(13, 'Access is denied', destination)
        error.winerror = 5
        raise error
    try:
        replace(source, destination)
    except PermissionError as error:
        refused.append(getattr(error, 'winerror', None))
        raise
    refused.append(None)
c.os.replace = windows_replace
result = c.cached(g, 'built', r / 'binary', r, 'a'*40, r,
                  lambda: {'bytes': c.read_bytes(g, r / 'binary').decode()})
assert result == {'bytes': 'qualified'}, result
assert os.name != 'nt' or refused == [5], refused
'''
            real_replace = os.replace
            second = []

            def pause_before_marshal(source, destination):
                if str(destination).endswith('.marshal') and not second:
                    published = Path(self.cache.stable_path(destination)).with_suffix('.json')
                    self.assertEqual(len(list(published.parent.glob('*.pending'))), 1)
                    with open(published, 'rb'):
                        second.append(subprocess.run(
                            [sys.executable, '-c', program, str(SCRIPT.resolve()), str(root), str(published)],
                            capture_output=True, timeout=60))
                real_replace(source, destination)

            with patch.object(self.cache.os, 'replace', side_effect=pause_before_marshal):
                first = self.cache.cached(
                    self.guard, 'built', binary, root, 'a' * 40, root,
                    lambda: {'bytes': self.cache.read_bytes(self.guard, binary).decode()})
            self.assertEqual(first, {'bytes': 'qualified'})
            self.assertEqual(len(second), 1)
            self.assertEqual(second[0].returncode, 0, second[0].stderr)
            directory = root / 'identity-cache'
            self.assertEqual(len(list(directory.glob('*.json'))), 1)
            self.assertEqual(len(list(directory.glob('*.marshal'))), 1)
            self.assertEqual(sorted(path.name for path in directory.glob('.*')), [])
            result = self.cache.cached(self.guard, 'built', binary, root, 'a' * 40, root,
                                       lambda: self.fail('the paused publication was not reusable'))
            self.assertEqual(result, {'bytes': 'qualified'})

    def test_home_projects_neighbor_dates_do_not_rehash_unchanged_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary).resolve() / 'HOME'
            projects = home / 'Projects'
            source = projects / 'candidate'
            target = source / 'gate'
            target.mkdir(parents=True)
            binary = target / 'binary'
            binary.write_bytes(b'qualified')
            calls = []

            def calculate():
                calls.append(None)
                return {'bytes': self.cache.read_bytes(self.guard, binary).decode()}

            first = self.cache.cached(self.guard, 'built', binary, source, 'a' * 40, target, calculate)
            (home / 'neighbor').write_text('unrelated')
            (projects / 'sibling-candidate').mkdir()
            second = self.cache.cached(
                self.guard, 'built', binary, source, 'a' * 40, target,
                lambda: self.fail('unchanged graph/stamps were rehashed after neighboring dates changed'))
            self.assertEqual(second, first)
            self.assertEqual(len(calls), 1)

    def test_generated_compiler_input_alias_cannot_redirect_a_cached_proof(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / 'source'
            source.mkdir()
            code = source / 'main.rs'
            code.write_bytes(b'fn main() {}')
            (source / '.gitignore').write_text('.rmux-audit/\n')
            subprocess.run(['git', '-C', str(source), 'init', '-q'], check=True)
            subprocess.run(['git', '-C', str(source), 'add', '.'], check=True)
            subprocess.run(['git', '-C', str(source), '-c', 'user.name=Identity Test',
                            '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
            g = self.guard
            head = g.git(source, 'rev-parse', 'HEAD')
            target = g.target_directory(source, head, '0' * 32)
            target.mkdir(parents=True)
            binary = target / 'binary'
            source_id = hashlib.sha256(os.fsencode(str(source))).hexdigest()
            binary.write_bytes(f'RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:{g.host_os()}:END'.encode())
            generated = target / 'generated.rs'
            original = target / 'original.rs'
            original.write_bytes(code.read_bytes())
            generated.symlink_to(original)
            compiler = root / 'compiler'
            compiler.write_bytes(b'compiler fixture')
            binding = g.verify(binary, source, head, target)
            binding.update(kind='bin', exit=0, input=str(generated), input_sha256=g.file_hash(generated),
                           compiler=str(compiler), compiler_sha256=g.file_hash(compiler),
                           generation=dict(origin=str(code), origin_sha256=g.file_hash(code),
                                           generator=str(compiler), generator_sha256=g.file_hash(compiler)))
            g.support('candidate_identity_retention').publish(g, target, binding)
            # Isolate the cache/input-path boundary; real compiler graphs and
            # executable consumers are covered by the frozen native acceptance.
            units = g.support('candidate_identity_units')
            with patch.object(units, 'verify', return_value={'sha256': binding['sha256']}):
                g.verify_built(binary, source, head, target)
                identical_foreign_code = root / 'foreign.rs'
                identical_foreign_code.write_bytes(generated.read_bytes())
                generated.unlink()
                generated.symlink_to(identical_foreign_code)
                with self.assertRaises(g.IdentityError):
                    g.verify_built(binary, source, head, target)

    def test_windows_uses_change_time_and_refuses_missing_native_metadata(self):
        windows = self.guard.support('candidate_identity_windows')
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve() / 'binary'
            path.write_bytes(b'qualified')
            baseline = list(self.guard.stamp(path.stat()))
            with patch.object(self.cache, 'sys', SimpleNamespace(platform='win32')), \
                    patch.object(windows, 'change_time', return_value=123456789):
                actual = self.cache.snapshot(self.guard, path)['stamp']
                self.assertEqual(actual, baseline[:-1] + [123456789])
            def information(handle, kind, pointer, size):
                self.assertEqual(kind, 0)
                self.assertEqual(size, 40)
                pointer._obj.ChangeTime = 1234
                return 1
            api = Mock(side_effect=information)
            kernel = SimpleNamespace(GetFileInformationByHandleEx=api)
            with patch.dict(sys.modules, msvcrt=SimpleNamespace(get_osfhandle=lambda fd: fd)), \
                    patch.object(windows.ctypes, 'WinDLL', return_value=kernel, create=True), \
                    patch.object(windows, '_file_info_api', None):
                self.assertEqual(windows.change_time(path), 123400)
                self.assertEqual(windows.change_time(path), 123400)
                api.side_effect = None
                api.return_value = 0
                with self.assertRaises(OSError):
                    windows.change_time(path)


if __name__ == '__main__':
    unittest.main()
