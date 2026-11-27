"""Adversarial projections: exact tree pins do not replace ledger leaf rules."""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import public_cut as cut
import public_cut_registry as registry


class PublicProjection(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='public-cut-')
        self.addCleanup(self.directory.cleanup)
        self.repo = Path(self.directory.name)
        self.command('init', '-q')
        self.command('config', 'user.name', 'Projection fixture')
        self.command('config', 'user.email', '')
        self.write('public.txt', 'public\n')
        self.command('add', '.')
        self.command('commit', '-qm', 'public parent')
        self.parent = self.command('rev-parse', 'HEAD')
        self.ledger = {'entry': {'label': 'internal', 'amount': 7}, 'proofs': ['sha256:abc']}
        self.write(cut.LEDGER, json.dumps(self.ledger))
        self.write('private/receipt.txt', 'private fixture\n')
        self.command('add', '.')
        self.command('commit', '-qm', 'source')
        self.source = self.command('rev-parse', 'HEAD')
        self.command('commit', '--allow-empty', '-qm', 'trunk authority')
        self.anchor = self.command('rev-parse', 'HEAD')
        projected = copy.deepcopy(self.ledger)
        projected['entry']['label'] = 'public'
        self.candidate = self.project({cut.LEDGER: json.dumps(projected)},
                                      remove=['private/receipt.txt'])
        before, after = cut.tree(self.repo, self.source), cut.tree(self.repo, self.candidate)
        self.rule = dict(version=1, trunk_anchor=self.anchor,
                         source=dict(commit=self.source, tree=self.tree(self.source)),
                         public=dict(commit=self.candidate, tree=self.tree(self.candidate)),
                         public_parent=self.parent, exclusions=['private/**'], paths={},
                         ledger={'/entry/label': dict(before=cut.sha('internal'),
                                  after=cut.sha('public'), reason='public label')})
        for path in before.keys() | after.keys():
            if before.get(path) == after.get(path):
                continue
            row = dict(before=before.get(path), after=after.get(path), reason='fixture rule')
            if row['after'] is None:
                row['exclusion'] = 'private/**'
            self.rule['paths'][path] = row

    def command(self, *args, data=None, env=None):
        return subprocess.check_output(['git', '-C', str(self.repo), *args],
                                       input=data, env=env).decode().strip()

    def write(self, path, data):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(data)

    def tree(self, revision):
        return self.command('rev-parse', revision + '^{tree}')

    def project(self, changes, remove=(), base=None, parent=None, mode='100644'):
        # Separate index: mutations are committed objects, never dirty worktrees.
        env = dict(os.environ, GIT_INDEX_FILE=str(self.repo / 'projection-index'))
        self.command('read-tree', base or self.source, env=env)
        for path in remove:
            self.command('update-index', '--force-remove', path, env=env)
        for path, data in changes.items():
            blob = self.command('hash-object', '-w', '--stdin', data=data.encode())
            self.command('update-index', '--add', '--cacheinfo', mode, blob, path, env=env)
        tree = self.command('write-tree', env=env)
        return self.command('commit-tree', tree, '-p', parent or self.parent,
                            data=b'projection fixture\n')

    def verify(self, candidate=None, rule=None):
        return cut.verify(self.repo, rule or self.rule, candidate or self.candidate, self.anchor)

    def test_exact_projection_passes(self):
        self.assertEqual(self.verify()['ledger_leaves'], ['/entry/label'])

    def test_cli_requires_an_explicit_external_rule(self):
        command = [sys.executable, '-B', str(Path(cut.__file__).resolve()),
                   '--repo', str(self.repo), '--cut', self.candidate, '--trunk', self.anchor]
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('the following arguments are required: --rule', result.stderr)
        rule_path = self.repo / 'reviewed-rule.json'
        rule_path.write_text(json.dumps(self.rule))
        result = subprocess.run(command + ['--rule', str(rule_path)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), self.verify())

    def test_unknown_addition_deletion_and_blob_substitution_are_red(self):
        variants = [self.project({'extra.txt': 'extra'}, base=self.candidate),
                    self.project({}, ['public.txt'], base=self.candidate),
                    self.project({'public.txt': 'changed'}, base=self.candidate)]
        for candidate in variants:
            with self.subTest(candidate=candidate), self.assertRaises(cut.ProjectionError):
                self.verify(candidate)

    def test_mode_change_is_red(self):
        candidate = self.project({'public.txt': 'public\n'}, base=self.candidate, mode='100755')
        with self.assertRaises(cut.ProjectionError):
            self.verify(candidate)

    def test_ledger_extra_field_change_is_red_even_with_resealed_tree_and_blob(self):
        ledger = copy.deepcopy(self.ledger)
        ledger['entry'].update(label='public', amount=8)
        candidate = self.project({cut.LEDGER: json.dumps(ledger)}, base=self.candidate)
        rule = copy.deepcopy(self.rule)
        rule['public'] = dict(commit=candidate, tree=self.tree(candidate))
        rule['paths'][cut.LEDGER]['after'] = cut.tree(self.repo, candidate)[cut.LEDGER]
        with self.assertRaisesRegex(cut.ProjectionError, 'ledger difference'):
            self.verify(candidate, rule)

    def test_ledger_added_key_list_member_and_type_change_are_red(self):
        variants = [dict(self.ledger, extra=1), dict(self.ledger, proofs=['sha256:abc', 'x']),
                    dict(self.ledger, entry={'label': 'public', 'amount': True})]
        for ledger in variants:
            candidate = self.project({cut.LEDGER: json.dumps(ledger)}, base=self.candidate)
            with self.subTest(ledger=ledger), self.assertRaises(cut.ProjectionError):
                self.verify(candidate)

    def test_source_tree_and_provenance_are_bound(self):
        for field, value in [('tree', self.tree(self.parent)), ('commit', self.candidate)]:
            rule = copy.deepcopy(self.rule)
            rule['source'][field] = value
            with self.subTest(field=field), self.assertRaises(cut.ProjectionError):
                self.verify(rule=rule)

    def test_public_parent_is_not_inferred_from_objects(self):
        candidate = self.project({}, base=self.candidate, parent=self.source)
        with self.assertRaisesRegex(cut.ProjectionError, 'public parent'):
            self.verify(candidate)

    def test_exclusions_cannot_authorize_unlisted_deletions(self):
        rule = copy.deepcopy(self.rule)
        rule['exclusions'] = ['another/**']
        with self.assertRaisesRegex(cut.ProjectionError, 'exclusion'):
            self.verify(rule=rule)

    def test_stale_ledger_exception_is_not_optional(self):
        rule = copy.deepcopy(self.rule)
        rule['ledger']['/entry/amount'] = dict(before=cut.sha(7), after=cut.sha(8), reason='extra')
        with self.assertRaisesRegex(cut.ProjectionError, 'ledger difference'):
            self.verify(rule=rule)

    def test_duplicate_rule_keys_are_rejected(self):
        with self.assertRaisesRegex(cut.ProjectionError, 'duplicate JSON key'):
            cut.decode('{"version": 1, "version": 2}')

    def test_section11_cannot_skip_any_of_its_three_authorities(self):
        rule_path = self.repo / 'rule.json'
        rule_path.write_text(json.dumps(self.rule))
        native = {'darwin': 'macos', 'linux': 'linux', 'win32': 'windows'}[registry.sys.platform]
        for fail_at in range(4):
            output = self.repo / ('receipt-' + str(fail_at))
            args = SimpleNamespace(cut_repo=self.repo, source_repo=self.repo,
                                   cut_rule=rule_path, cut_output=output,
                                   platform=native, trunk=self.anchor)
            results = [SimpleNamespace(returncode=int(i == fail_at)) for i in range(3)]
            with patch.object(registry, 'clean'), patch.object(registry, 'verify', return_value={}), \
                    patch.object(registry.subprocess, 'run', side_effect=results) as run:
                if fail_at < 3:
                    with self.assertRaises(cut.ProjectionError):
                        registry.replay(args)
                    self.assertEqual(run.call_count, fail_at + 1)
                else:
                    self.assertEqual(registry.replay(args), 0)
                    self.assertEqual(run.call_count, 3)
            receipt = json.loads((output / 'section-11.json').read_text())
            self.assertEqual(receipt['status'], 'PASS' if fail_at == 3 else 'FAIL')


if __name__ == '__main__':
    unittest.main()
