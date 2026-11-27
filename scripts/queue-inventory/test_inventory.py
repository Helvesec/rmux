"""Regression guards for inventory completeness, capacities and native selection."""
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from check_inventory import current_inventory, validate_gate_links
from run_references import execute
from source_scan import scan_sources, test_symbols
from validation import numeric_capacity, validate_inventory


PATH = 'crates/example/src/lib.rs'
TEXT = '''fn queue() { let (tx, rx) = tokio::sync::mpsc::channel::<u8>(17); }
#[test] fn queue_rejects_overflow() {}
'''


def fixture():
    sources = {PATH: TEXT}
    sites = scan_sources(sources)
    anchor = {'path': PATH, 'snippet': 'tokio::sync::mpsc::channel::<u8>(17)'}
    profile = {
        'owner': 'queue', 'producer': 'tx', 'consumer': 'rx',
        'capacity': {
            'elements': {'kind': 'fixed', 'value': 17, 'description': '17 records',
                         'evidence': [anchor]},
            'bytes': {'kind': 'not_bounded', 'description': 'No aggregate byte bound',
                      'evidence': [anchor]},
        },
        'saturation': 'send awaits capacity', 'closure': 'drop closes channel',
        'evidence': [anchor], 'tests': [
            {'path': PATH, 'symbol': 'queue_rejects_overflow', 'package': 'example',
             'target': 'lib', 'platforms': ['linux', 'macos', 'windows']},
        ],
    }
    inventory = {'schema': 1, 'entries': [{'site': sites[0], 'profile': 'queue'}],
                 'exemptions': [], 'profiles': {'queue': profile}}
    return inventory, sites, sources


STD_PATHS = ('mpsc', 'std::sync::mpsc', 'std_mpsc')
TYPES = ('PtyChild', 'Vec<u8>', '(u8, u8)', 'Option<Vec<u8>>', '&\'static [u8]', 'fn(u8) -> u8')


class ChannelGuards(unittest.TestCase):
    def exemption(self, expression):
        sources = {PATH: f'fn queue() {{ let (tx, rx) = {expression}; }}'}
        sites = scan_sources(sources)
        self.assertEqual(len(sites), 1, expression)
        self.assertEqual(sites[0]['kind'], 'channel')
        self.assertEqual(sites[0]['expression'], expression)
        self.assertFalse(sites[0]['test_only'])
        row = {'site': sites[0], 'category': 'unbounded',
               'reason': 'primitive has no capacity',
               'evidence': [{'path': PATH, 'snippet': expression}]}
        return {'schema': 1, 'entries': [], 'exemptions': [row], 'profiles': {}}, sites, sources

    def test_std_generic_unbounded_channels(self):
        for prefix in STD_PATHS:
            for element in TYPES:
                expression = f'{prefix}::channel::<{element}>()'
                with self.subTest(expression=expression):
                    self.assertEqual(validate_inventory(*self.exemption(expression))['exemptions'], 1)

    def test_std_unbounded_channel_spacing(self):
        for expression in (
            'mpsc :: channel :: < PtyChild > ( )',
            'std :: sync :: mpsc :: channel::< Vec < u8 > > ()',
            'std_mpsc::channel\n::<(u8, u8)>\n(\n)',
            'mpsc :: channel ( )',
        ):
            with self.subTest(expression=expression):
                self.assertEqual(validate_inventory(*self.exemption(expression))['exemptions'], 1)

    def test_existing_unbounded_channels(self):
        for expression in (
            *(f'{prefix}::channel()' for prefix in STD_PATHS),
            'mpsc::unbounded_channel()',
            'tokio::sync::mpsc::unbounded_channel::<Vec<u8>>()',
        ):
            with self.subTest(expression=expression):
                self.assertEqual(validate_inventory(*self.exemption(expression))['exemptions'], 1)

    def test_bounded_primitives_stay_bounded(self):
        for constructor, argument in (
            *((f'{prefix}::sync_channel', '17') for prefix in STD_PATHS),
            ('tokio::sync::mpsc::channel', '17'),
            ('mpsc::channel', '17'),
            ('broadcast::channel', '17'),
            ('watch::channel', '17'),
            ('oneshot::channel', ''),
            ('crossbeam_channel::bounded', '17'),
        ):
            for generic in ('', '::<Vec<u8>>'):
                expression = f'{constructor}{generic}({argument})'
                for spelling in (expression, expression.replace('::', ' :: ').replace('(', ' ( ')):
                    with self.subTest(expression=spelling):
                        with self.assertRaisesRegex(ValueError, 'bounded primitive exempted as unbounded'):
                            validate_inventory(*self.exemption(spelling))

    def test_generic_std_channel_still_requires_row_and_evidence(self):
        inventory, sites, sources = self.exemption('mpsc::channel::<PtyChild>()')
        inventory['exemptions'][0]['evidence'] = []
        with self.assertRaisesRegex(ValueError, 'production exemption needs source evidence'):
            validate_inventory(inventory, sites, sources)
        inventory['exemptions'].clear()
        with self.assertRaisesRegex(ValueError, 'missing creations'):
            validate_inventory(inventory, sites, sources)

    def test_bounded_generic_capacity_still_checked(self):
        for constructor in (*(f'{prefix}::sync_channel' for prefix in STD_PATHS),
                            'tokio::sync::mpsc::channel', 'crossbeam_channel::bounded'):
            with self.subTest(constructor=constructor):
                inventory, _, sources = fixture()
                sources[PATH] = sources[PATH].replace('tokio::sync::mpsc::channel', constructor)
                sites = scan_sources(sources)
                inventory['entries'][0]['site'] = sites[0]
                profile = inventory['profiles']['queue']
                # fixture shares this anchor between policy and both capacity units.
                profile['evidence'][0]['snippet'] = f'{constructor}::<u8>(17)'
                self.assertEqual(validate_inventory(inventory, sites, sources)['bounded_entries'], 1)
                profile['capacity']['elements']['value'] = 18
                with self.assertRaisesRegex(ValueError, 'capacity differs'):
                    validate_inventory(inventory, sites, sources)


class InventoryGuards(unittest.TestCase):
    def setUp(self):
        self.inventory, self.sites, self.sources = fixture()

    def validate(self):
        return validate_inventory(self.inventory, self.sites, self.sources)

    def test_valid_current_primitive(self):
        self.assertEqual(self.validate()['bounded_entries'], 1)

    def test_missing_production_creation_is_rejected(self):
        self.inventory['entries'].clear()
        with self.assertRaisesRegex(ValueError, 'missing creations'):
            self.validate()

    def test_false_fixed_capacity_is_rejected(self):
        self.inventory['profiles']['queue']['capacity']['elements']['value'] = 18
        with self.assertRaisesRegex(ValueError, 'capacity differs'):
            self.validate()

    def test_unresolved_fixed_capacity_is_rejected(self):
        self.sources[PATH] = TEXT.replace('(17)', '(missing::CAP)')
        self.sites = scan_sources(self.sources)
        self.inventory['entries'][0]['site'] = self.sites[0]
        anchor = {'path': PATH, 'snippet': 'missing::CAP'}
        p = self.inventory['profiles']['queue']
        p['evidence'] = [anchor]
        for bound in p['capacity'].values():
            bound['evidence'] = [anchor]
        with self.assertRaisesRegex(ValueError, 'cannot resolve fixed capacity'):
            self.validate()

    def test_production_cannot_be_exempted_as_test(self):
        row = self.inventory['entries'].pop()
        row.update(category='test_only', reason='wrong')
        self.inventory['exemptions'].append(row)
        with self.assertRaisesRegex(ValueError, 'production site exempted as test'):
            self.validate()

    def test_bounded_channel_cannot_be_exempted_as_unbounded(self):
        row = self.inventory['entries'].pop()
        row.update(category='unbounded', reason='wrong')
        self.inventory['exemptions'].append(row)
        with self.assertRaisesRegex(ValueError, 'bounded primitive exempted as unbounded'):
            self.validate()

    def test_duplicate_creation_is_rejected(self):
        self.inventory['entries'] *= 2
        with self.assertRaisesRegex(ValueError, 'duplicate creation'):
            self.validate()

    def test_stale_anchor_is_rejected(self):
        self.inventory['profiles']['queue']['evidence'] = [
            {'path': PATH, 'snippet': 'does_not_exist()'}]
        with self.assertRaisesRegex(ValueError, 'evidence anchor absent'):
            self.validate()

    def test_missing_annotated_test_is_rejected(self):
        self.inventory['profiles']['queue']['tests'][0]['symbol'] = 'absent'
        with self.assertRaisesRegex(ValueError, 'annotated test definition absent'):
            self.validate()

    def test_wrong_cargo_target_is_rejected(self):
        self.inventory['profiles']['queue']['tests'][0]['target'] = 'test:other'
        with self.assertRaisesRegex(ValueError, 'wrong Cargo target'):
            self.validate()

    def test_unbounded_bytes_are_explicit_not_inferred_from_elements(self):
        self.assertEqual(self.validate()['verdict'], 'PASS')
        self.inventory['profiles']['queue']['capacity']['bytes']['kind'] = 'unknown'
        with self.assertRaisesRegex(ValueError, 'unknown bytes capacity'):
            self.validate()

    def test_new_creation_requires_an_inventory_row(self):
        more = {'src/new_queue.rs': 'fn q() { let _ = mpsc::channel(5); }'}
        self.sources.update(more)
        self.sites += scan_sources(more)
        with self.assertRaisesRegex(ValueError, 'missing creations'):
            self.validate()

    def test_stale_occurrence_is_rejected(self):
        rows = [{'profile': 'queue', 'site': {
            'path': PATH, 'kind': 'channel',
            'expression': 'tokio::sync::mpsc::channel::<u8>(17)', 'occurrence': 2}}]
        with self.assertRaisesRegex(ValueError, 'stale inventory site'):
            current_inventory(self.sources, self.inventory['profiles'], rows, [])


class RelocationFamily:
    """Exercise one relocation family and reject each restored stale reference."""
    moves = ()
    expected_rows = expected_anchors = expected_tests = None
    stale_anchor_error = stale_test_error = None

    @classmethod
    def setUpClass(cls):
        from check_inventory import load_sources

        root = Path(__file__).resolve().parents[2]
        registry = root / 'scripts/queue-inventory'
        cls.sources = load_sources(root)
        cls.old_paths = {'crates/rmux-server/src/' + new:
                         'crates/rmux-server/src/' + old for old, new in cls.moves}
        cls.rows = json.loads((registry / 'sites.json').read_text())
        cls.profiles = json.loads((registry / 'profiles.json').read_text())
        cls.references = []

        def visit(value):
            if isinstance(value, dict):
                if value.get('path') in cls.old_paths:
                    cls.references.append(value)
                for child in value.values():
                    visit(child)
            elif isinstance(value, list):
                for child in value:
                    visit(child)

        visit(cls.rows)
        visit(cls.profiles)

    def test_moved_sites_and_missing_siblings(self):
        rows = [row for row in self.rows if row['site']['path'] in self.old_paths]
        self.assertEqual(len(rows), self.expected_rows)
        sources = {path: self.sources[path] for path in self.old_paths}
        used = {row['profile'] for row in rows if 'profile' in row}
        profiles = {key: self.profiles[key] for key in used}
        inventory, sites = current_inventory(sources, profiles, rows, [])
        self.assertEqual(validate_inventory(inventory, sites, self.sources)['verdict'], 'PASS')
        for row in rows:
            with self.subTest(site=row['site']):
                stale = copy.deepcopy(row)
                path = stale['site']['path']
                stale['site']['path'] = self.old_paths[path]
                with self.assertRaisesRegex(ValueError, 'stale inventory site'):
                    current_inventory({path: sources[path]}, profiles, [stale], [])
                removed = {path: sources[path].replace(row['site']['expression'], '')}
                with self.assertRaisesRegex(ValueError, 'stale inventory site'):
                    current_inventory(removed, profiles, [row], [])
                incomplete = copy.deepcopy(inventory)
                key = 'exemptions' if 'category' in row else 'entries'
                siblings = [entry for entry in incomplete[key]
                            if not entry['site']['test_only']
                            and entry['site']['path'] == path
                            and entry['site']['kind'] == row['site']['kind']
                            and entry['site']['expression'] == row['site']['expression']]
                incomplete[key].remove(siblings[row['site']['occurrence'] - 1])
                with self.assertRaisesRegex(ValueError, 'missing creations'):
                    validate_inventory(incomplete, sites, self.sources)

    def test_moved_evidence(self):
        from validation import check_anchor

        anchors = [ref for ref in self.references if 'snippet' in ref]
        self.assertEqual(len(anchors), self.expected_anchors)
        for anchor in anchors:
            with self.subTest(anchor=anchor):
                check_anchor(anchor, self.sources)
                stale = dict(anchor, path=self.old_paths[anchor['path']])
                with self.assertRaisesRegex(ValueError, self.stale_anchor_error):
                    check_anchor(stale, self.sources)
        return anchors

    def test_moved_native_test_references(self):
        tests = [ref for ref in self.references if 'symbol' in ref]
        self.assertEqual(len(tests), self.expected_tests)
        for reference in tests:
            with self.subTest(reference=reference):
                inventory, sites, sources = fixture()
                # Validate the real annotation and Cargo target without native execution.
                sources.update(self.sources)
                inventory['profiles']['queue']['tests'] = [dict(reference)]
                validate_inventory(inventory, sites, sources)
                inventory['profiles']['queue']['tests'][0]['path'] = self.old_paths[reference['path']]
                with self.assertRaisesRegex(ValueError, self.stale_test_error):
                    validate_inventory(inventory, sites, sources)


class RelocatedServerGuards(RelocationFamily, unittest.TestCase):
    """The C08 family; split files list every current home of their sites."""
    moves = (
        ('handler_control.rs',
         'handler/control_support.rs'),
        ('handler_control.rs',
         'handler/control_support/registration.rs'),
        ('handler_copy_mode/pipe_command.rs',
         'handler/copy_mode_support/pipe_command.rs'),
        ('handler_lifecycle.rs',
         'handler/lifecycle_support.rs'),
        ('handler_lifecycle.rs',
         'handler/lifecycle_support/publication.rs'),
        ('handler_mode_tree/parse.rs',
         'handler/mode_tree_support/mode_tree_parse.rs'),
        ('handler_overlay/completion.rs',
         'handler/overlay_support/completion.rs'),
        ('handler_overlay/parse.rs',
         'handler/overlay_support/parse.rs'),
        ('handler_overlay/popup_io.rs',
         'handler/overlay_support/popup_io.rs'),
        ('handler_pane/attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live.rs'),
        ('handler_pane/attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live/frame_order.rs'),
        ('handler_pane/attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live/modal.rs'),
        ('handler_pane/attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live/work_queue.rs'),
        ('handler_prompt/lifecycle.rs',
         'handler/prompt_support/lifecycle.rs'),
        ('handler_scripting/queue_execution.rs',
         'handler/scripting_support/queue_execution.rs'),
        ('handler_scripting/prompt_parse.rs',
         'handler/scripting_support/prompt_parse.rs'),
        ('handler_scripting/queue.rs',
         'handler/scripting_support/queue.rs'),
        ('handler_scripting/queue_parse.rs',
         'handler/scripting_support/queue_parse.rs'),
        ('handler_scripting/targets.rs',
         'handler/scripting_support/targets.rs'),
        ('handler_scripting/targets.rs',
         'handler/scripting_support/targets/queue_arguments.rs'),
        ('handler_scripting/tokens.rs',
         'handler/scripting_support/tokens.rs'),
        ('handler_shutdown.rs',
         'handler/shutdown_support.rs'),
        ('handler_waits.rs',
         'handler/wait_support.rs'),
        ('handler_web_capture.rs',
         'handler/web_support/capture.rs'),
        ('pane_pipe.rs',
         'pane_terminals/pane_pipe.rs'),
        ('handler_scripting.rs',
         'handler/scripting_support.rs'),
        ('control/fifo_tests.rs',
         'control/subscriptions/fifo_tests.rs'),
        ('pane_io/reader.rs',
         'pane_io/reader/timers.rs'),
    )
    expected_rows, expected_anchors, expected_tests = 36, 63, 20
    stale_anchor_error = 'invalid evidence path|evidence anchor absent'
    stale_test_error = 'invalid test reference'

    def test_moved_cfg_test_path_declaration(self):
        """The relocated probe is test-only through its relative #[path]; a stale one is product."""
        parent = 'crates/rmux-server/src/handler/scripting_support.rs'
        probe = 'crates/rmux-server/src/handler_scripting/send_keys_admission_probe.rs'
        declaration = '#[path = "../handler_scripting/send_keys_admission_probe.rs"]'
        self.assertEqual(self.sources[parent].count(declaration), 1)

        chain = ('lib.rs', 'handler.rs', 'handler/scripting_support.rs',
                 'handler_scripting/send_keys_admission_probe.rs')
        real = {p: self.sources[p] for p in ('crates/rmux-server/src/' + c for c in chain)}

        def probe_sites(sources):
            return [site['test_only'] for site in scan_sources(sources) if site['path'] == probe]

        self.assertEqual(probe_sites(real), [True])
        stale = dict(real)
        stale[parent] = stale[parent].replace(declaration, declaration.replace('../', ''))
        self.assertEqual(probe_sites(stale), [False])


class SplitServerModuleGuards(RelocationFamily, unittest.TestCase):
    """Sites moved into child modules while the parent file still exists."""
    moves = (
        ('handler/control_support.rs',
         'handler/control_support/registration.rs'),
        ('handler/lifecycle_support.rs',
         'handler/lifecycle_support/publication.rs'),
        ('handler/pane_support/pane_attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live/frame_order.rs'),
        ('handler/pane_support/pane_attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live/modal.rs'),
        ('handler/pane_support/pane_attached_input/live.rs',
         'handler/pane_support/pane_attached_input/live/work_queue.rs'),
        ('listener.rs',
         'listener/connection_dispatch.rs'),
        ('pane_io.rs',
         'pane_io/forward.rs'),
        ('pane_io/types.rs',
         'pane_io/types/attach.rs'),
        ('pane_io/types.rs',
         'pane_io/types/output_state.rs'),
        ('pane_io/types.rs',
         'pane_io/types/sender.rs'),
        ('pane_io/types.rs',
         'pane_io/types/tests.rs'),
    )
    expected_rows, expected_anchors, expected_tests = 15, 20, 4
    # The parent survives, so each snippet and test must have left it.
    stale_anchor_error = 'evidence anchor absent'
    stale_test_error = 'annotated test definition absent'


class ModuleTreeGuards(unittest.TestCase):
    """cfg(test) follows `mod x;` and #[path] declarations from the crate root."""
    ROOT = 'crates/example/src/lib.rs'
    HELPER = 'crates/example/src/support/helper.rs'
    QUEUE = 'pub fn helper() { let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<u8>(); }\n'

    def helper_test_only(self, sources):
        return [s['test_only'] for s in scan_sources(sources) if s['path'] == self.HELPER]

    def test_file_declared_only_under_cfg_test_is_test_only(self):
        sources = {self.ROOT: '#[cfg(test)]\n#[path = "support/helper.rs"]\nmod helper;\n',
                   self.HELPER: self.QUEUE}
        self.assertEqual(self.helper_test_only(sources), [True])
        current, sites = current_inventory(sources, {}, [], [])
        self.assertEqual(validate_inventory(current, sites, sources)['verdict'], 'PASS')
        self.assertEqual([row['category'] for row in current['exemptions']], ['test_only'])

    def test_second_inclusion_outside_test_makes_the_file_product(self):
        # Angle mort N5: one ungated chain compiles the helper into the product.
        sources = {self.ROOT: '#[cfg(test)]\n#[path = "support/helper.rs"]\nmod helper;\n'
                              '#[path = "support/helper.rs"]\nmod leaked_helper;\n',
                   self.HELPER: self.QUEUE}
        self.assertEqual(self.helper_test_only(sources), [False])
        current, sites = current_inventory(sources, {}, [], [])
        with self.assertRaisesRegex(ValueError, 'missing creations: ' + self.HELPER):
            validate_inventory(current, sites, sources)

    def test_cfg_test_propagates_through_plain_and_path_children(self):
        middle = 'crates/example/src/support.rs'
        sources = {self.ROOT: '#[cfg(all(test, any(unix, windows)))]\npub(crate) mod support;\n',
                   middle: 'mod helper;\n', self.HELPER: self.QUEUE}
        self.assertEqual(self.helper_test_only(sources), [True])
        sources[self.ROOT] = 'pub(crate) mod support;\n'
        self.assertEqual(self.helper_test_only(sources), [False])

    def test_path_loaded_file_resolves_children_like_mod_rs(self):
        # rustc: a #[path] module file resolves `mod x;` beside itself, not under its stem.
        loaded = 'crates/example/src/loaded.rs'
        beside = 'crates/example/src/helper.rs'
        sources = {self.ROOT: '#[path = "loaded.rs"]\nmod renamed;\n',
                   loaded: '#[cfg(test)]\nmod helper;\n', beside: self.QUEUE,
                   'crates/example/src/loaded/helper.rs': self.QUEUE}
        flags = {s['path']: s['test_only'] for s in scan_sources(sources)}
        self.assertTrue(flags[beside])
        self.assertFalse(flags['crates/example/src/loaded/helper.rs'])  # unreached: name heuristic

    def test_inline_test_module_gates_nested_declaration(self):
        sources = {self.ROOT: '#[cfg(test)]\nmod tests {\n    #[path = "../support/helper.rs"]\n'
                              '    mod helper;\n}\n',
                   self.HELPER: self.QUEUE}
        self.assertEqual(self.helper_test_only(sources), [True])

    def test_non_test_cfgs_stay_product(self):
        for cfg in ('any(test, windows)', 'not(test)', 'feature = "test"', 'all(unix, not(test))'):
            with self.subTest(cfg=cfg):
                sources = {self.ROOT: f'#[cfg({cfg})]\n#[path = "support/helper.rs"]\nmod helper;\n',
                           self.HELPER: self.QUEUE}
                self.assertEqual(self.helper_test_only(sources), [False])

    def test_test_named_file_included_in_product_is_product(self):
        named = 'crates/example/src/helper_tests.rs'
        sources = {self.ROOT: 'mod helper_tests;\n', named: self.QUEUE}
        self.assertEqual([s['test_only'] for s in scan_sources(sources)], [False])
        del sources[self.ROOT]
        self.assertEqual([s['test_only'] for s in scan_sources(sources)], [True])

    def test_integration_test_root_and_inner_cfg_test_are_test_only(self):
        child = 'crates/example/tests/common/mod.rs'
        sources = {'crates/example/tests/flow.rs': 'mod common;\n', child: self.QUEUE,
                   self.ROOT: 'mod support;\n',
                   'crates/example/src/support.rs': '#![cfg(test)]\nmod helper;\n',
                   self.HELPER: self.QUEUE}
        flags = {s['path']: s['test_only'] for s in scan_sources(sources)}
        self.assertEqual(flags, {child: True, self.HELPER: True})

    def test_unlisted_cargo_target_keeps_its_children_product(self):
        # A [[bin]] path such as src/daemon_main.rs is no default root; it still compiles rt.rs.
        rt = 'crates/example/src/rt.rs'
        sources = {'crates/example/src/main.rs': '#[cfg(test)]\nmod rt;\n',
                   'crates/example/src/daemon_main.rs': 'mod rt;\n', rt: self.QUEUE}
        self.assertEqual([s['test_only'] for s in scan_sources(sources)], [False])

    def test_nested_inner_cfg_test_gates_only_its_module(self):
        support = 'crates/example/src/support.rs'
        sources = {self.ROOT: 'mod support;\n',
                   support: 'mod probe {\n    #![cfg(test)]\n}\n' + self.QUEUE}
        self.assertEqual([s['test_only'] for s in scan_sources(sources)], [False])

    def test_tests_directory_under_src_is_not_a_crate_root(self):
        # rustc: src/x/tests/a.rs loaded by `mod a;` is non-mod-rs, so `mod b;` is a/b.rs.
        deep = 'crates/example/src/x/tests/a/b.rs'
        sources = {self.ROOT: 'mod x;\n', 'crates/example/src/x.rs': 'pub mod tests;\n',
                   'crates/example/src/x/tests/mod.rs': 'pub mod a;\n',
                   'crates/example/src/x/tests/a.rs': 'pub mod b;\n', deep: self.QUEUE}
        self.assertEqual([s['test_only'] for s in scan_sources(sources)], [False])

    def test_unsupported_cfg_attr_path_fails_closed(self):
        sources = {self.ROOT: '#[cfg_attr(test, path = "support/helper.rs")]\nmod helper;\n',
                   self.HELPER: self.QUEUE}
        with self.assertRaisesRegex(ValueError, 'cfg_attr\\(path\\) is unsupported'):
            scan_sources(sources)


class SourceGuards(unittest.TestCase):
    def test_comments_and_literals_do_not_create_sites(self):
        source = '// mpsc::channel(1)\nfn q() { let s = r#"VecDeque::new()"#; }'
        self.assertEqual(scan_sources({PATH: source}), [])

    def test_test_body_is_exempt_but_any_test_windows_is_product(self):
        text = ('#[cfg(test)] mod tests { fn q() { mpsc::channel(1); } }\n'
                '#[cfg(any(test, windows))] fn p() { mpsc::channel(2); }')
        sites = scan_sources({PATH: text})
        self.assertEqual([s['test_only'] for s in sites], [True, False])

    def test_comment_cannot_fake_annotated_test(self):
        self.assertEqual(test_symbols('// #[test] fn fake() {}\nfn real() {}'), [])

    def test_supplementary_store_anchor_must_be_unique(self):
        spec = [{'name': 'store', 'path': PATH, 'anchor': 'items: Vec<u8>'}]
        for text in ('struct S {}', 'items: Vec<u8> items: Vec<u8>'):
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, 'missing/ambiguous'):
                scan_sources({PATH: text}, spec)

    def test_qualified_constant_requires_a_real_unambiguous_binding(self):
        local = 'crates/example/src/listener.rs'
        sources = {PATH: 'mod handler;', local: 'use crate::handler;',
                   'crates/example/src/handler.rs': 'pub const CAP: usize = 256;'}
        self.assertEqual(numeric_capacity('handler::CAP', local, sources), 256)
        self.assertEqual(numeric_capacity('crate::handler::CAP', local, sources), 256)
        sources[local] = ''
        self.assertIsNone(numeric_capacity('handler::CAP', local, sources))
        sources[local] = 'use crate::handler; mod handler;'
        self.assertIsNone(numeric_capacity('handler::CAP', local, sources))
        sources[local] = 'use crate::handler;'
        sources[PATH] = '#[path="elsewhere.rs"] mod handler;'
        self.assertIsNone(numeric_capacity('handler::CAP', local, sources))

    def test_qualified_constant_follows_only_a_sole_parent_glob(self):
        parent, child = 'crates/example/src/listener.rs', 'crates/example/src/listener/dispatch.rs'
        base = {PATH: 'mod handler; mod listener;',
                'crates/example/src/handler.rs': 'pub const CAP: usize = 256;',
                parent: 'use crate::handler::{self, Other};\nmod dispatch;',
                child: 'use super::*;'}
        self.assertEqual(numeric_capacity('handler::CAP', child, base), 256)
        for label, path, text in (
                ('no glob', child, 'use super::Other;'),
                ('second glob', child, 'use super::*;\nuse crate::other::*;'),
                ('braced glob', child, 'use super::*;\nuse crate::{other::*, Thing};'),
                ('local shadow', child, 'use super::*;\nuse crate::other::handler;'),
                ('local module', child, 'use super::*;\nmod handler;'),
                ('parent lacks binding', parent, 'mod dispatch;'),
                ('child not declared', parent, 'use crate::handler::{self, Other};'),
                ('path attribute', parent, 'use crate::handler::{self};\n#[path = "x.rs"] mod dispatch;')):
            with self.subTest(label):
                self.assertIsNone(numeric_capacity('handler::CAP', child, {**base, path: text}))

    def test_gate_links_cannot_be_comments_only(self):
        with tempfile.TemporaryDirectory(prefix='queue-gates-') as directory:
            root = Path(directory)
            (root / 'scripts').mkdir()
            line = 'python3 -B scripts/queue-inventory/check_inventory.py --source . --execute'
            for gate in ('gate-unix-fast.sh', 'gate-windows-fast.ps1'):
                (root / 'scripts' / gate).write_text('# ' + line + '\n')
            with self.assertRaisesRegex(ValueError, 'must execute the queue references'):
                validate_gate_links(root)
            for gate in ('gate-unix-fast.sh', 'gate-windows-fast.ps1'):
                (root / 'scripts' / gate).write_text(line + '\n')
            validate_gate_links(root)


class NativeSelectionGuards(unittest.TestCase):
    def profiles(self):
        return {'q': {'tests': [
            {'package': 'example', 'target': 'lib', 'symbol': 'queue_test',
             'platforms': ['linux']},
        ]}}

    def fake_run(self, command, root, timeout=None):
        self.commands.append(command)
        if command[0] == 'cargo':
            return json.dumps({'reason': 'compiler-artifact', 'executable': '/fixture/test',
                               'profile': {'test': True}})
        if '--list' in command:
            return self.listing
        return self.result

    def setUp(self):
        self.commands = []
        self.listing = 'module::queue_test: test\n'
        self.result = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'

    def run_selection(self, profiles=None):
        with patch('run_references.host_platform', return_value='linux'), \
                patch('run_references.run', self.fake_run):
            return execute(Path('/fixture'), profiles or self.profiles())

    def test_exact_native_name_executes(self):
        self.assertEqual(self.run_selection()['passed'], 1)
        self.assertIn('module::queue_test', self.commands[-1])
        self.assertIn('--exact', self.commands[-1])

    def test_missing_native_test_fails_before_execution(self):
        self.listing = 'another::test: test\n'
        with self.assertRaisesRegex(ValueError, 'expected one native test'):
            self.run_selection()
        self.assertFalse(any('--exact' in c for c in self.commands))

    def test_ambiguous_native_name_fails_before_execution(self):
        self.listing += 'other::queue_test: test\n'
        with self.assertRaisesRegex(ValueError, 'expected one native test'):
            self.run_selection()
        self.assertFalse(any('--exact' in c for c in self.commands))

    def test_platform_exclusion_cannot_hide_a_compiled_test(self):
        profiles = self.profiles()
        profiles['q']['tests'][0]['platforms'] = ['windows']
        with self.assertRaisesRegex(ValueError, 'incorrectly excluded native test'):
            self.run_selection(profiles)

    def test_ignored_test_is_not_a_passing_reference(self):
        self.result = 'test result: ok. 0 passed; 0 failed; 1 ignored;\n'
        with self.assertRaisesRegex(ValueError, 'no executed passing test'):
            self.run_selection()

    def test_entire_population_qualifies_before_any_test_runs(self):
        profiles = self.profiles()
        other = copy.deepcopy(profiles['q']['tests'][0])
        other.update(package='other', symbol='missing_test')
        profiles['other'] = {'tests': [other]}
        with self.assertRaisesRegex(ValueError, 'expected one native test'):
            self.run_selection(profiles)
        self.assertEqual(sum(c[0] == 'cargo' for c in self.commands), 2)
        self.assertFalse(any('--exact' in c for c in self.commands))

    def test_identity_runner_precedes_native_listing_and_exact_execution(self):
        # The SDK command fixture re-execs this very test image inside its
        # unchanged 2s deadline. Cargo's active runner qualifies the retained
        # image before the test starts; a raw compiler alias bypasses that step.
        # Only a Windows runner first proves the image with `check`, outside the
        # 60 s libtest deadline; every host then lists and executes with `run`.
        python, script = '/fixture/python', '/fixture/identity.py'
        environment = {
            'RMUX_IDENTITY_PYTHON': python,
            'RMUX_IDENTITY_SCRIPT': script,
            'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER': f'{python} {script} run',
        }

        def runner(command, root, timeout=None):
            if command[0] == 'cargo':
                return self.fake_run(command, root, timeout)
            self.commands.append((command, timeout))
            return self.listing if '--list' in command else self.result

        profiles = self.profiles()
        profiles['q']['tests'][0]['platforms'] = ['linux', 'macos', 'windows']
        for platform in ('linux', 'macos', 'windows'):
            self.commands = []
            with self.subTest(platform=platform), patch.dict(os.environ, environment, clear=True), \
                    patch('run_references.host_platform', return_value=platform), \
                    patch('run_references.run', runner):
                self.assertEqual(execute(Path('/fixture'), profiles)['passed'], 1)
                launch = [python, script, 'run', '/fixture/test']
                qualify = [([python, script, 'check', '/fixture/test'], None)]
                self.assertEqual(self.commands[0][0], 'cargo')
                self.assertEqual(self.commands[1:], [
                    *(qualify if platform == 'windows' else []),
                    (launch + ['--list', '--format', 'terse'], 60),
                    (launch + ['module::queue_test', '--exact', '--nocapture', '--test-threads=1'], 60),
                ], 'native references must use the active identity runner')


if __name__ == '__main__':
    unittest.main()
