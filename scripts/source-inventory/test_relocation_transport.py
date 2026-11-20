"""The admitted move must preserve attributes and the oversize file identity."""
import copy
import unittest
from pathlib import Path
from unittest.mock import patch

from compare import compare
from model import digest


class RelocationTransportTests(unittest.TestCase):
    def test_exact_relocation_requires_an_explicit_epoch(self):
        old, new = 'src/old.rs', 'src/new.rs'
        data = b'fn same() {}\n' * 600
        attribute = {'kind': 'allow_dead_code', 'value': 'dead_code',
                     'owner': ['fn same', 'fn same'], 'condition': True,
                     'classification': 'production'}
        def report(path, revision):
            return {'schema': 'rmux-source-inventory/1', 'errors': [],
                    'revision': revision, 'tool_inputs_sha256': 'same',
                    'declarations_sha256': 'same', 'semantic_attributes_schema': 1,
                    'totals': {'production': 600}, 'ratchets': {},
                    'files': [{'path': path, 'blob': digest(data),
                               'counts': {'production': 600}, 'physical_lines': 600,
                               'regions': [{'start': 1, 'end': 600, 'class': 'production'}],
                               'semantic_attributes': [copy.deepcopy(attribute)]}]}
        before, after = report(old, '1' * 40), report(new, '2' * 40)
        trees = {before['revision']: {old: {'blob': digest(data), 'data': data}},
                 after['revision']: {new: {'blob': digest(data), 'data': data}}}
        # Even equal hashes/counts cannot silently select a relocation epoch.
        with patch('compare.read_tree', side_effect=lambda repo, rev: trees[rev]):
            with self.assertRaisesRegex(ValueError, 'epoch|activation|relocation'):
                compare(Path('.'), before, after)


    def test_original_599_limit_and_existing_oversize_growth_remain_strict(self):
        def report(lines, revision):
            data = b'fn retained() {}\n' * lines
            row = dict(path='src/lib.rs', blob=digest(data), counts={'production': lines},
                       physical_lines=lines, regions=[dict(start=1, end=lines, **{'class': 'production'})],
                       semantic_attributes=[])
            return {'schema': 'rmux-source-inventory/1', 'errors': [],
                    'revision': revision, 'tool_inputs_sha256': 'same',
                    'declarations_sha256': 'same', 'semantic_attributes_schema': 1,
                    'totals': {'production': lines}, 'ratchets': {},
                    'files': [row] if lines else []}, ({'src/lib.rs': {'blob': digest(data),
                    'data': data}} if lines else {})
        for old, new, reason in ((0, 599, None), (0, 600, 'new_production_file_ge_600'),
                                 (600, 600, None), (600, 601, 'existing_oversize_growth')):
            with self.subTest(before=old, after=new):
                before, left = report(old, '1' * 40)
                after, right = report(new, '2' * 40)
                trees = {before['revision']: left, after['revision']: right}
                with patch('compare.read_tree', side_effect=lambda repo, rev: trees[rev]):
                    violations = compare(Path('.'), before, after)['production_size_violations']
                self.assertEqual([row['reason'] for row in violations], [reason] if reason else [])


class ProvedEpochCanaries(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import json
        from model import read_tree
        from relocation_chain import load_chain
        cls.repo = Path(__file__).resolve().parents[2]
        cls.record = load_chain(cls.repo)
        cls.trees = [read_tree(cls.repo, cls.record[k]) for k in ('base', 'candidate')]
        cls.reports = []
        for side, revision, tree in zip(('before', 'after'), ('base', 'candidate'), cls.trees):
            cls.reports.append({
                'revision': cls.record[revision], 'semantic_attributes_schema': 1,
                'manifests': copy.deepcopy(cls.record['manifests_' + side]),
                'files': [dict(path=path, blob=tree[path]['blob'],
                               counts=copy.deepcopy(row['counts']),
                               physical_lines=row['physical_lines'],
                               semantic_attributes=copy.deepcopy(row['attributes']),
                               consumers=copy.deepcopy(row['consumers']))
                          for path, row in cls.record[side].items()]})

    def transport(self, reports=None, trees=None, epoch=None):
        from relocation_chain import EPOCH, transport
        return transport(self.repo, *(reports or self.reports), *(trees or self.trees),
                         epoch=epoch or EPOCH)

    def test_all_individual_semantics_and_51_sizes_transport_exactly(self):
        from collections import Counter
        from compare import semantic_population
        mapping, sizes = self.transport()
        self.assertEqual(len(sizes), 51)
        old, _ = semantic_population(self.reports[0])
        new, _ = semantic_population(self.reports[1])
        normalized = Counter()
        for key, count in old.items():
            target, limit = mapping[key]
            self.assertEqual(count, limit)
            if target is not None:
                normalized[target] += count
        self.assertEqual(normalized, new)
        for report in self.reports:
            attrs = [a for row in report['files'] for a in row['semantic_attributes']]
            self.assertEqual(sum(a['kind'] == 'allow_dead_code' for a in attrs), 150)
            self.assertEqual(sum(a['kind'] == 'path' and a['classification'] == 'tooling'
                                 for a in attrs), 4)
        self.assertEqual(sum(limit for target, limit in mapping.values() if target is None), 638)

    def test_activation_and_epoch_are_explicit_and_ancestral(self):
        from relocation_chain import EPOCH, core_after_paths, source_epoch
        from test_core_ratchets import BASELINE, BOUNDS, CORE
        for epoch in (None, 'historical', 'forged'):
            with self.subTest(epoch=epoch), self.assertRaisesRegex(ValueError, 'epoch|activation'):
                source_epoch(self.repo, self.record['candidate'], epoch)
        self.assertFalse(source_epoch(self.repo, self.record['base'], EPOCH))
        self.assertEqual(core_after_paths(self.repo, self.record['base'], BOUNDS, CORE,
                                         BASELINE, EPOCH), {p: p for p in BOUNDS})
        current = core_after_paths(self.repo, self.record['candidate'], BOUNDS, CORE, BASELINE, EPOCH)
        self.assertEqual(len(set(current.values())), 12)
        self.assertEqual({p: n for p, n in current.items() if p != n}, self.record['core_after'])
        for before, after in ((self.reports[1], self.reports[0]),):
            with self.assertRaisesRegex(ValueError, 'reverse'):
                self.transport([before, after], list(reversed(self.trees)))

    def test_tool_only_inactive_epoch_preserves_original_behavior(self):
        from relocation_chain import EPOCH, transport
        for epoch in (None, 'historical', EPOCH):
            self.assertEqual(transport(self.repo, self.reports[0], self.reports[0],
                                       self.trees[0], self.trees[0], epoch), ({}, {}))

    def test_individual_attribute_cfg_class_consumer_and_multiplicity_canaries(self):
        mutations = {
            'add': lambda row: row['semantic_attributes'].append(copy.deepcopy(row['semantic_attributes'][0])),
            'remove': lambda row: row['semantic_attributes'].pop(0),
            'value': lambda row: row['semantic_attributes'][0].update(value='other'),
            'owner': lambda row: row['semantic_attributes'][0].update(owner=['other', 'other']),
            'cfg': lambda row: row['semantic_attributes'][0].update(condition='windows'),
            'class': lambda row: row['semantic_attributes'][0].update(classification='tests'),
            'consumer': lambda row: row['consumers'][0]['consumers'].__setitem__(0, 'forged.rs:mod'),
            'consumer_add': lambda row: row['consumers'][0]['consumers'].append('extra.rs:mod'),
            'consumer_remove': lambda row: row['consumers'][0]['consumers'].pop(),
            'consumer_cfg': lambda row: row['consumers'][0].update(condition='forged'),
            'consumer_role': lambda row: row['consumers'][0].update(role='tests'),
            'consumer_multiplicity': lambda row: row['consumers'].append(copy.deepcopy(row['consumers'][0])),
        }
        for side in (0, 1):
            for name, mutate in mutations.items():
                with self.subTest(side=side, mutation=name):
                    reports = copy.deepcopy(self.reports)
                    row = next(r for r in reports[side]['files'] if 'options/registry' in r['path']
                               and any(a['kind'] == 'allow_dead_code' for a in r['semantic_attributes']))
                    mutate(row)
                    with self.assertRaisesRegex(ValueError, 'identity'):
                        self.transport(reports)

    def test_allow_and_tooling_are_never_globally_exempted(self):
        for kind, classification in (('allow_dead_code', None), ('path', 'tooling')):
            for operation in ('add', 'remove', 'substitute'):
                with self.subTest(kind=kind, operation=operation):
                    reports = copy.deepcopy(self.reports)
                    row, attribute = next((r, a) for r in reports[1]['files']
                                          for a in r['semantic_attributes']
                                          if a['kind'] == kind and
                                          (classification is None or a['classification'] == classification))
                    if operation == 'add':
                        row['semantic_attributes'].append(copy.deepcopy(attribute))
                    elif operation == 'remove':
                        row['semantic_attributes'].remove(attribute)
                    else:
                        attribute['owner'] = ['replacement', 'replacement']
                    with self.assertRaisesRegex(ValueError, 'identity'):
                        self.transport(reports)

    def test_file_root_source_collision_growth_and_new_large_file_canaries(self):
        for mutation in ('missing', 'collision', 'undeclared_move', 'new_large', 'source', 'growth', 'root', 'target'):
            with self.subTest(mutation=mutation):
                reports = copy.deepcopy(self.reports)
                trees = [dict(tree) for tree in self.trees]
                row = next(r for r in reports[1]['files'] if r['path'] == self.record['sizes'][0]['new'])
                if mutation == 'missing':
                    reports[1]['files'].remove(row)
                elif mutation == 'collision':
                    reports[1]['files'].append(copy.deepcopy(row))
                elif mutation == 'undeclared_move':
                    row['path'] += '.unproved.rs'
                elif mutation == 'new_large':
                    reports[1]['files'].append(dict(row, path='src/unproved.rs', counts={'production': 600}))
                elif mutation in ('source', 'growth'):
                    path = row['path']
                    trees[1][path] = dict(trees[1][path], data=trees[1][path]['data'] + b'\n')
                    if mutation == 'growth':
                        row['counts']['production'] += 1
                        row['physical_lines'] += 1
                elif mutation == 'root':
                    path = 'crates/rmux-client/Cargo.toml'
                    trees[1][path] = dict(trees[1][path], data=trees[1][path]['data'] +
                                         b'\n[[bin]]\nname="unproved"\npath="src/lib.rs"\n')
                else:
                    path = 'xtask/src/main.rs'
                    trees[1][path] = dict(trees[1][path], data=trees[1][path]['data'].replace(
                        b'options/registry/mod.rs', b'options/registry/table.rs'))
                with self.assertRaisesRegex(ValueError, 'identity|population|collision'):
                    self.transport(reports, trees)

    def test_core_population_missing_zero_collision_and_growth_refuse(self):
        import os
        from relocation_chain import EPOCH, core_after_paths
        from test_core_ratchets import BASELINE, BOUNDS, CORE, CoreRatchetTests
        addresses = core_after_paths(self.repo, self.record['candidate'], BOUNDS, CORE, BASELINE, EPOCH)
        before = {'errors': [], 'files': [dict(path=p, counts={'production': n}) for p, n in BOUNDS.items()]}
        after = {'errors': [], 'files': [dict(path=n, counts={'production': BOUNDS[p]})
                                        for p, n in addresses.items()]}
        empty_delta = {'semantic_attribute_additions': [], 'production_size_violations': []}
        for mutation in ('control', 'missing', 'zero', 'collision', 'old_path_fallback', 'growth'):
            value = copy.deepcopy(after)
            row = value['files'][4]
            if mutation == 'missing':
                value['files'].remove(row)
            elif mutation == 'zero':
                row['counts']['production'] = 0
            elif mutation == 'collision':
                row['path'] = value['files'][5]['path']
            elif mutation == 'old_path_fallback':
                row['path'] = list(BOUNDS)[4]
            elif mutation == 'growth':
                value['files'][0]['counts']['production'] += 1
            case = CoreRatchetTests('test_core_growth_and_semantic_path_are_removed')
            with self.subTest(mutation=mutation), patch.dict(os.environ, {
                    'SOURCE_RATCHET_REVISION': self.record['candidate'], 'SOURCE_SYNTAX': '/unused'}), \
                    patch('test_core_ratchets.scan', side_effect=[before, value]), \
                    patch('test_core_ratchets.compare', return_value=empty_delta):
                result = unittest.TestResult()
                case.run(result)
                if mutation == 'control':
                    self.assertFalse(result.failures or result.errors)
                else:
                    self.assertTrue(result.failures or result.errors)
        weakened = dict(BOUNDS)
        weakened[CORE[0]] += 1
        with self.assertRaisesRegex(ValueError, 'bound'):
            core_after_paths(self.repo, self.record['candidate'], weakened, CORE, BASELINE, EPOCH)

    def test_missing_altered_chain_proof_hash_and_activation_refuse(self):
        import json
        import shutil
        import tempfile
        from relocation_chain import TOOL, load_chain
        audit = self.repo / '.rmux-audit/s2137/canaries'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            root = Path(directory)
            chain = root / 'relocation-revisions.json'
            original = (TOOL / chain.name).read_bytes()
            chain.write_bytes(original)
            shutil.copytree(TOOL / 'relocation-proofs', root / 'relocation-proofs')
            self.assertEqual(load_chain(self.repo, chain)['epoch'], self.record['epoch'])
            for mutation in ('missing', 'activation_absent', 'activation_forged', 'source_hash', 'collision'):
                with self.subTest(mutation=mutation):
                    data = json.loads(original)
                    if mutation == 'missing':
                        chain.unlink()
                    else:
                        if mutation == 'activation_absent':
                            del data[0]['activation']
                        elif mutation == 'activation_forged':
                            data[0]['activation'] = '0' * 40
                        elif mutation == 'source_hash':
                            data[0]['sizes'][0]['sha256_after'] = '0' * 64
                        else:
                            data[0]['core_after'][list(data[0]['core_after'])[0]] = 'src/cli/dispatch.rs'
                        chain.write_text(json.dumps(data))
                    with self.assertRaisesRegex(ValueError, 'chain|activation|identity'):
                        load_chain(self.repo, chain)
                    chain.write_bytes(original)
            for name in self.record['proofs']:
                path = root / 'relocation-proofs' / name
                data = path.read_bytes()
                for missing in (False, True):
                    if missing:
                        path.unlink()
                    else:
                        path.write_bytes(data + b'alteration')
                    with self.subTest(proof=name, missing=missing), self.assertRaisesRegex(ValueError, 'proof'):
                        load_chain(self.repo, chain)
                    path.write_bytes(data)

    def test_deletion_then_restoration_does_not_rewrite_chain_history(self):
        import os
        import subprocess
        import tempfile
        from model import borrow_objects, git
        from relocation_chain import CHAIN, load_chain
        audit = self.repo / '.rmux-audit/s2137/history'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            subprocess.run(['git', 'init', '-q', str(fixture)], check=True)
            common = git(self.repo, 'rev-parse', '--git-common-dir').decode().strip()
            objects = (self.repo / common / 'objects').resolve()
            borrow_objects(fixture / '.git', objects)
            head = git(self.repo, 'rev-parse', 'HEAD').decode().strip()
            git(fixture, 'update-ref', 'HEAD', head)
            git(fixture, 'read-tree', head)
            git(fixture, 'update-index', '--force-remove', CHAIN)
            removed_tree = git(fixture, 'write-tree').decode().strip()
            original_tree = git(self.repo, 'rev-parse', head + '^{tree}').decode().strip()
            env = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui',
                       GIT_AUTHOR_EMAIL='shideneyu@gmail.com', GIT_COMMITTER_NAME='Sidney Sissaoui',
                       GIT_COMMITTER_EMAIL='shideneyu@gmail.com')
            for tree in (removed_tree, original_tree):
                head = subprocess.check_output(['git', '-C', str(fixture), 'commit-tree', tree,
                                                 '-p', head, '-m', 'chain history canary'],
                                                env=env).decode().strip()
            git(fixture, 'update-ref', 'HEAD', head)
            with self.assertRaisesRegex(ValueError, 'removed|append-only'):
                load_chain(fixture)

    def recorded(self, tracked, expected):
        """Committed bytes whose SHA-256 is the pinned fingerprint (independent oracle)."""
        import subprocess
        from model import git
        for revision in git(self.repo, 'log', '--format=%H', 'HEAD', '--', tracked).decode().split():
            raw = subprocess.run(['git', '-C', str(self.repo), 'show', revision + ':' + tracked],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if not raw.returncode and digest(raw.stdout) == expected:
                return raw.stdout
        self.fail('recorded evidence absent from history: ' + tracked)

    def test_tracked_evidence_is_the_recorded_bytes_without_home_directories(self):
        import json
        import re
        from model import canonical
        from relocation_chain import ROOTS, TOOL
        home = re.compile(rb'/(?:Users|home)/[^/\s"]+/')
        prefix = 'scripts/source-inventory/'
        expected_counts = {'pass263.json': 35, 'pass264.json': 44, 'review263.txt': 0, 'review264.txt': 0}
        self.assertEqual(set(self.record['proofs']), set(expected_counts))
        for name, pinned in self.record['proofs'].items():
            with self.subTest(proof=name):
                tracked = (TOOL / 'relocation-proofs' / name).read_bytes()
                original = self.recorded(prefix + 'relocation-proofs/' + name, pinned)
                self.assertEqual(home.findall(tracked), [])
                self.assertEqual(len(home.findall(original)), expected_counts[name])
                self.assertEqual(home.sub(b'~/', original), tracked)
        tracked = (TOOL / 'relocation-revisions.json').read_bytes()
        self.assertEqual(home.findall(tracked), [])
        rows = json.loads(tracked)
        original = json.loads(self.recorded(prefix + 'relocation-revisions.json',
                                            '972df1fc23d6606873453dd1b87f3f0b97e8640623905b4115f497f80bdd6bfd'))
        self.assertEqual([digest(canonical(row).encode()) for row in original], list(ROOTS))
        self.assertEqual(rows[1], original[1])
        self.assertEqual({k: v for k, v in rows[0].items() if k != 'source_seal'},
                         {k: v for k, v in original[0].items() if k != 'source_seal'})
        self.assertEqual(rows[0]['source_seal']['sha256'], original[0]['source_seal']['sha256'])
        self.assertEqual(rows[0]['source_seal']['path'],
                         home.sub(b'~/', original[0]['source_seal']['path'].encode()).decode())

    def test_only_the_exact_public_view_of_recorded_evidence_is_admitted(self):
        import json
        import re
        import shutil
        import tempfile
        from relocation_chain import TOOL, load_chain
        audit = self.repo / '.rmux-audit/s1731/public-view-canaries'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            root = Path(directory)
            chain = root / 'relocation-revisions.json'
            shutil.copyfile(TOOL / chain.name, chain)
            shutil.copytree(TOOL / 'relocation-proofs', root / 'relocation-proofs')
            self.assertEqual(load_chain(self.repo, chain), self.record)
            proof = root / 'relocation-proofs/pass264.json'
            public = proof.read_bytes()
            recorded = self.recorded('scripts/source-inventory/relocation-proofs/pass264.json',
                                     self.record['proofs']['pass264.json'])
            proof.write_bytes(recorded)
            self.assertEqual(load_chain(self.repo, chain), self.record)
            for name, forged in (
                    ('other_home', public.replace(b'"~/', b'"/Users/forged/', 1)),
                    ('other_directory', public.replace(b'~/Projects/rmux-s2136-full-layout',
                                                       b'~/Projects/rmux-s2136-forged-layout', 1)),
                    ('other_placeholder', public.replace(b'"~/', b'"$HOME/', 1)),
                    ('partial_view', re.sub(rb'/(?:Users|home)/[^/\s"]+/', b'~/', recorded, count=1))):
                with self.subTest(proof=name), self.assertRaisesRegex(ValueError, 'proof'):
                    proof.write_bytes(forged)
                    load_chain(self.repo, chain)
            proof.write_bytes(public)
            original = chain.read_bytes()
            seal = json.loads(original)[0]['source_seal']
            for name, value in (('other_home', dict(seal, path='/Users/forged/' + seal['path'][2:])),
                                ('other_directory', dict(seal, path=seal['path'].replace('s2136', 's2137'))),
                                ('other_placeholder', dict(seal, path='$HOME/' + seal['path'][2:])),
                                ('seal_hash', dict(seal, sha256='0' * 64))):
                with self.subTest(chain=name), self.assertRaisesRegex(ValueError, 'chain'):
                    data = json.loads(original)
                    data[0]['source_seal'] = value
                    chain.write_text(json.dumps(data))
                    load_chain(self.repo, chain)
            chain.write_bytes(original)
            self.assertEqual(load_chain(self.repo, chain), self.record)

    def test_history_cannot_substitute_an_equivalent_home_directory(self):
        import json
        import os
        import subprocess
        import tempfile
        from model import borrow_objects, git
        from relocation_chain import CHAIN, load_chain
        audit = self.repo / '.rmux-audit/s1731/history'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            subprocess.run(['git', 'init', '-q', str(fixture)], check=True)
            common = git(self.repo, 'rev-parse', '--git-common-dir').decode().strip()
            objects = (self.repo / common / 'objects').resolve()
            borrow_objects(fixture / '.git', objects)
            head = git(self.repo, 'rev-parse', 'HEAD').decode().strip()
            data = json.loads(git(self.repo, 'show', head + ':' + CHAIN))
            data[0]['source_seal']['path'] = '/home/forged/' + data[0]['source_seal']['path'][2:]
            blob = subprocess.check_output(['git', '-C', str(fixture), 'hash-object', '-w', '--stdin'],
                                           input=json.dumps(data, indent=2).encode()).decode().strip()
            git(fixture, 'read-tree', head)
            git(fixture, 'update-index', '--cacheinfo', '100644,' + blob + ',' + CHAIN)
            forged_tree = git(fixture, 'write-tree').decode().strip()
            original_tree = git(self.repo, 'rev-parse', head + '^{tree}').decode().strip()
            env = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui',
                       GIT_AUTHOR_EMAIL='shideneyu@gmail.com', GIT_COMMITTER_NAME='Sidney Sissaoui',
                       GIT_COMMITTER_EMAIL='shideneyu@gmail.com')
            for tree in (forged_tree, original_tree):
                head = subprocess.check_output(['git', '-C', str(fixture), 'commit-tree', tree,
                                                 '-p', head, '-m', 'equivalent home canary'],
                                                env=env).decode().strip()
            git(fixture, 'update-ref', 'HEAD', head)
            with self.assertRaisesRegex(ValueError, 'append-only'):
                load_chain(fixture)


class SemanticActivationCanaries(unittest.TestCase):
    """D573(3) option (i): one sealed transition activates whole; nothing else is admitted.

    Strict both ways: on a revision only the last activation on its ancestry is admitted,
    so a descendant can neither keep the proved population nor stack activations.
    """
    REVISION = 'dfe42e88ebe2c33b4bd5df50fbce5d82cd1ca1f0'
    CONSUMERS = (
        ('tests/common/mod.rs', 'target_os = "linux"', 'tests/empty_socket_lock.rs:mod'),
        ('tests/common/mod.rs', 'unix', 'tests/window_json_layout.rs:mod'),
        ('tests/common_cross/mod.rs', True, 'tests/server_invalid_flag.rs:mod'),
        ('tests/common/mod.rs', 'unix', 'tests/cli_command_lock_long_wait.rs:mod'),
        ('crates/rmux-server/tests/common/mod.rs', 'unix',
         'crates/rmux-server/tests/idle_exit_endpoint.rs:mod'),
    )
    REFUSAL = '^descendant semantic attribute/cfg/consumer identity differs$'

    @classmethod
    def setUpClass(cls):
        from relocation_chain import admitted_semantics, load_chain, proved_semantics
        cls.repo = Path(__file__).resolve().parents[2]
        cls.record = load_chain(cls.repo)
        cls.proved = proved_semantics(cls.record)
        cls.activated = admitted_semantics(cls.repo, 'HEAD', cls.record)

    def rows(self, population):
        """Activation-sized rows whose guarded semantic identities are exactly ``population``."""
        import json
        rows = {path: dict(path=path, counts=copy.deepcopy(row['counts']),
                           consumers=row['consumers'], semantic_attributes=[])
                for path, row in self.record['after'].items()}
        seen = set()
        for key, count in population.items():
            path, attribute, consumers = json.loads(key)
            row = rows.setdefault(path, dict(path=path, counts={}, semantic_attributes=[]))
            if path not in seen:
                row['consumers'] = consumers
                seen.add(path)
            self.assertEqual(row['consumers'], consumers, path)
            row['semantic_attributes'].extend(copy.deepcopy(attribute) for _ in range(count))
        return rows

    def check(self, population, admitted=None):
        from relocation_chain import validate_descendant
        rows = self.rows(population)
        validate_descendant({'files': list(rows.values())}, dict.fromkeys(rows), self.record,
                            self.activated if admitted is None else admitted)

    def without(self, population, path_name, condition, consumer):
        import collections
        import json
        from relocation_chain import semantic_key
        result = collections.Counter()
        for key, count in population.items():
            path, attribute, groups = json.loads(key)
            for group in groups:
                if (path, group['condition']) == (path_name, condition):
                    group['consumers'].remove(consumer)
            result[semantic_key(path, attribute, groups)] += count
        return result

    def test_entry_binds_sealed_transition_and_three_documented_consumers(self):
        from relocation_chain import load_chain, semantic_activations
        entry, = semantic_activations()
        sealed, = load_chain(self.repo, epochs=('D570-f7f744fb',))['proof_epochs']
        self.assertEqual((entry['revision'], entry['proof_epoch'], entry['proof_sha256']),
                         (self.REVISION, 'D570-f7f744fb', sealed['sha256']))
        self.assertEqual(tuple((row['path'], row['condition'], row['consumer'])
                               for row in entry['consumer_additions']), self.CONSUMERS)
        for row in entry['consumer_additions']:
            with self.subTest(consumer=row['consumer']):
                self.assertTrue(row['reason'])
                self.assertRegex(row['introduced_commit'], '^[0-9a-f]{40}$')

    def test_activation_is_exactly_sealed_delta_plus_each_consumer(self):
        import collections
        from model import canonical
        from relocation_chain import load_chain
        sealed, = load_chain(self.repo, epochs=('D570-f7f744fb',))['proof_epochs']
        additions, removals = (collections.Counter({canonical(item['key']): item['count']
                                                   for item in sealed[name]})
                               for name in ('semantic_additions', 'semantic_removals'))
        changed = self.activated - self.proved
        for item in self.CONSUMERS:
            changed = self.without(changed, *item)
        self.assertEqual(changed, additions)
        self.assertEqual(self.proved - self.activated, removals)

    def test_only_descendants_of_the_epoch_revision_are_activated(self):
        from relocation_chain import admitted_semantics
        for revision in (self.record['activation'], 'f7f744fb51e3f6bbfce1de475011a9b0efe85e20',
                         '3a9797525dfcf6ad25531744d0f8d63f33eae7c5'):
            with self.subTest(revision=revision):
                admitted = admitted_semantics(self.repo, revision, self.record)
                self.assertEqual(admitted, self.proved)
                self.check(self.proved, admitted)
                with self.assertRaisesRegex(ValueError, self.REFUSAL):
                    self.check(self.activated, admitted)

    def test_activated_population_passes_and_proved_population_refuses_on_head(self):
        from relocation_chain import admitted_semantics
        admitted = admitted_semantics(self.repo, 'HEAD', self.record)
        self.assertNotEqual(admitted, self.proved)
        self.check(self.activated, admitted)
        # Canary: reverting to the proved population would silently restore the 3R
        # and drop the 4A and the three documented consumers; it must refuse.
        with self.assertRaisesRegex(ValueError, self.REFUSAL):
            self.check(self.proved, admitted)

    def test_later_activation_supersedes_rather_than_stacks(self):
        import relocation_chain
        from model import canonical, digest
        entry, = relocation_chain.SEMANTIC_ACTIVATIONS
        empty = dict(epoch='canary-empty-proof', sha256='0' * 64,
                     semantic_removals=[], semantic_additions=[])
        later = dict(entry, epoch='canary-later', previous_sha256=relocation_chain.ACTIVATION_ROOTS[0],
                     proof_epoch=empty['epoch'], proof_sha256=empty['sha256'],
                     consumer_additions=[dict(entry['consumer_additions'][1],
                                              consumer='tests/canary_later.rs:mod')])
        roots = relocation_chain.ACTIVATION_ROOTS + (digest(canonical(later).encode()),)
        real = relocation_chain.explicit_proof_epochs

        def proofs(repo, names):
            return [empty] if names == (empty['epoch'],) else real(repo, names)
        with patch.object(relocation_chain, 'SEMANTIC_ACTIVATIONS', (entry, later)), \
                patch.object(relocation_chain, 'ACTIVATION_ROOTS', roots), \
                patch.object(relocation_chain, 'explicit_proof_epochs', proofs):
            admitted = relocation_chain.admitted_semantics(self.repo, 'HEAD', self.record)
        self.assertNotIn(admitted, (self.proved, self.activated))
        self.check(admitted, admitted)
        for name, earlier in (('proved', self.proved), ('activated', self.activated)):
            with self.subTest(earlier=name), self.assertRaisesRegex(ValueError, self.REFUSAL):
                self.check(earlier, admitted)

    def test_partial_extra_or_withdrawn_consumer_transitions_refuse(self):
        import collections
        from relocation_chain import add_consumer
        for item in self.CONSUMERS:
            with self.subTest(withdrawn=item[2]), self.assertRaisesRegex(ValueError, self.REFUSAL):
                self.check(self.without(self.activated, *item))
        extra = dict(path='tests/common/mod.rs', module_dir='tests/common', condition='unix',
                     consumer='tests/undeclared_consumer.rs:mod')
        with self.assertRaisesRegex(ValueError, self.REFUSAL):
            self.check(add_consumer(self.activated, extra))
        sealed_only = self.activated
        for item in self.CONSUMERS:
            sealed_only = self.without(sealed_only, *item)
        with self.assertRaisesRegex(ValueError, self.REFUSAL):
            self.check(sealed_only)
        key = next(key for key in self.activated if 'ProcessChild' in key)
        with self.assertRaisesRegex(ValueError, self.REFUSAL):
            self.check(self.activated - collections.Counter({key: 1}))

    def test_withdrawn_substituted_or_appended_activation_refuses(self):
        import relocation_chain
        entry, = relocation_chain.SEMANTIC_ACTIVATIONS
        substituted = dict(entry, consumer_additions=entry['consumer_additions'] + [dict(
            entry['consumer_additions'][-1], consumer='tests/undeclared_consumer.rs:mod')])
        appended = dict(entry, previous_sha256=relocation_chain.ACTIVATION_ROOTS[0])
        for name, value in (('withdraw', ()), ('substitute', (substituted,)),
                            ('append', (entry, appended))):
            with self.subTest(change=name), patch.object(
                    relocation_chain, 'SEMANTIC_ACTIVATIONS', value), self.assertRaisesRegex(
                    ValueError, 'semantic activation'):
                relocation_chain.admitted_semantics(self.repo, 'HEAD', self.record)
