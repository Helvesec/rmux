"""Git-backed provenance and activation boundaries for explicit consumers."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import consumer_declarations as declarations
from model import canonical, git
from relocation_chain import add_consumer, semantic_key


class ConsumerDeclarationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Retain small fixtures in the ignored audit directory for diagnosis.
        audit = Path(__file__).resolve().parents[2] / '.rmux-audit/consumer-declarations'
        audit.mkdir(parents=True, exist_ok=True)
        cls.repo = Path(tempfile.mkdtemp(dir=audit))
        git(cls.repo, 'init', '-q')
        git(cls.repo, 'config', 'user.name', 'Sidney Sissaoui')
        git(cls.repo, 'config', 'user.email', 'shideneyu@gmail.com')
        for name, text in {
            'Cargo.toml': '[package]\nname="fixture"\nversion="0.1.0"\nedition="2021"\n',
            'src/lib.rs': '',
            'tests/common/mod.rs': '#![allow(dead_code)]\npub fn helper() {}\n',
            'scripts/source-inventory/declarations.json': json.dumps({'rust_roots': {}, 'crate_roles': {}}),
        }.items():
            path = cls.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        cls.base = cls.commit('base')
        (cls.repo / 'tests/new.rs').write_text('#![cfg(unix)]\nmod common;\n#[test] fn test() {}\n')
        cls.intro = cls.commit('introduce test consumer')
        (cls.repo / 'src/lib.rs').write_text('// neutral\n')
        cls.head = cls.commit('neutral descendant')
        cls.row = dict(path='tests/common/mod.rs', module_dir='tests/common', condition='unix',
                       consumer='tests/new.rs:mod', introduced_commit=cls.intro, reason='new test')
        cls.file = cls.repo / 'consumer-additions.jsonl'

    @classmethod
    def commit(cls, message):
        git(cls.repo, 'add', '.')
        git(cls.repo, 'commit', '-qm', message)
        return git(cls.repo, 'rev-parse', 'HEAD').decode().strip()

    def activate(self, revision, **changes):
        row = dict(self.row, **changes)
        self.file.write_text(canonical(row) + '\n')
        with patch.object(declarations, 'DECLARATIONS', self.file):
            return declarations.declared_consumers(self.repo, revision)

    def test_real_introduction_and_descendant(self):
        self.assertEqual(self.activate(self.intro), [self.row])
        self.assertEqual(self.activate(self.head), [self.row])

    def test_historical_and_non_descendant_not_activated(self):
        self.assertEqual(self.activate(self.base), [])
        tree = git(self.repo, 'rev-parse', self.base + '^{tree}').decode().strip()
        sibling = git(self.repo, 'commit-tree', tree, '-p', self.base, '-m', 'sibling').decode().strip()
        self.assertEqual(self.activate(sibling), [])

    def test_unknown_commit_refuses(self):
        with self.assertRaisesRegex(ValueError, 'exact commit'):
            self.activate(self.head, introduced_commit='0' * 40)

    def test_wrong_existing_commit_refuses(self):
        with self.assertRaisesRegex(ValueError, 'does not add'):
            self.activate(self.head, introduced_commit=self.head)

    def test_non_ancestor_declaration_refuses(self):
        tree = git(self.repo, 'rev-parse', self.intro + '^{tree}').decode().strip()
        sibling = git(self.repo, 'commit-tree', tree, '-p', self.base, '-m', 'foreign intro').decode().strip()
        with self.assertRaisesRegex(ValueError, 'not an ancestor'):
            self.activate(self.head, introduced_commit=sibling)

    def test_wrong_provenance_refuses(self):
        for change in ({'condition': True}, {'module_dir': 'tests/other'},
                       {'path': 'src/lib.rs'}):
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, 'provenance differs'):
                self.activate(self.head, **change)

    def test_malformed_and_duplicate_declarations_refuse(self):
        for change in ({'introduced_commit': self.intro[:8]}, {'reason': ''},
                       {'consumer': '../new.rs:mod'}, {'extra': True}, {'condition': []}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                declarations.parse_declarations(canonical(dict(self.row, **change)).encode())
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            declarations.parse_declarations((canonical(self.row) + '\n') .encode() * 2)

    def test_only_declared_identity_added(self):
        from collections import Counter
        attribute = dict(kind='allow_dead_code', value='dead_code', owner=['file'],
                         condition=True, classification='tests')
        groups = [dict(module_dir='tests/common', condition='unix', consumers=['tests/old.rs:mod'])]
        original = Counter({semantic_key(self.row['path'], attribute, groups): 1})
        admitted = add_consumer(original, self.row)
        self.assertNotEqual(admitted, original)  # withdrawal of the declared consumer still differs
        added = json.loads(next(iter(admitted)))[2][0]['consumers']
        self.assertEqual(added, ['tests/new.rs:mod', 'tests/old.rs:mod'])
        with self.assertRaises(ValueError):
            add_consumer(admitted, self.row)
        undeclared = add_consumer(admitted, dict(self.row, consumer='tests/undeclared.rs:mod'))
        self.assertNotEqual(admitted, undeclared)


if __name__ == '__main__':
    unittest.main()
