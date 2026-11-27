#!/usr/bin/env python3
"""Regression falsifiers for corpus integrity, not product conformance tests."""
import copy
import io
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/release'))
CHECK = runpy.run_path(str(ROOT / 'scripts/check-kitty-conformance.py'))
RELEASE = runpy.run_path(str(ROOT / 'scripts/release/validate-contracts.py'))
from kitty_corpus import encoding, finite_policies, negotiation


def metadata_mutations():
    """RKA-1 reviewer inputs and direct typed-anchor/link siblings only.

    Each tuple edits one payload; an empty operation removes the last path key.
    Kept data-driven so audit reproduction can run these exact inputs via CLI.
    """
    obligations, vectors = 'obligations.jsonl', 'vectors.jsonl'
    yield obligations, 'K-QUICK', ('source',), 'set', None, 'source:'
    yield obligations, 'R-LIFE', ('authority',), 'set', None, 'schema: authority'
    yield obligations, 'R-LIFE', ('decision',), 'set', None, 'obligation selection'
    for file, identity, path in (
        (obligations, 'K-QUICK', ('source',)),
        (vectors, 'K-FUNC-F1-1', ('source',)),
        (vectors, 'K-LEGACY-FUNC-F3-CTRL', ('sources', 0)),
    ):
        for key in ('uri', 'lines', 'sha256'):
            yield file, identity, (*path, key), '', None, 'complete anchor'
        for value in ({}, [], 'anchor', False):
            yield file, identity, path, 'set', value, 'source:'
        for key, value in (('uri', None), ('uri', 1), ('uri', ''),
                           ('lines', None), ('lines', [True, 2]),
                           ('lines', [0, 2]), ('lines', [2, 1]),
                           ('sha256', None), ('sha256', 'bad')):
            yield file, identity, (*path, key), 'set', value, 'source:'
    for value in (None, {}, [], 'anchors'):
        yield vectors, 'K-LEGACY-FUNC-F3-CTRL', ('sources',), 'set', value, 'source:'
    yield vectors, 'K-LEGACY-FUNC-F3-CTRL', ('sources', 0), 'set', None, 'source:'
    for identity in ('K-REPORT-TEXT', 'K-ERRATA', 'K-MOK', 'R-BOUNDS', 'R-ROUTE', 'R-LIFE', 'R-OUTER'):
        for value in (None, 'A-LICENSE'):
            yield obligations, identity, ('decision',), 'set', value, 'obligation selection'
    for file, identity in ((obligations, 'R-LIFE'), (vectors, 'K-C0-MODIFIED-AMBIGUOUS')):
        for field in ('authority', 'decision'):
            yield file, identity, (field,), '', None, 'required fields'
            for value in ('unknown', [], {}, False):
                yield file, identity, (field,), 'set', value, 'schema: authority' if field == 'authority' else 'policy:'
    for file, identity, field in ((obligations, 'K-QUICK', 'cases'),
                                   (vectors, 'K-C0-MODIFIED-AMBIGUOUS', 'obligations')):
        for value in (None, {}, 'K-QUICK', [None], ['unknown']):
            yield file, identity, (field,), 'set', value, 'schema: obligation'
    yield vectors, 'K-C0-MODIFIED-AMBIGUOUS', ('decision',), 'set', None, 'required non-null link'
    yield vectors, 'K-C0-MODIFIED-AMBIGUOUS', ('output_hex',), 'set', '', 'outputs must be separate'
    yield vectors, 'K-C0-MODIFIED-AMBIGUOUS', ('policy_output_hex',), 'set', None, 'outputs must be separate'
    yield vectors, 'K-SYNTAX-MINIMAL', ('policy_output_hex',), 'set', '', 'selected output authority'
    yield obligations, 'R-LIFE', ('authority',), 'set', 'pinned', 'selected obligation authority'
    yield vectors, 'B-CURRENT-SET', ('proof', 'owner'), 'set', 'Kitty normative', 'observation proof link'
    yield vectors, 'B-CURRENT-SET', ('proof',), 'set', None, 'observation proof link'
    yield vectors, 'B-CURRENT-SET', ('authority',), 'set', 'pinned', 'observation authority/direction'


def mutate_metadata(data, mutation):
    file, identity, path, operation, value, _ = mutation
    target = next(row for row in data[file] if row['id'] == identity)
    for key in path[:-1]:
        target = target[key]
    if operation == 'set':
        target[path[-1]] = copy.deepcopy(value)
    else:
        del target[path[-1]]


def nested_anchor_mutations(data):
    """Complete parents must not hide descendants at any corpus entry path."""
    good = copy.deepcopy(next(o for o in data['obligations.jsonl'] if o['id'] == 'K-QUICK')['source'])
    bad = dict(good, uri='unknown')
    partial = {'lines': good['lines']}
    for file, identity, path, value, diagnostic in (
        ('obligations.jsonl', 'K-QUICK', ('source', 'sources'), [bad], 'immutable anchor URI'),
        ('obligations.jsonl', 'K-QUICK', ('source', 'source'), partial, 'complete anchor'),
        ('vectors.jsonl', 'K-LEGACY-FUNC-F3-CTRL', ('sources', 0, 'source'), bad, 'immutable anchor URI'),
        ('vectors.jsonl', 'K-LEGACY-FUNC-F3-CTRL', ('sources', 0, 'sources'), [partial], 'complete anchor'),
        ('policies.json', 'A-LIFE', ('source',), dict(good, sources=[partial]), 'complete anchor'),
        ('obligations.jsonl', 'K-QUICK', ('source', 'annotations'), [{'context': {'source': bad}}], 'immutable anchor URI'),
        ('obligations.jsonl', 'K-QUICK', ('source', 'sources'), [good], None),
        ('policies.json', 'A-LIFE', ('source',), dict(good, annotations=[
            {'source': None, 'sources': [dict(good, source=good)]}, {'note': 'valid metadata'}]), None),
    ):
        yield file, identity, path, 'set', value, diagnostic


class CorpusTests(unittest.TestCase):
    def setUp(self):
        audit = ROOT / '.rmux-audit'
        audit.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='kitty-regression-', dir=audit)
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        for name in ('pin.json', *CHECK['FILES']):
            shutil.copyfile(ROOT / CHECK['CORPUS'] / name, self.directory / name)
        self.pin, self.data, _ = CHECK['load'](self.directory)

    def row(self, identity):
        return next(r for r in self.data['vectors.jsonl'] if r['id'] == identity)

    def save_rehashed(self):
        # These tests intentionally cross the digest barrier and test structure.
        for name, data in self.data.items():
            raw = (self.directory / name).read_bytes()
            previous = ([CHECK['read_json'](line) for line in raw.splitlines()]
                        if name.endswith('.jsonl') else CHECK['read_json'](raw))
            if previous == data:
                continue
            text = (''.join(json.dumps(r)+'\n' for r in data) if name.endswith('.jsonl')
                    else json.dumps(data)+'\n')
            raw = text.encode('utf-8')
            (self.directory / name).write_bytes(raw)
            self.pin['population'][name] = {'sha256':CHECK['digest'](raw), 'bytes':len(raw)}
        (self.directory / 'pin.json').write_text(json.dumps(self.pin)+'\n', encoding='utf-8')

    def validate(self):
        return CHECK['validate'](*CHECK['load'](self.directory))

    def test_baseline(self):
        self.assertEqual(self.validate(), (941,32))

    def test_complete_anchors_continue_recursive_validation(self):
        good = next(o for o in self.data['obligations.jsonl'] if o['id'] == 'K-QUICK')['source']
        # Inspect source/source, source/sources, sources/source, sources/sources
        # and annotation dict/list traversal, with a fresh digest registry each time.
        for outer in ('source', 'sources', 'annotations'):
            for inner in ('source', 'sources'):
                for missing in (None, 'uri', 'lines', 'sha256', 'invalid-uri'):
                    child = copy.deepcopy(good)
                    if missing == 'invalid-uri':
                        child['uri'] = 'unknown'
                    elif missing:
                        del child[missing]
                    parent = dict(good, **{inner: [child] if inner == 'sources' else child})
                    value = {outer: parent if outer == 'source' else [parent],
                             'optional': {'source': None}}
                    with self.subTest(outer=outer, inner=inner, missing=missing):
                        if missing:
                            diagnostic = 'immutable anchor URI' if missing == 'invalid-uri' else 'complete anchor'
                            with self.assertRaisesRegex(ValueError, diagnostic):
                                CHECK['anchors'](value, self.pin['pin'], None, {})
                        else:
                            CHECK['anchors'](value, self.pin['pin'], None, {})

    def test_nested_anchors_at_real_release_entry_after_rehash(self):
        # A private index and full tracked-file copy preserve the actual release
        # CLI and checker subprocess. No patched admission result or CLI is used.
        root = self.directory / 'release-tree'
        root.mkdir()
        git = shutil.which('git')
        def git_output(*args):
            return subprocess.check_output([git, *args], cwd=ROOT)
        paths = git_output('ls-files', '-z').decode().split('\0')
        for name in filter(None, paths):
            target = root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, target)
        index = self.directory / 'release.index'
        index_path = Path(git_output('rev-parse', '--git-path', 'index').decode().strip())
        shutil.copyfile(ROOT / index_path, index)
        env = dict(os.environ, GIT_DIR=git_output('rev-parse', '--absolute-git-dir').decode().strip(),
                   GIT_WORK_TREE=str(root), GIT_INDEX_FILE=str(index),
                   GIT_OPTIONAL_LOCKS='0', PYTHONDONTWRITEBYTECODE='1')
        original, original_pin = copy.deepcopy(self.data), copy.deepcopy(self.pin)
        payloads = {name: (self.directory / name).read_bytes() for name in ('pin.json', *self.data)}
        for mutation in nested_anchor_mutations(original):
            with self.subTest(mutation=mutation):
                self.data, self.pin = copy.deepcopy(original), copy.deepcopy(original_pin)
                for name, raw in payloads.items():
                    (self.directory / name).write_bytes(raw)
                mutate_metadata(self.data, mutation)
                self.save_rehashed()
                for name, raw in payloads.items():
                    changed = (self.directory / name).read_bytes()
                    if name not in (mutation[0], 'pin.json'):
                        self.assertEqual(changed, raw)
                        self.assertEqual(self.pin['population'][name], original_pin['population'][name])
                    (root / CHECK['CORPUS'] / name).write_bytes(changed)
                result = subprocess.run([sys.executable, '-B', str(root / 'scripts/release/validate-contracts.py')],
                                        cwd=root, env=env, input='', text=True, capture_output=True, timeout=60)
                self.assertEqual(result.returncode, 1 if mutation[-1] else 0, result.stderr)
                if mutation[-1]:
                    self.assertIn(mutation[-1], result.stderr)
                    self.assertNotIn('digest:', result.stderr)
                    self.assertNotIn('release-contracts-ok', result.stdout)
                else:
                    self.assertEqual(result.stdout, 'release-contracts-ok\n')
                    self.assertEqual(result.stderr, '')

    def test_metadata_counterexamples_after_intended_rehash(self):
        original = copy.deepcopy(self.data)
        original_pin = copy.deepcopy(self.pin)
        payloads = {name: (self.directory / name).read_bytes() for name in self.data}
        for mutation in metadata_mutations():
            with self.subTest(mutation=mutation):
                self.data = copy.deepcopy(original)
                self.pin = copy.deepcopy(original_pin)
                for name, raw in payloads.items():
                    (self.directory / name).write_bytes(raw)
                mutate_metadata(self.data, mutation)
                self.save_rehashed()
                for name, raw in payloads.items():
                    if name != mutation[0]:
                        self.assertEqual((self.directory / name).read_bytes(), raw)
                        self.assertEqual(self.pin['population'][name], original_pin['population'][name])
                with self.assertRaisesRegex(ValueError, mutation[-1]):
                    self.validate()

    def test_optional_null_sources_and_unselected_obligations(self):
        # Vectors inherit obligation anchors. Policy-only metadata and preserved
        # current-source proofs do not acquire a normative Kitty-source duty.
        for identity in ('K-SYNTAX-MINIMAL', 'K-C0-MODIFIED-AMBIGUOUS', 'B-CURRENT-SET'):
            self.row(identity)['source'] = None
        next(o for o in self.data['obligations.jsonl'] if o['id'] == 'R-BOUNDS')['source'] = None
        for identity in ('R-FRAME', 'R-PRODUCER', 'K-QUICK'):
            self.assertIsNone(next(o for o in self.data['obligations.jsonl'] if o['id'] == identity)['decision'])
        self.save_rehashed()
        self.assertEqual(self.validate(), (941,32))

    def test_coverage_obligation_links_have_array_shape(self):
        section = self.data['coverage.json']['sections'][0]
        section['obligations'] = {identity: True for identity in section['obligations']}
        self.save_rehashed()
        with self.assertRaisesRegex(ValueError, 'coverage: section links'):
            self.validate()

    def test_each_population_digest_rejects_byte_corruption(self):
        for name in CHECK['FILES']:
            with self.subTest(name=name):
                path = self.directory / name
                original = path.read_bytes()
                path.write_bytes(original + b' ')
                with self.assertRaisesRegex(ValueError, 'digest:'):
                    self.validate()
                path.write_bytes(original)

    def test_pin_duplicate_version_and_provenance(self):
        path = self.directory / 'pin.json'
        original = path.read_bytes()
        path.write_bytes(original[:-2] + b',"version":"other"}\n')
        with self.assertRaisesRegex(ValueError, 'duplicate JSON key'):
            self.validate()
        for key, value, diagnostic in [('version','wrong','schema/version'),
                                       ('scope','full Kitty support','qualification scope')]:
            data = json.loads(original)
            data[key] = value
            path.write_text(json.dumps(data), encoding='utf-8')
            with self.assertRaisesRegex(ValueError, diagnostic):
                self.validate()
        data = json.loads(original)
        data['pin']['revision'] = '0'*40
        path.write_text(json.dumps(data), encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'immutable provenance'):
            self.validate()

    def test_refuter_corruptions_after_rehash(self):
        original = copy.deepcopy(self.data)
        cases = [
            ('K-FUNC-F3-1','input_hex','1b5b31347e','functional wire identity'),
            ('K-FLAG-10-UP-REPEAT','input_hex','1b5b313b313a3441','event range'),
            ('K-TILDE-E3','input_hex','1b5b323b307e','modifier range'),
            ('K-TILDE-E3','input_hex','1b5b323b313a333b397e','field count'),
            ('K-FLAG-31-TEXT-PRESS','output_hex','1b5b39373a36353a39373b323b363675','associated-text output'),
            ('K-CTRL-097','output_hex','02','Ctrl letter mapping'),
            ('K-SYNTAX-ALT-BASE','event',dict(self.row('K-SYNTAX-ALT-BASE')['event'],base=100),'decoded event fields'),
            ('K-C0-MODIFIED-AMBIGUOUS','decision',None,'required non-null link'),
            ('K-FLAG-31-QUERY','output_hex','1b5b3f333075','query/state mismatch'),
            ('K-STATE-ALT-POP','next',dict(self.row('K-STATE-ALT-POP')['next'], main=[31]),'query/state mismatch'),
            ('K-LEGACY-FUNC-F3-CTRL','policy_output_hex','1b5b313b3552','legacy F1/F2/F3/F4'),
        ]
        for identity, field, value, diagnostic in cases:
            with self.subTest(identity=identity, field=field):
                self.data = copy.deepcopy(original)
                self.row(identity)[field] = value
                self.save_rehashed()
                with self.assertRaisesRegex(ValueError, diagnostic):
                    self.validate()

    def test_missing_repeat_partition_counterfactual(self):
        rows = self.data['vectors.jsonl']
        missing = [r for r in rows if not (r.get('event',{}).get('key') in (9,13,127)
                   and r['event']['event_type'] == 2)]
        with self.assertRaisesRegex(ValueError, 'missing C0 press/repeat/release'):
            finite_policies(missing)
        original = copy.deepcopy(self.data)
        removed = {r['id'] for r in rows} - {r['id'] for r in missing}
        self.data['vectors.jsonl'] = missing
        for obligation in self.data['obligations.jsonl']:
            obligation['cases'] = [i for i in obligation['cases'] if i not in removed]
        coverage = self.data['coverage.json']
        coverage['vectors'] = len(missing)
        coverage['authority_counts'] = {a:sum(r['authority']==a for r in missing)
                                        for a in {r['authority'] for r in missing}}
        coverage['c0_repeat_partition']['cases'] = [i for i in
            coverage['c0_repeat_partition']['cases'] if i not in removed]
        self.save_rehashed()
        with self.assertRaisesRegex(ValueError, 'C0 partition links'):
            self.validate()
        self.data = original
        # IDs are not the oracle for finite partitions.
        for index, row in enumerate(rows):
            row['id'] = 'renamed-' + str(index)
        finite_policies(rows)

    def test_duplicate_id_and_missing_field_after_rehash(self):
        self.data['vectors.jsonl'].append(copy.deepcopy(self.data['vectors.jsonl'][0]))
        self.save_rehashed()
        with self.assertRaisesRegex(ValueError, 'duplicate/invalid ID'):
            self.validate()
        self.data['vectors.jsonl'].pop()
        del self.data['vectors.jsonl'][0]['direction']
        self.save_rehashed()
        with self.assertRaisesRegex(ValueError, 'required fields'):
            self.validate()

    def test_wrong_c0_repeat_policy_and_flag8_control(self):
        for identity in ('K-C0-F02-ENTER-E2','K-C0-F03-TAB-E2','K-C0-F10-BACKSPACE-E2'):
            with self.subTest(identity=identity):
                rows = copy.deepcopy(self.data['vectors.jsonl'])
                row = next(r for r in rows if r['id'] == identity)
                field = 'policy_output_hex' if row['output_hex'] is None else 'output_hex'
                row[field] = '0d'
                with self.assertRaisesRegex(ValueError, 'C0 partition output'):
                    finite_policies(rows)
        self.assertEqual(self.row('K-C0-F08-ENTER-E2')['output_hex'], '1b5b313375')
        self.assertEqual(self.row('K-C0-TAB-F03-M5-E2')['policy_output_hex'], '1b5b393b363a3275')
        self.assertEqual(self.row('K-C0-TAB-F01-M1-E1')['policy_output_hex'], '1b5b5a')

    def test_uniform_grammar_boundaries_and_omissions(self):
        for final, key in [('u','97'),('~','2'),('A','1')]:
            for fields in (';1:4',';0',';257',';1:0',';1:3;9' if final != 'u' else ';1;9'):
                with self.subTest(final=final, fields=fields):
                    with self.assertRaises(ValueError):
                        encoding(('\x1b['+key+fields+final).encode())
            for fields in ('',';1',';1:1',';:2',';1:'):
                encoding(('\x1b['+key+fields+final).encode())
        for raw in (b'\x1b[1;5R', b'\x1b[R', b'\x1b[97:65;1u', b'\x1b[55296u'):
            with self.assertRaises(ValueError):
                encoding(raw)
        self.assertEqual(encoding(b'\x1b[00097;0256:3u')['modifiers'],255)
        self.assertEqual(encoding(b'\x1b[0;;128512u')['text'],[128512])
        self.assertEqual(encoding(b'\x1b[1089::99;5u')['base'],99)

    def test_invalid_state_and_required_disposition(self):
        self.row('K-FLAG-31-QUERY')['next']['main'] = [32]
        with self.assertRaisesRegex(ValueError, 'pane stack'):
            CHECK['validate'](self.pin,self.data)
        self.row('K-FLAG-31-QUERY')['next']['main'] = [31]
        self.data['policies.json'][0]['selected_disposition'] = None
        with self.assertRaisesRegex(ValueError, 'missing candidate disposition'):
            CHECK['validate'](self.pin,self.data)

    def test_query_known_bits_and_pop0(self):
        state = {'screen':'main','main':[0],'alternate':[3], 'pane':'P1','runtime_generation':7}
        result, out = negotiation(state, b'\x1b[=63u\x1b[?u\x1b[<0u')
        self.assertEqual(result,dict(state,main=[31]))
        self.assertEqual(out,b'\x1b[?31u')
        for flag in (2,8):
            result, out = negotiation(state, f'\x1b[={flag};2u\x1b[?u\x1b[={flag};3u\x1b[?u'.encode())
            self.assertEqual(result,state)
            self.assertEqual(out,f'\x1b[?{flag}u\x1b[?0u'.encode())

    def test_policy_closure_each_new_input(self):
        fn = RELEASE['validate_candidate']
        namespace = fn.__globals__
        original_load = namespace['load']
        paths = ['scripts/check-kitty-conformance.py','scripts/kitty_corpus.py',
                 'scripts/test-kitty-conformance.py'] + [(CHECK['CORPUS']/n).as_posix() for n in ('pin.json',*CHECK['FILES'])]
        for missing in paths:
            def altered(path):
                contract = original_load(path)
                if path.name == 'candidate-contract.json':
                    contract['policy_paths'].remove(missing)
                return contract
            with self.subTest(missing=missing), patch.dict(namespace, {'load':altered}):
                with self.assertRaisesRegex(ValueError, 'release policy does not cover'):
                    fn()

    def test_combined_release_gate_propagates_corpus_failure(self):
        (self.directory/'vectors.jsonl').write_bytes(b'corrupted fixture\n')
        original_run = subprocess.run
        calls = []
        def run(argv, **kwargs):
            if len(argv)>2 and str(argv[2]).endswith('check-kitty-conformance.py'):
                calls.append(argv)
                return original_run(argv+['--corpus',str(self.directory)], **kwargs)
            return original_run(argv, **kwargs)
        output = io.StringIO()
        with patch.object(subprocess,'run',side_effect=run), redirect_stdout(output):
            with self.assertRaisesRegex(ValueError, 'Kitty corpus integrity failed:.*digest:'):
                RELEASE['main']()
        self.assertEqual(len(calls),1)
        self.assertNotIn('release-contracts-ok',output.getvalue())


if __name__ == '__main__':
    unittest.main()
