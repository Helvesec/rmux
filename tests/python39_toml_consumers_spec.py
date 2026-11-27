"""Bounded consumer parsing checks; no builds, archives or performance runs."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / 'scripts'
if '--without-tomllib' in sys.argv:
    sys.argv.remove('--without-tomllib')
    sys.modules['tomllib'] = None

sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(SCRIPTS / 'release'))
sys.path.insert(0, str(SCRIPTS / 'source-inventory'))
import installed_consumer_packages as packages
import inventory
from toml_reader import TOMLDecodeError

spec = importlib.util.spec_from_file_location('perf_cutoff', SCRIPTS / 'bench/check-perf-cutoff.py')
perf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perf)

MANIFEST = '''# Cargo manifest subset consumed by all three tools
[package]
name = "sample"
version = "1.0.0"
edition.workspace = true
build = false
[features]
default = [
  "dep:helper", # optional dependency
]
extra = []
[dependencies]
helper = { version = "1.2", optional = true }
[target.'cfg(windows)'.dependencies]
platform = "2"
[lib]
path = "lib.rs"
[[bin]]
name = "sample-cli"
path = "cli.rs"
[[test]]
name = "contract"
path = "contract.rs"
'''
EXPECTED = {
    'package': {'name': 'sample', 'version': '1.0.0',
                'edition': {'workspace': True}, 'build': False},
    'features': {'default': ['dep:helper'], 'extra': []},
    'dependencies': {'helper': {'version': '1.2', 'optional': True}},
    'target': {'cfg(windows)': {'dependencies': {'platform': '2'}}},
    'lib': {'path': 'lib.rs'},
    'bin': [{'name': 'sample-cli', 'path': 'cli.rs'}],
    'test': [{'name': 'contract', 'path': 'contract.rs'}],
}


class TomlConsumersSpec(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix='rmux-toml-consumers-')
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.manifest = self.root / 'Cargo.toml'
        self.manifest.write_text(MANIFEST, encoding='utf-8')
        server = self.root / 'crates/rmux-server/Cargo.toml'
        server.parent.mkdir(parents=True)
        server.write_text(MANIFEST, encoding='utf-8')

    def targets(self, document):
        files = {'Cargo.toml': {'data': document.encode('utf-8')}}
        files.update({name: {'data': b''} for name in ('lib.rs', 'cli.rs', 'contract.rs')})
        return inventory.targets(files, {'crate_roles': {}, 'rust_roots': {}})

    def test_package_manifest_shape(self):
        self.assertEqual(packages.read_toml(self.manifest), EXPECTED)

    def test_normalized_memchr_description(self):
        fixture = ROOT / 'tests/fixtures/sdk-policy/normalized-memchr.toml'
        self.assertEqual(packages.read_toml(fixture), {'package': {
            'name': 'memchr', 'version': '2.8.2',
            'description':
                'Provides extremely fast (uses SIMD on x86_64, aarch64 and wasm32) routines for\n'
                '1, 2 or 3 byte search and single substring search.\n'}})

    def test_multiline_basic_lexical_values_in_all_consumers(self):
        cases = [
            ('""""""', ''),
            ('["""first\nsecond""", "single"]', ['first\nsecond', 'single']),
            ('{ text = """first\nsecond""" }', {'text': 'first\nsecond'}),
            ('"""\n\n  text\n"""', '\n  text\n'),
            ('"""\r\ntext\r\nnext"""', 'text\nnext'),
            ('"""one "two" and ""three"" # literal"""', 'one "two" and ""three"" # literal'),
            ('"""text""""', 'text"'),
            ('"""text"""""', 'text""'),
            ('"""a\\  \n \t\r\n b"""', 'ab'),
            ('"""a\\\r\nb"""', 'ab'),
            (r'"""\t\b\n\r\f\u0041\U0001F980\"\\"""', '\t\b\n\r\fA🦀"\\'),
        ]
        rejected = subprocess.CompletedProcess([], 1, b'',
            b'does not contain this feature: perf-instrument')
        for value, expected in cases:
            with self.subTest(value=value):
                document = MANIFEST.replace('build = false', 'build = false\ndescription = ' + value)
                self.manifest.write_bytes(document.encode())
                parsed = packages.read_toml(self.manifest)
                self.assertEqual(parsed, {**EXPECTED, 'package': {
                    **EXPECTED['package'], 'description': expected}})
                self.assertEqual(self.targets(document), self.targets(MANIFEST))
                with patch.object(perf, 'ROOT', self.root), patch.object(
                        perf, 'run', return_value=rejected):
                    self.assertEqual(len(perf.check_features('unused-cargo')), 2)

    def test_multiline_errors_in_all_consumers(self):
        values = [
            '"""unfinished', '"""two""', '"""six""""""',
            r'"""bad\q"""', r'"""bad\u12xz"""',
            r'"""bad\uD800"""', r'"""bad\U00110000"""',
            '"""bad\\   x"""', '"""bad\\\n',
            '"""bad\x00"""', '"""bad\x7f"""',
        ]
        documents = ['description = ' + value for value in values]
        documents += ['"""key""" = 1', '["""table"""]', 'a = "one\ntwo"',
                      'a = """ok""" trailing', 'a = """ok"""\na = 1']
        for document in documents:
            with self.subTest(document=document):
                self.manifest.write_text(document)
                with self.assertRaisesRegex(TOMLDecodeError, r'line \d+:'):
                    packages.read_toml(self.manifest)
                with self.assertRaises(TOMLDecodeError):
                    self.targets(document)
                with patch.object(perf, 'ROOT', self.root), patch.object(perf, 'run') as run:
                    with self.assertRaises(TOMLDecodeError):
                        perf.check_features('unused-cargo')
                    run.assert_not_called()

    def test_file_newlines_in_all_consumers(self):
        rejected = subprocess.CompletedProcess([], 1, b'',
            b'does not contain this feature: perf-instrument')
        server = self.root / 'crates/rmux-server/Cargo.toml'
        cases = [
            ('bare-cr', 'first\rsecond', None),
            ('backslash-cr', 'first\\\rsecond', None),
            ('lf', 'first\nsecond', 'first\nsecond'),
            ('crlf', 'first\r\nsecond', 'first\nsecond'),
            ('backslash-lf', 'first\\\nsecond', 'firstsecond'),
            ('backslash-crlf', 'first\\\r\nsecond', 'firstsecond'),
        ]
        for label, value, expected in cases:
            document = MANIFEST.replace('build = false',
                'build = false\ndescription = """' + value + '"""')
            for path in (self.manifest, server):
                with self.subTest(case=label, path=path.relative_to(self.root)):
                    for manifest in (self.manifest, server):
                        manifest.write_bytes(MANIFEST.encode('utf-8'))
                    path.write_bytes(document.encode('utf-8'))
                    raw = path.read_bytes().decode('utf-8')
                    if expected is None:
                        with self.subTest(adapter='packages'):
                            with self.assertRaises(TOMLDecodeError):
                                packages.read_toml(path)
                        with self.subTest(adapter='inventory'):
                            with self.assertRaises(TOMLDecodeError):
                                self.targets(raw)
                        with self.subTest(adapter='perf'), patch.object(
                                perf, 'ROOT', self.root), patch.object(
                                perf, 'run', return_value=rejected) as run:
                            with self.assertRaises(TOMLDecodeError):
                                perf.check_features('unused-cargo')
                            self.assertEqual(run.call_count, int(path == server))
                    else:
                        parsed = {**EXPECTED, 'package': {
                            **EXPECTED['package'], 'description': expected}}
                        self.assertEqual(packages.read_toml(path), parsed)
                        self.assertEqual(self.targets(raw), self.targets(MANIFEST))
                        seen = []

                        def parse(text):
                            result = packages.loads(text)
                            seen.append(result)
                            return result

                        with patch.object(perf, 'ROOT', self.root), patch.object(
                                perf, 'run', return_value=rejected) as run, patch.object(
                                perf.toml_reader, 'loads', side_effect=parse):
                            self.assertEqual(perf.check_features('unused-cargo'), {
                                name: rejected.stderr.decode()
                                for name in ('rmux', 'rmux-server')})
                            self.assertEqual(run.call_count, 2)
                        self.assertEqual(seen, [parsed, EXPECTED] if path == self.manifest
                                         else [EXPECTED, parsed])

    def test_checked_in_consumer_inputs(self):
        for relative in ('Cargo.toml', 'Cargo.lock', 'crates/rmux-server/Cargo.toml',
                         'tests/fixtures/sdk-policy/normalized-web-crypto.toml'):
            with self.subTest(path=relative):
                parsed = packages.read_toml(ROOT / relative)
                self.assertIsInstance(parsed['package'],
                                      list if relative.endswith('.lock') else dict)
                # Optional reference comparison; blocked/native 3.9 runs use
                # the explicit expected shapes in the other tests.
                if sys.version_info >= (3, 11) and sys.modules.get('tomllib', True) is not None:
                    import tomllib
                    self.assertEqual(parsed, tomllib.loads((ROOT / relative).read_text()))

    def test_lock_identities_and_deliberate_resolution_failure(self):
        lock = self.root / 'Cargo.lock'
        lock.write_text('''version = 4
[[package]]
name = "sample"
version = "1.0.0"
[[package]]
name = "helper"
version = "1.2.0"
source = "registry+example"
checksum = "abc"
dependencies = ["sample"]
''', encoding='utf-8')
        self.assertEqual(packages.identities(lock), {
            ('sample', '1.0.0', None, None),
            ('helper', '1.2.0', 'registry+example', 'abc')})
        packages.check_lock(lock, lock, [])
        changed = self.root / 'changed.lock'
        for before, after in (('"abc"', '"def"'), ('"helper"', '"other"'),
                              ('"1.2.0"', '"1.2.1"')):
            with self.subTest(changed_identity=before):
                changed.write_text(lock.read_text().replace(before, after))
                with self.assertRaisesRegex(ValueError, 'resolution changed a locked identity'):
                    packages.check_lock(changed, lock, [])

    def test_inventory_targets_and_metadata_shape(self):
        roots, manifests = self.targets(MANIFEST)
        self.assertEqual(roots, [('cli.rs', 'production', True, 'Cargo.toml'),
                                 ('contract.rs', 'tests', True, 'Cargo.toml'),
                                 ('lib.rs', 'production', True, 'Cargo.toml')])
        self.assertEqual(manifests, {'.': {
            'name': 'sample', 'manifest': 'Cargo.toml',
            'dependencies': {key: EXPECTED[key] for key in ('dependencies', 'target')},
            'features': EXPECTED['features']}})
        with self.assertRaisesRegex(ValueError, 'missing declared Cargo target'):
            self.targets(MANIFEST.replace('lib.rs', 'missing.rs'))

    def test_perf_feature_parse_only(self):
        rejected = subprocess.CompletedProcess([], 1, b'',
            b'does not contain this feature: perf-instrument')
        with patch.object(perf, 'ROOT', self.root), patch.object(perf, 'run', return_value=rejected) as run:
            self.assertEqual(perf.check_features('unused-cargo'), {
                name: rejected.stderr.decode() for name in ('rmux', 'rmux-server')})
            self.assertEqual(run.call_count, 2)
            for features in ('perf-instrument = []', 'forward = ["helper/perf-instrument"]'):
                self.manifest.write_text('[features]\n' + features + '\n')
                run.reset_mock()
                with self.assertRaisesRegex(RuntimeError, 'still (publishes|forwards)'):
                    perf.check_features('unused-cargo')
                run.assert_not_called()

    def test_invalid_toml_rejected_by_every_consumer(self):
        for document in ('[features]\na=[]\na=[]\n', '[features]\n[features]\n',
                         '[features]\na=["unterminated]\n',
                         '[features]\na={b=1,}\n'):
            with self.subTest(document=document):
                self.manifest.write_text(document)
                with self.assertRaises(ValueError):
                    packages.read_toml(self.manifest)
                with self.assertRaises(ValueError):
                    self.targets(document)
                with patch.object(perf, 'ROOT', self.root), patch.object(perf, 'run') as run:
                    with self.assertRaises(ValueError):
                        perf.check_features('unused-cargo')
                    run.assert_not_called()

    def test_shared_reader_unsupported_constructs_stay_loud(self):
        for value in ('1979-05-27', '07:32:00'):
            document = '[features]\nunsupported=' + value + '\n'
            self.manifest.write_text(document)
            with self.assertRaises(TOMLDecodeError):
                packages.read_toml(self.manifest)
            with self.assertRaises(TOMLDecodeError):
                self.targets(document)
            with patch.object(perf, 'ROOT', self.root), patch.object(perf, 'run') as run:
                with self.assertRaises(TOMLDecodeError):
                    perf.check_features('unused-cargo')
                run.assert_not_called()

    def test_nested_script_imports_without_preloaded_shared_reader(self):
        for relative in ('bench/check-perf-cutoff.py', 'source-inventory/inventory.py'):
            path = SCRIPTS / relative
            code = '''import runpy, sys
sys.modules['tomllib'] = None
sys.path.insert(0, sys.argv[1])
runpy.run_path(sys.argv[2], run_name='consumer_import_test')
'''
            result = subprocess.run([sys.executable, '-I', '-c', code,
                                     str(path.parent), str(path)],
                                    cwd=self.root, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
