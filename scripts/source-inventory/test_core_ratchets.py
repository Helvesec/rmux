"""Keep the CORE correction bound to the original twelve-path population."""
import json
import os
from pathlib import Path
import tempfile
import unittest

from compare import compare
from inventory import scan
from model import canonical, git, read_tree
from relocation_chain import EPOCH, core_after_paths


BASELINE = 'f54ae83954c368c5c4e3706806690e0388c5414e'
BOUNDS = {
    'crates/rmux-core/src/input/mod.rs': 811,
    'crates/rmux-core/src/screen.rs': 1293,
    'crates/rmux-core/src/screen/writer.rs': 720,
    'crates/rmux-server/src/handler.rs': 1050,
    'crates/rmux-server/src/handler_attach.rs': 1934,
    'crates/rmux-server/src/handler_client_runtime.rs': 703,
    'crates/rmux-server/src/handler_overlay.rs': 1261,
    'crates/rmux-server/src/handler_pane/attached_input.rs': 1309,
    'crates/rmux-server/src/pane_transcript.rs': 1195,
    'crates/rmux-server/src/renderer/pane_delta.rs': 981,
    'crates/rmux-server/src/terminal.rs': 891,
    'src/cli/dispatch.rs': 862,
}
CORE = tuple(list(BOUNDS)[:3])
LAYOUT = 'crates/rmux-core/src/session/layout_cycle.rs'


class CoreRatchetTests(unittest.TestCase):
    def test_popup_fixture_classification_keeps_unknown_sibling_rejected(self):
        repo = Path(__file__).resolve().parents[2]
        raw = (repo / 'scripts/source-inventory/declarations.json').read_bytes()
        policy = json.loads(raw)
        paths = ('tests/popup_reference.py', 'tests/popup_reference_darwin.py',
                 'tests/popup_reference_windows.py', 'tests/popup_status_reference.py')
        unknown = 'tests/popup_status_unknown.py'
        audit = repo / '.rmux-audit/test-fixtures'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            git(fixture, 'init', '-q')
            for path in (*paths, unknown):
                target = fixture / path
                target.parent.mkdir(parents=True, exist_ok=True)
                # Classify actual fixture bytes without importing native effects.
                target.write_bytes((repo / path).read_bytes() if path != unknown
                                   else b'# unknown sibling must stay unclassified\n')
            git(fixture, 'add', 'tests')
            git(fixture, '-c', 'user.name=Sidney Sissaoui',
                '-c', 'user.email=shideneyu@gmail.com', 'commit', '-qm', 'fixture')
            revision = git(fixture, 'rev-parse', 'HEAD').decode().strip()
            # This Git fixture has no Rust: its exact parser population is empty.
            report = scan(fixture, revision, None, policy, canonical(policy).encode(),
                          parsed={})
        self.assertEqual(report['errors'], ['unclassified file: ' + unknown])
        rows = {row['path']: row for row in report['files']}
        for path in paths:
            with self.subTest(path=path):
                self.assertEqual(rows[path]['counts'],
                                 {'tests': rows[path]['physical_lines']})
        self.assertEqual(rows[unknown]['counts'], {})

    def test_core_growth_and_semantic_path_are_removed(self):
        repo = Path(__file__).resolve().parents[2]
        revision = os.environ.get('SOURCE_RATCHET_REVISION') or git(
            repo, 'rev-parse', 'HEAD').decode().strip()
        raw = read_tree(repo, revision)[
            'scripts/source-inventory/declarations.json']['data']
        parser = Path(os.environ['SOURCE_SYNTAX']).resolve()
        reports = [scan(repo, sha, parser, json.loads(raw), raw)
                   for sha in (BASELINE, revision)]
        for report in reports:
            self.assertEqual(report['errors'], [])
        before, after = ({row['path']: row for row in report['files']}
                         for report in reports)
        addresses = core_after_paths(repo, revision, BOUNDS, CORE, BASELINE, EPOCH)
        self.assertEqual(len(set(addresses.values())), 12)
        self.assertEqual(len(BOUNDS), 12)
        for path, bound in BOUNDS.items():
            with self.subTest(population=path):
                self.assertIn(addresses[path], after)
                self.assertEqual(before[path]['counts']['production'], bound)
                self.assertGreater(after[addresses[path]]['counts']['production'], 0)
        for path in CORE:
            with self.subTest(core=path):
                self.assertLessEqual(after[addresses[path]]['counts']['production'], BOUNDS[path])
        delta = compare(repo, *reports, epoch=EPOCH)
        self.assertEqual([row for row in delta['semantic_attribute_additions']
                          if row['path'] == LAYOUT], [])
        self.assertEqual([row for row in delta['production_size_violations']
                          if row['path'] in CORE], [])
        tree = read_tree(repo, revision)
        self.assertIn(b'mod resize;', tree[LAYOUT]['data'])
        self.assertIn('crates/rmux-core/src/session/layout_cycle/resize.rs', tree)
