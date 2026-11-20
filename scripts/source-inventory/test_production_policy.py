"""D65 acceptance canaries: adopted blacklist and measured production envelopes."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from compare import compare
from inventory import scan
from model import canonical, read_tree
from production_policy import check_ratchets


class MarkdownDecisionTests(unittest.TestCase):
    GUIDE = 'docs/migration/0.10-to-0.11.md'
    POLICY = 'scripts/source-inventory/markdown-decisions.json'

    def setUp(self):
        audit = Path(os.environ.get('SOURCE_AUDIT', '.rmux-audit/d88-fixtures')).resolve()
        audit.mkdir(parents=True, exist_ok=True)
        self.repo = Path(tempfile.mkdtemp(dir=audit))
        self.git('init', '-q')
        self.git('config', 'user.name', 'Markdown fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('marker', 'tracked\n')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args],
                                       stderr=subprocess.PIPE, text=True).strip()

    def write(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def decisions(self, paths):
        self.write(self.POLICY, canonical({'schema': 1, 'paths': paths}))

    def report(self, production=()):
        self.git('add', '.')
        self.git('commit', '--allow-empty', '-qm', 'Markdown fixture')
        revision = self.git('rev-parse', 'HEAD')
        tree = read_tree(self.repo, revision)
        policy = dict(version=1, files={'tooling': list(tree)}, documents={},
                      provenance={}, includes={}, rust_roots={}, crate_roles={})
        if production:
            policy['files']['production'] = list(production)
            policy['files']['tooling'] = [path for path in tree if path not in production]
        for path, row in tree.items():
            if path.endswith('.md'):
                policy['documents'][path] = dict(role='guide', source=[path],
                    consumers=['maintainers'], owner='maintainer', freshness='exact source',
                    basis='maintained declaration', source_blobs=[row['blob']])
        report = scan(self.repo, revision, Path('/unused'), policy,
                      canonical(policy).encode(), parsed={})
        self.assertEqual(report['errors'], [])
        return report

    def cli(self, before, after, expected):
        directory = Path(tempfile.mkdtemp(dir=self.repo / '.git'))
        for name, report in (('before', before), ('after', after)):
            (directory / (name + '.json')).write_text(canonical(report))
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('compare.py')),
            '--repo', str(self.repo), '--before', str(directory / 'before.json'),
            '--after', str(directory / 'after.json'), '--output', str(directory / 'delta.json'),
            '--check-ratchets'], capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stderr)

    def test_guide_rejected_without_list_and_accepted_with_decision(self):
        before = self.report()
        self.write(self.GUIDE, '# Migration\n')
        after = self.report()
        with self.assertRaisesRegex(ValueError, 'ratchet RED'):
            check_ratchets(compare(self.repo, before, after))
        self.cli(before, after, 1)
        self.decisions({self.GUIDE: 'D77'})
        after = self.report()
        check_ratchets(compare(self.repo, before, after))
        self.cli(before, after, 0)

    def test_unlisted_markdown_cannot_be_offset_by_a_deletion(self):
        self.write('old.md', '# Old\n')
        before = self.report()
        (self.repo / 'old.md').unlink()
        self.write('other.md', '# Unrequested\n')
        self.decisions({})
        after = self.report()
        with self.assertRaisesRegex(ValueError, 'ratchet RED'):
            check_ratchets(compare(self.repo, before, after))
        self.cli(before, after, 1)

    def test_decision_for_absent_path_is_rejected(self):
        before = self.report()
        self.decisions({self.GUIDE: 'D77'})
        after = self.report()
        with self.assertRaisesRegex(ValueError, 'absent.*' + self.GUIDE):
            compare(self.repo, before, after)
        self.cli(before, after, 1)

    def test_decided_guide_does_not_hide_other_ratchets(self):
        self.write('src/runtime.txt', '// production\n' * 600)
        before = self.report(production=['src/runtime.txt'])
        self.write(self.GUIDE, '# Migration\n')
        self.decisions({self.GUIDE: 'D77'})
        self.write('src/runtime.txt', '// production\n' * 600 + '#[path = "x"] #[allow(dead_code)]\n')
        after = self.report(production=['src/runtime.txt'])
        delta = compare(self.repo, before, after)
        self.assertEqual(delta['literal_ratchet_increases'], ['path_literal', 'allow_dead_code_literal'])
        self.assertEqual(delta['production_size_violations'], [dict(
            path='src/runtime.txt', reason='existing_oversize_growth', before=600, after=601)])
        with self.assertRaisesRegex(ValueError, 'ratchet RED'):
            check_ratchets(delta)
        self.cli(before, after, 1)

    def test_invalid_decision_lists_fail_closed(self):
        self.write(self.GUIDE, '# Migration\n')
        before = self.report()
        for value in ({'schema': 2, 'paths': {}}, {'schema': 1, 'paths': []},
                      {'schema': 1, 'paths': {self.GUIDE: ''}},
                      {'schema': 1, 'paths': {'docs/../guide.md': 'D77'}}):
            with self.subTest(policy=value):
                self.write(self.POLICY, canonical(value))
                with self.assertRaisesRegex(ValueError, 'Markdown'):
                    compare(self.repo, before, self.report())

    def test_versioned_list_names_only_the_three_decided_paths(self):
        policy = json.loads(Path(__file__).with_name('markdown-decisions.json').read_text())
        self.assertEqual(policy, {'schema': 1, 'paths': {
            self.GUIDE: 'D77', 'docs/release/0.11.0.md': 'D77',
            'docs/release/publication-runbook.md': 'D81'}})


class ProductionPolicyTests(unittest.TestCase):
    def test_d65_blacklist_and_production_ratchet(self):
        import production_policy as gate
        for token in ('dispatch_id', 'DISPATCH-ID', 'dispatch_id_x',
                      'fleet', 'orchestration', 'agent_role', 'agent-session', 'build_slot'):
            with self.subTest(positive=token):
                self.assertTrue(gate.blacklist_matches(token))
        self.assertEqual(gate.blacklist_matches('dispatch_identity'), [])
        self.assertEqual(gate.check_envelope(0, {}), 0)
        self.assertEqual(gate.check_envelope(-1, {}), 0)
        for delta, mandate in ((1, {}), (2, {'production_growth_envelope': 1}),
                              (1, {'production_growth_envelope': 1}),
                              (1001, {'production_growth_envelope': 1001,
                                      'production_growth_justification': 'bounded addition'})):
            with self.subTest(delta=delta, mandate=mandate):
                with self.assertRaises(ValueError):
                    gate.check_envelope(delta, mandate)
        self.assertEqual(gate.check_envelope(1000, {
            'production_growth_envelope': 1000,
            'production_growth_justification': 'bounded addition'}), 1000)
        self.assertEqual(gate.check_envelope(1001, {
            'production_growth_envelope': 1001,
            'production_growth_justification': 'bounded addition',
            'production_growth_decision': 'D66 sponsor approval'}), 1001)
        gate.check_freeze(343521)
        with self.assertRaises(ValueError):
            gate.check_freeze(343522)
        reference = json.loads(Path(gate.__file__).with_name('production-reference.json').read_text())
        self.assertEqual(reference['production_lines'], 343521)
        self.assertEqual(reference['revision'], 'ea435dddd3fba0e2c9e8e7c8d97150b0712ed21b')
        self.assertTrue(reference['tool_inputs_sha256'])
        self.assertTrue(reference['parser_inputs_sha256'])
        ledger = json.loads(Path(gate.__file__).with_name('production-ledger.json').read_text())
        self.assertEqual(ledger['entries'][0]['lot'], 'INT145')
        self.assertEqual(ledger['entries'][0]['delta'], 0)

    def test_published_word_list_is_explicitly_empty_and_generic_guard_stays_red(self):
        import production_policy as gate
        # No private word is published in any form, not even as a digest.
        self.assertIs(type(gate.TOKENS), tuple)
        self.assertEqual(gate.TOKENS, ())
        policy = json.loads(Path(gate.__file__).with_name('blacklist-policy.json').read_text())
        self.assertIn('token_sha256', policy)
        self.assertEqual(policy['token_sha256'], [])
        self.assertIs(policy['adopted'], True)
        self.assertEqual(policy['generic_pattern'], gate.GENERIC)
        for text in ('FLEET-x', 'reorchestrate', 'Orchestrator'):
            with self.subTest(positive=text):
                self.assertEqual(len(gate.blacklist_matches(text)), 1)
        for text in ('flee', 'orchestra', 'Fle-et', ''):
            with self.subTest(negative=text):
                self.assertEqual(gate.blacklist_matches(text), [])
        report = {'files': [dict(path='src/lib.rs', counts={'production': 2}, regions=[
            dict(start=1, end=2, **{'class': 'production'})])]}
        tree = {'src/lib.rs': {'data': b'pub fn plain() {}\n// FLEET\n'}}
        self.assertEqual(gate.scan_blacklist(report, tree),
                         [{'path': 'src/lib.rs', 'line': 2, 'token': 'FLEET'}])

    # Neutral stand-ins for a private list read outside the published tree: a
    # word that is the prefix of another, shared prefixes, a space, separators
    # and digits.
    NEUTRAL_WORDS = ('Delta', 'Delta7', 'Echo', 'Foxtrot-', 'Golf Hotel', 'INDIA_JULIETT',
                     'India_J', 'Kilo9')

    def test_digest_matcher_equals_the_former_case_insensitive_alternation(self):
        import random
        import re
        import production_policy as gate
        words = self.NEUTRAL_WORDS
        tokens = tuple((gate.fold(word[:2]), len(word), gate.token_digest(word)) for word in words)
        matcher = gate.compile_matcher(tokens)
        former = re.compile(gate.GENERIC + '|' + '|'.join(re.escape(word) for word in words))
        pieces = (*words, 'fleet', 'orchestrat', 'dispatch_id', 'dispatch_identity', 'agent-role',
                  'build_slot', 'x', '_', ' ', '-', '3', '7', '\u017f', '\u212a', '\u0131', '\u0130',
                  '\u00df', 'Delt', 'Kil', 'INDIA_')
        variants = (str, str.upper, str.lower, str.swapcase,
                    lambda s: s.replace('s', '\u017f').replace('k', '\u212a').replace('i', '\u0131'),
                    lambda s: s.replace('i', '\u0130').replace('K', '\u212a'))
        rng = random.Random(65)
        samples = [variant(piece) for piece in pieces for variant in variants]
        for _ in range(3000):
            samples.append(''.join(rng.choice(variants)(rng.choice(pieces))
                                   for _ in range(rng.randint(1, 6))))
        for sample in samples:
            with self.subTest(sample=sample):
                self.assertEqual(matcher(sample), [m.group() for m in former.finditer(sample)])

    def test_fold_table_is_exactly_the_ignorecase_ascii_equivalence(self):
        import re
        import production_policy as gate
        everything = ''.join(chr(c) for c in range(0x110000) if not 0xd800 <= c <= 0xdfff)
        equated = {char for char in re.findall('[a-z0-9 _-]', everything, re.IGNORECASE)
                   if not char.isascii()}
        self.assertEqual(set(gate.FOLD), equated)
        for char, target in gate.FOLD.items():
            self.assertTrue(re.fullmatch(re.escape(target), char, re.IGNORECASE))

    def test_policy_file_and_gate_share_the_published_digests(self):
        import production_policy as gate
        policy = json.loads(Path(gate.__file__).with_name('blacklist-policy.json').read_text())
        self.assertNotIn('tokens', policy)
        self.assertEqual(set(policy), {'schema', 'authority', 'adopted', 'token_sha256',
                                       'generic_pattern', 'scope'})
        self.assertEqual(policy['token_sha256'], [list(token) for token in gate.TOKENS])
        self.assertEqual(policy['generic_pattern'], gate.GENERIC)

    def test_blacklist_preserves_manifest_asset_and_production_scope(self):
        import production_policy as gate
        population = (
            ('src/lib.rs', 'production', 'dispatch_id'),
            ('resources/public.js', 'assets', 'fleet'),
            ('Cargo.toml', 'tooling', 'agent_role'),
            ('crates/core/Cargo.toml', 'tooling', 'build-slot'),
            ('crates/rmux-server/src/handler_overlay/parse_environment_tests.rs',
             'production', 'orchestration'),
            ('tests/only.rs', 'tests', 'orchestration'),
            ('scripts/standalone/Cargo.toml', 'tooling', 'orchestration'),
        )
        report = {'files': [dict(path=path, counts={role: 1},
                                regions=[dict(start=1, end=1, **{'class': role})])
                            for path, role, token in population]}
        tree = {path: {'data': token.encode()} for path, role, token in population}
        self.assertEqual({hit['path'] for hit in gate.scan_blacklist(report, tree)},
                         {row[0] for row in population[:5]})

    def test_parser_qualification_uses_attested_root_for_both_cargo_layouts(self):
        import sys
        import types
        import uuid
        from unittest.mock import patch
        import production_gate as gate
        repo = Path(__file__).resolve().parents[2]
        target = repo / '.rmux-audit' / ('d65-unit-' + uuid.uuid4().hex)
        target.mkdir(parents=True)
        (target / 'identity-compilation-unit.json').write_text('{}')

        def verified(binary, source, head, actual_target):
            self.assertEqual(actual_target, target)
            return {'qualified': True}

        binding = types.SimpleNamespace(binding=lambda *args: {
            'manifest': str(gate.TOOL / 'Cargo.toml')})
        identity = types.SimpleNamespace(verify_built=verified, support=lambda name: binding)
        with patch.dict(sys.modules, {'candidate_identity': identity}):
            for layout in ('debug', 'native-triple/debug'):
                binary = target / layout / 'rmux-source-syntax'
                binary.parent.mkdir(parents=True)
                binary.write_text('fixture')
                self.assertEqual(gate.qualified_parser(repo, 'f' * 40, binary), {'qualified': True})
