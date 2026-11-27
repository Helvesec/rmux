"""Declared synthetic images only: no Cargo, build, SBOM test or binary execution."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class CollectorTests(unittest.TestCase):
    def setUp(self):
        audit = ROOT / '.rmux-audit/d144-unit'
        audit.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        spec = importlib.util.spec_from_file_location(
            'collector_under_test', ROOT / 'scripts/release/d144_collector.py')
        self.collector = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.collector)
        c = self.collector
        c.WINDOWS = True  # Replay Windows audit events on the portable fixture too.
        c.ROOT = self.root
        c.AUDIT = self.root / '.rmux-audit/d144'
        c.AUDIT.mkdir(parents=True)
        c.NATURAL = self.root / '.rmux-audit/local-sbom-tests'
        c.PARENT = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        c.GIT = shutil.which('git')
        self.source = c.NATURAL / 'fixture with spaces'
        self.target = self.source / 'target' / ('a' * 40) / 'win32/debug'
        self.target.mkdir(parents=True)
        (self.source / 'Cargo.toml').write_text('# qualification only; never sent to Cargo\n')
        for args in (['init', '-q'], ['add', 'Cargo.toml'],
                     ['-c', 'user.name=Sidney Sissaoui', '-c', 'user.email=shideneyu@gmail.com',
                      'commit', '-qm', 'Declared synthetic D144 fixture']):
            subprocess.run([c.GIT, *args], cwd=self.source, check=True, capture_output=True)
        # Git marks loose objects read-only on Windows. This synthetic fixture
        # uses direct rmtree in its child; prepare its own files for that cleanup.
        if os.name == 'nt':
            for file in (self.source / '.git/objects').rglob('*'):
                if file.is_file():
                    file.chmod(stat.S_IREAD | stat.S_IWRITE)
        self.head = subprocess.check_output([c.GIT, 'rev-parse', 'HEAD'], cwd=self.source, text=True).strip()

    def event(self, role, serialized=True):
        argv = [r'C:\Program Files\Rust\cargo.exe', 'build', '--locked', '--offline',
                '--target-dir', str(self.source / 'target'), '--bin', 'rmux']
        if role == 'cli':
            argv += ['--features', 'tiny-cli']
        raw = subprocess.list2cmdline(argv) if serialized else argv
        self.collector.observe('subprocess.Popen', (None, raw, str(self.source), None))

    def completed(self, role):
        self.image = self.target / 'rmux.exe'
        payload = b'MZ-DECLARED-SYNTHETIC-ONLY-' + role.encode()
        self.image.write_bytes(payload)
        (self.target / 'rmux.d').write_text(str(self.image) + ': src/main.rs\n')
        fingerprint = self.target / '.fingerprint/rmux-0123456789abcdef'
        fingerprint.mkdir(parents=True, exist_ok=True)
        (fingerprint / 'bin-rmux.json').write_text(json.dumps({'features': [role]}))
        rows = [dict(reason='compiler-artifact', target=dict(name='rmux', kind=['bin']),
                     executable=str(self.image), filenames=[str(self.image)],
                     manifest_path=str(self.source / 'Cargo.toml')),
                dict(reason='build-finished', success=True)]
        (self.source / (role + '.jsonl')).write_text(''.join(json.dumps(r) + '\n' for r in rows))
        return hashlib.sha256(payload).hexdigest()

    def check_overwrite(self, serialized):
        self.event('helper', serialized)
        helper = self.completed('helper')
        helper_mtime = self.image.stat().st_mtime_ns
        self.event('cli', serialized)
        cli = self.completed('cli')
        self.collector.observe('subprocess.Popen', ('git', ['git', 'status'], str(self.source), None))
        snapshots = self.collector.AUDIT / 'build-snapshots.jsonl'
        self.assertTrue(snapshots.is_file(), 'Windows Popen string builds were silently ignored')
        rows = [json.loads(line) for line in snapshots.read_text().splitlines()]
        self.assertEqual(len(rows), 2)
        self.assertEqual([row['images'][0]['sha256'] for row in rows], [helper, cli])
        self.assertEqual(rows[0]['images'][0]['path'], rows[1]['images'][0]['path'])
        for row in rows:
            item = row['images'][0]
            self.assertTrue(row['complete'], row['issues'])
            self.assertEqual(row['source_git']['head'], self.head)
            self.assertEqual(row['build']['parent_sha'], self.collector.PARENT)
            self.assertEqual(item['mtime_ns'], item['mtime_after_hash_ns'])
            self.assertGreaterEqual(item['nlink'], 1)
            self.assertTrue(any(u['path'].endswith('.d') for u in row['units']))
            self.assertTrue(any('.fingerprint' in u['path'] for u in row['units']))
            origin = item['origins'][0]
            self.assertEqual(origin['artifact']['target']['name'], 'rmux')
            original_row = Path(origin['log']['retained']).read_text().splitlines()[origin['line'] - 1]
            self.assertEqual(json.loads(original_row), origin['artifact'])
            self.assertEqual(hashlib.sha256(Path(item['retained']).read_bytes()).hexdigest(), item['sha256'])
        self.assertEqual(rows[0]['images'][0]['mtime_ns'], helper_mtime)
        self.assertFalse((self.collector.AUDIT / 'observer-errors.jsonl').exists())

    def test_windows_string_keeps_helper_before_cli_replaces_same_path(self):
        self.check_overwrite(True)

    def test_list_keeps_each_build_and_raw_argv(self):
        self.check_overwrite(False)
        rows = [json.loads(s) for s in (self.collector.AUDIT / 'subprocess.jsonl').read_text().splitlines()]
        self.assertIsInstance(rows[0]['argv'], list)
        self.assertEqual(rows[0]['argv'], rows[0]['argv_tokens'])

    def test_windows_spaces_quotes_empty_and_trailing_slashes(self):
        cases = ['', 'simple', 'with spaces', 'a"b', 'with\ttab', 'C:\\plain\\',
                 'C:\\space dir\\', '\\', '\\\\', '\\\"', 'unicode-é-雪']
        for first in cases:
            for second in cases:
                argv = [r'C:\Program Files\Python\python.exe', first, second, 'next']
                with self.subTest(argv=argv):
                    self.assertEqual(self.collector.windows_argv(subprocess.list2cmdline(argv)), argv)

    @unittest.skipUnless(os.name == 'nt', 'Windows native argv authority required')
    def test_windows_parser_matches_native_api(self):
        from test_native_input_collector_windows_argv import windows_argv
        for argv in ([r'C:\Program Files\Rust\cargo.EXE', 'build', '--features', 'a"b'],
                     [r'C:\Python\python.exe', '', 'C:\\plain\\', 'C:\\space dir\\', 'last'],
                     [r'C:\Python\python.exe', 'a\\\"b', 'one\ttwo', 'unicode-é-雪']):
            command = subprocess.list2cmdline(argv)
            self.assertEqual(self.collector.windows_argv(command), windows_argv(command))

    def test_failed_build_is_retained_and_records_error(self):
        self.event('helper')
        digest = self.completed('helper')
        (self.source / 'helper.jsonl').write_text('{"reason":"build-finished","success":false}\n')
        self.event('cli')
        self.assertTrue((self.collector.AUDIT / 'observer-errors.jsonl').is_file())
        row = json.loads((self.collector.AUDIT / 'build-snapshots.jsonl').read_text())
        self.assertFalse(row['complete'])
        self.assertEqual(row['images'][0]['sha256'], digest)
        self.assertTrue(Path(row['images'][0]['retained']).is_file())
        self.assertIsNotNone(self.collector.LAST_BUILD, 'next event must still be observed after error')

    def test_missing_log_or_unit_never_counts_as_complete(self):
        for missing in ('helper.jsonl', 'target'):
            with self.subTest(missing=missing):
                self.collector.LAST_BUILD = None
                self.event('helper')
                self.completed('helper')
                if missing == 'helper.jsonl':
                    (self.source / missing).unlink()
                else:
                    shutil.rmtree(self.target / '.fingerprint')
                    (self.target / 'rmux.d').unlink()
                self.collector.observe('other-event', ())
                self.assertIsNotNone(self.collector.LAST_BUILD)
                self.collector.observe('subprocess.Popen', ('git', ['git', 'status'], str(self.source), None))
                rows = (self.collector.AUDIT / 'build-snapshots.jsonl').read_text().splitlines()
                self.assertFalse(json.loads(rows[-1])['complete'])

    def test_malformed_argv_is_an_error(self):
        self.collector.observe('subprocess.Popen', (None, '"unterminated', str(self.source), None))
        self.assertTrue((self.collector.AUDIT / 'observer-errors.jsonl').is_file())

    def test_changed_head_is_an_error_after_retention(self):
        self.event('helper', False)
        digest = self.completed('helper')
        subprocess.run([self.collector.GIT, '-c', 'user.name=Sidney Sissaoui', '-c',
                        'user.email=shideneyu@gmail.com', 'commit', '--allow-empty', '-qm', 'Changed HEAD'],
                       cwd=self.source, check=True, capture_output=True)
        self.event('cli', False)
        self.assertTrue((self.collector.AUDIT / 'images' / digest).is_file())
        self.assertTrue((self.collector.AUDIT / 'observer-errors.jsonl').is_file())

    def test_cleanup_seals_last_build_and_original_fixture(self):
        self.event('helper')
        self.completed('helper')
        self.collector.observe('shutil.rmtree', (str(self.source), None))
        self.assertIsNone(self.collector.LAST_BUILD)
        self.assertTrue((self.collector.AUDIT / 'fixtures.jsonl').is_file())
        self.assertFalse((self.collector.AUDIT / 'observer-errors.jsonl').exists())

    def test_exit_seals_last_completed_build(self):
        self.event('helper')
        self.completed('helper')
        self.collector.finish()
        row = json.loads((self.collector.AUDIT / 'build-snapshots.jsonl').read_text())
        self.assertTrue(row['complete'])
        self.assertFalse((self.collector.AUDIT / 'observer-errors.jsonl').exists())

    def test_identical_log_rewritten_by_next_build_is_still_a_new_build(self):
        for _ in range(2):
            self.event('helper')
            self.completed('helper')
            self.collector.finish()
        rows = [json.loads(s) for s in (self.collector.AUDIT / 'build-snapshots.jsonl').read_text().splitlines()]
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(row['complete'] for row in rows))
        self.assertFalse((self.collector.AUDIT / 'observer-errors.jsonl').exists())

    def test_each_image_alias_keeps_its_compilation_origin(self):
        self.event('helper')
        helper = self.completed('helper')
        alias = self.target / 'deps/rmux-0123456789abcdef.exe'
        alias.parent.mkdir()
        os.link(self.image, alias)
        self.event('cli')
        # Replace the public path, leaving the helper's hardlink intact.
        self.image.unlink()
        self.completed('cli')
        self.collector.finish()
        rows = [json.loads(s) for s in (self.collector.AUDIT / 'build-snapshots.jsonl').read_text().splitlines()]
        self.assertEqual(len(rows), 2)
        old = next(image for image in rows[-1]['images'] if image['sha256'] == helper)
        self.assertIn('helper.jsonl', old['origins'][0]['log']['path'])
        self.assertEqual(old['origins'][0]['sha256'], helper)
        self.assertFalse((self.collector.AUDIT / 'observer-errors.jsonl').exists())

    @unittest.skipUnless(os.name == 'nt', 'Windows long path read authority required')
    def test_windows_long_paths_are_read_and_retained_with_original_names(self):
        self.event('helper')
        digest = self.completed('helper')
        long = self.target.joinpath(*(['long-segment-1234567890'] * 8), 'rmux.exe')
        self.assertGreater(len(str(long)), 260)
        self.collector.access(long.parent).mkdir(parents=True)
        self.collector.access(long).write_bytes(self.image.read_bytes())
        self.collector.finish()
        row = json.loads((self.collector.AUDIT / 'build-snapshots.jsonl').read_text())
        item = next(image for image in row['images'] if image['path'] == str(long))
        self.assertEqual(item['sha256'], digest)
        self.assertTrue(Path(item['retained']).is_file())
        self.assertTrue(row['complete'])
        # Test fixture cleanup only; no product cleanup path is replaced.
        shutil.rmtree(self.collector.access(self.source))

    def test_new_paths_are_explicitly_classified_and_policy_bound(self):
        policy = json.loads((ROOT / 'scripts/source-inventory/declarations.json').read_bytes())
        contract = json.loads((ROOT / '.github/release/candidate-contract.json').read_bytes())
        for role, path in [('tooling', 'scripts/release/d144_collector.py'),
                           ('tests', 'scripts/test_d144_collector.py')]:
            self.assertIn(path, policy['files'][role])
            self.assertIn(path, contract['policy_paths'])
        self.assertEqual(contract['policy_paths'], sorted(set(contract['policy_paths'])))

    def test_unix_string_is_one_executable_name(self):
        self.collector.WINDOWS = False
        self.collector.observe('subprocess.Popen', (None, 'cargo build', str(self.source), None))
        self.assertIsNone(self.collector.LAST_BUILD)
        row = json.loads((self.collector.AUDIT / 'subprocess.jsonl').read_text())
        self.assertEqual(row['argv_tokens'], ['cargo build'])

    def test_native_popen_sequence_uses_original_waits_and_keeps_each_image(self):
        # Python executes a declared fixture named "build" through an explicit
        # executable. The actual audit event has Cargo-shaped argv; no Cargo runs.
        script = self.source / 'build'
        script.write_text('''import json,sys
from pathlib import Path
role=sys.argv[1]
source=Path.cwd()
target=source/'target'/('a'*40)/'win32/debug'
image=target/'rmux.exe'
image.write_bytes(b'MZ-DECLARED-SYNTHETIC-ONLY-'+role.encode())
(target/'rmux.d').write_text(str(image)+': src/main.rs\\n')
unit=target/'.fingerprint/rmux-0123456789abcdef'
unit.mkdir(parents=True,exist_ok=True)
(unit/'bin-rmux.json').write_text(json.dumps({'features':[role]}))
rows=[dict(reason='compiler-artifact',target=dict(name='rmux',kind=['bin']),
executable=str(image),filenames=[str(image)],manifest_path=str(source/'Cargo.toml')),
dict(reason='build-finished',success=True)]
(source/(role+'.jsonl')).write_text(''.join(json.dumps(r)+'\\n' for r in rows))
''')
        driver = self.root / 'driver.py'
        driver.write_text('''import importlib.util,json,shutil,subprocess,sys
from pathlib import Path
module,root,parent,git,source=sys.argv[1:]
spec=importlib.util.spec_from_file_location('native_collector',module)
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)
# The parent is the actual checked-out repository; fixtures have their own HEAD.
c.ROOT=Path(root);c.AUDIT=Path(root)/'.rmux-audit/d144'
c.NATURAL=Path(root)/'.rmux-audit/local-sbom-tests';c.GIT=git;c.PARENT=parent
sys.addaudithook(c.observe)
waits=[]
for role in ('helper','cli'):
    argv=[str(Path(root)/'Cargo path with spaces/cargo.exe'),'build',role]
    if role=='cli' and sys.platform=='win32':argv=subprocess.list2cmdline(argv)
    child=subprocess.Popen(argv,executable=sys.executable,cwd=source)
    rc=child.wait();assert rc==0
    waits.append(dict(pid=child.pid,rc=rc,original_wait=True,input_type=type(argv).__name__))
shutil.rmtree(source)
assert c.LAST_BUILD is None
assert not (c.AUDIT/'observer-errors.jsonl').exists()
(c.AUDIT/'original-waits.json').write_text(json.dumps(waits,indent=2))
''')
        child = subprocess.Popen([sys.executable, '-B', str(driver),
            str(ROOT / 'scripts/release/d144_collector.py'), str(self.root),
            self.collector.PARENT, self.collector.GIT, str(self.source)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        stdout, stderr = child.communicate()
        self.assertEqual(child.wait(), 0, (stdout, stderr))
        rows = [json.loads(s) for s in (self.collector.AUDIT / 'build-snapshots.jsonl').read_text().splitlines()]
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(row['complete'] for row in rows))
        self.assertEqual([row['images'][0]['sha256'] for row in rows], [
            hashlib.sha256(b'MZ-DECLARED-SYNTHETIC-ONLY-' + role).hexdigest()
            for role in (b'helper', b'cli')])
        self.assertTrue(all(isinstance(row['build']['argv'], str if os.name == 'nt' else list)
                            for row in rows))
        self.assertFalse(self.source.exists())
        proof = ROOT / '.rmux-audit/d144-native-witness' / str(os.getpid())
        proof.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(self.collector.AUDIT, proof)

    def test_outside_fixture_cannot_create_build_snapshot(self):
        self.collector.observe('subprocess.Popen', ('cargo', ['cargo', 'build'], str(self.root), None))
        self.assertIsNone(self.collector.LAST_BUILD)

    @unittest.skipUnless(os.name != 'nt', 'Unix list event authority required')
    def test_unix_native_popen_remains_a_list(self):
        import sys
        events = []
        def hook(event, args):
            if event == 'subprocess.Popen':
                events.append(args)
        sys.addaudithook(hook)
        child = subprocess.Popen([sys.executable, '-c', 'pass'], cwd=self.source)
        self.assertEqual(child.wait(), 0)
        self.assertIsInstance(events[-1][1], list)


if __name__ == '__main__':
    unittest.main()
