"""Inert closure, replay, and with-manifest siblings for INT589-F2."""
from collections import Counter
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_candidate_identity_multimanifest as fixtures


class LockClosure(unittest.TestCase):
    setUp = fixtures.CommonContextIdentity.setUp
    git = fixtures.CommonContextIdentity.git
    before = fixtures.CommonContextIdentity.before

    def commit(self):
        self.git('add', '.')
        self.git('-c', 'user.name=Sidney Sissaoui', '-c', 'user.email=shideneyu@gmail.com',
                 'commit', '-qm', 'inert lock variation')
        self.head = self.git('rev-parse', 'HEAD')
        self.target = self.g.target_directory(self.repo, self.head, '0' * 32)
        self.target.mkdir(parents=True)

    def unit(self, code, dependencies=(), kind='lib', env=None):
        args = [self.compiler, str(code), '--crate-name', 'fixture', '--crate-type', kind,
                '--out-dir', str(self.target / 'debug')]
        for dependency in dependencies:
            args += ['--extern', 'dependency=' + str(dependency)]
        environment = {'RMUX_IDENTITY_RUSTC': self.compiler,
                       'CARGO_MANIFEST_DIR': str(code.parent)}
        environment.update(env or {})
        return self.units.before(self.g, args, self.repo, self.head, self.target, env=environment)

    def publish(self, unit, name):
        # These are non-executable byte fixtures, not compiled images.
        path = self.target / name
        graph = unit['graph']
        sha = self.units.digest(graph)
        source_id = hashlib.sha256(str(self.repo).encode()).hexdigest()
        marker = f'RMUX_BUILD_IDENTITY_V1:{self.head}:{source_id}:{self.g.host_os()}:END\n'
        path.write_bytes(b'INERT FIXTURE\n' + marker.encode() + self.units.graph_marker(graph, sha))
        record = dict(path=str(path), sha256=self.g.file_hash(path), kind=unit['kind'],
                      graph=graph, graph_sha256=sha)
        receipt = self.units.preserve(self.g, self.target / 'identity-units', record)
        (receipt.parent / (record['sha256'] + '.json')).write_bytes(receipt.read_bytes())
        return path

    def verify(self, path):
        return self.units.verify(self.g, path, self.repo, self.head, self.target)

    def manifests(self, unit):
        return [c['manifest'] for c in unit['graph']['lock_contexts']]

    def share_dependency(self):
        with (self.repo / 'Cargo.lock').open('a') as stream:
            stream.write('[[package]]' + self.lock.read_text().split('[[package]]')[2])
        self.commit()

    def test_parser_dependency_links_only_parser_and_verifies_same_context(self):
        dep = self.publish(self.unit(self.package / 'lib.rs'), 'dep.rlib')
        parser = self.publish(self.unit(self.code, [dep], 'bin'), 'parser.inert')
        verified = self.verify(parser)
        self.assertEqual([c['manifest'] for c in verified['lock_contexts']], [str(self.manifest)])
        self.assertIn(self.head.encode(), parser.read_bytes())
        self.assertIn(b'RMUX_BUILD_GRAPH_V2:', parser.read_bytes())
        with self.assertRaisesRegex(self.g.IdentityError, 'cross original workspace locks'):
            self.unit(self.repo / 'main.rs', [dep], 'bin')

    def test_shared_dependency_keeps_both_locks_until_final_consumer(self):
        self.share_dependency()
        unit = self.unit(self.package / 'lib.rs')
        self.assertEqual(self.manifests(unit), [str(self.repo / 'Cargo.toml'), str(self.manifest)])
        dep = self.publish(unit, 'shared.rlib')
        for name, code, manifest in [('root', self.repo / 'main.rs', self.repo / 'Cargo.toml'),
                                     ('parser', self.code, self.manifest)]:
            parent = self.unit(code, [dep], 'bin')
            self.assertEqual(self.manifests(parent), [str(manifest)])
            self.verify(self.publish(parent, name + '.inert'))

    def test_root_only_dependency_cannot_link_parser(self):
        self.share_dependency()
        self.lock.write_text('version=4\n[[package]]\nname="parser"\nversion="1.0.0"\n')
        self.commit()
        dep = self.publish(self.unit(self.package / 'lib.rs'), 'root-only.rlib')
        self.verify(self.publish(self.unit(self.repo / 'main.rs', [dep]), 'root.rlib'))
        with self.assertRaisesRegex(self.g.IdentityError, 'cross original workspace locks'):
            self.unit(self.code, [dep])

    def test_transitive_parser_dependency_cannot_hide_behind_shared_package(self):
        self.share_dependency()
        parser = self.publish(self.unit(self.code), 'parser-lib.rlib')
        shared = self.publish(self.unit(self.package / 'lib.rs', [parser]), 'shared.rlib')
        self.assertEqual([c['manifest'] for c in self.verify(shared)['lock_contexts']], [str(self.manifest)])
        with self.assertRaisesRegex(self.g.IdentityError, 'cross original workspace locks'):
            self.unit(self.repo / 'main.rs', [shared])

    def test_replay_rejects_forged_transitive_context_and_missing_version(self):
        dep = self.publish(self.unit(self.package / 'lib.rs'), 'dep.rlib')
        unit = self.unit(self.code, [dep], 'bin')
        root = self.g.support('candidate_identity_manifest').root_binding(self.g, self.repo, self.head)
        for mutation in ('widen', 'missing-version', 'missing-closure', 'unknown-version'):
            bad = copy.deepcopy(unit)
            graph = bad['graph']
            if mutation == 'widen':
                graph['lock_contexts'].insert(0, root)
                graph['lock_sha256'] = root['lock_sha256']
                graph.pop('compilation_unit')
            elif mutation == 'missing-version':
                graph.pop('lock_context_version')
            elif mutation == 'missing-closure':
                graph.pop('lock_contexts')
            else:
                graph['lock_context_version'] = 9
            with self.subTest(mutation=mutation), self.assertRaises(self.g.IdentityError):
                self.verify(self.publish(bad, mutation + '.inert'))

    def test_original_manifest_and_lock_mutations_refused_before_and_replay(self):
        dep = self.publish(self.unit(self.package / 'lib.rs'), 'dep.rlib')
        parser = self.publish(self.unit(self.code, [dep], 'bin'), 'parser.inert')
        for path in (self.repo / 'Cargo.toml', self.repo / 'Cargo.lock', self.manifest, self.lock):
            original = path.read_bytes()
            try:
                path.write_bytes(original + b'\n# changed\n')
                with self.subTest(path=path, action='before'), self.assertRaises(self.g.IdentityError):
                    self.unit(self.package / 'lib.rs')
                with self.subTest(path=path, action='replay'), self.assertRaises(self.g.IdentityError):
                    self.verify(parser)
            finally:
                path.write_bytes(original)
        self.verify(parser)

    def test_registry_archive_and_extraction_mutation_refused(self):
        archive = self.fixture / 'cargo/registry/cache/fixture/zmij-1.0.23.crate'
        for path in (archive, self.package / 'lib.rs', self.package / 'Cargo.toml'):
            original = path.read_bytes()
            try:
                path.write_bytes(original + b'\n# mutation\n')
                with self.subTest(path=path), self.assertRaises(self.g.IdentityError):
                    self.unit(self.package / 'lib.rs')
            finally:
                path.write_bytes(original)

    def test_unpinned_package_rejected(self):
        manifest = self.package / 'Cargo.toml'
        manifest.write_text(manifest.read_text().replace('1.0.23', '9.0.0'))
        with self.assertRaisesRegex(self.g.IdentityError, 'not pinned'):
            self.unit(self.package / 'lib.rs')

    def test_manifest_environment_cannot_move_source_between_workspaces(self):
        for code, directory in ((self.code, self.repo), (self.repo / 'main.rs', self.manifest.parent)):
            with self.subTest(code=code), self.assertRaisesRegex(self.g.IdentityError, 'different workspaces'):
                self.unit(code, env={'CARGO_MANIFEST_DIR': str(directory)})

    def test_arbitrary_standalone_not_added_to_common_context(self):
        foreign = self.repo / 'other'
        foreign.mkdir()
        for name in ('Cargo.toml', 'Cargo.lock', 'main.rs'):
            shutil.copyfile(self.manifest.parent / name, foreign / name)
        self.commit()
        with self.assertRaisesRegex(self.g.IdentityError, 'cross original workspace locks'):
            self.unit(foreign / 'main.rs')

    def test_root_workspace_member_keeps_root_lock(self):
        member = self.repo / 'crates/member'
        member.mkdir(parents=True)
        (member / 'Cargo.toml').write_text('[package]\nname="member"\nversion="1.0.0"\n')
        (member / 'lib.rs').write_text('pub fn member() {}\n')
        self.commit()
        self.assertEqual(self.manifests(self.unit(member / 'lib.rs')), [str(self.repo / 'Cargo.toml')])

    def standalone(self):
        manifests = self.g.support('candidate_identity_manifest')
        unit = manifests.describe(self.g, self.manifest, self.repo, self.head)
        self.target = self.g.target_directory(self.repo, self.head, '1' * 32)
        manifests.initialize(self.g, unit, self.repo, self.head, self.target)
        return unit

    def test_with_manifest_remains_isolated_and_replayable(self):
        binding = self.standalone()
        dep = self.publish(self.unit(self.package / 'lib.rs'), 'dep.rlib')
        parser = self.publish(self.unit(self.code, [dep], 'bin'), 'parser.inert')
        self.assertEqual(self.verify(parser)['lock_contexts'], [binding])
        with self.assertRaisesRegex(self.g.IdentityError, 'another compilation unit'):
            self.unit(self.repo / 'main.rs')
        ticket = self.target / 'identity-compilation-unit.json'
        raw = ticket.read_bytes()
        altered = json.loads(raw)
        altered['lock'] = str(self.repo / 'Cargo.lock')
        ticket.write_text(json.dumps(altered))
        with self.assertRaises(self.g.IdentityError):
            self.verify(parser)
        ticket.write_bytes(raw)
        self.verify(parser)

    def test_with_manifest_action_refuses_common_context_without_new_prepare(self):
        manifests = self.g.support('candidate_identity_manifest')
        with patch.dict(os.environ, {'RMUX_IDENTITY_HEAD': self.head}), \
                patch.object(self.g, 'prepare') as prepare:
            with self.assertRaisesRegex(self.g.IdentityError, 'fresh identity context'):
                manifests.action(self.g, [str(self.manifest), '--', 'must-not-execute'])
            prepare.assert_not_called()

    def test_replay_cannot_remove_or_inject_standalone_context_ticket(self):
        parser = self.publish(self.unit(self.code, kind='bin'), 'common-parser.inert')
        common_target = self.target
        binding = self.standalone()
        standalone = self.publish(self.unit(self.code, kind='bin'), 'standalone-parser.inert')
        ticket = self.target / 'identity-compilation-unit.json'
        raw = ticket.read_bytes()
        ticket.unlink()
        with self.assertRaisesRegex(self.g.IdentityError, 'lock closure binding differs'):
            self.verify(standalone)
        ticket.write_bytes(raw)
        self.verify(standalone)
        self.target = common_target
        injected = self.target / 'identity-compilation-unit.json'
        injected.write_text(json.dumps(binding))
        with self.assertRaisesRegex(self.g.IdentityError, 'lock closure binding differs'):
            self.verify(parser)
        injected.unlink()
        self.verify(parser)

    def test_with_manifest_dispatch_passes_exact_authenticated_binding(self):
        manifests = self.g.support('candidate_identity_manifest')
        git_program = shutil.which('git')
        self.assertIsNotNone(git_program, 'fixture requires Git on PATH')
        git_environment = {'PATH': str(Path(git_program).absolute().parent)}
        previous = Path.cwd()
        try:
            os.chdir(self.repo)
            with patch.dict(os.environ, git_environment, clear=True), \
                    patch.object(self.g, 'prepare', return_value={'fixture': 'inert'}) as prepare, \
                    patch.object(manifests.subprocess, 'call', return_value=0) as call:
                self.assertEqual(manifests.action(self.g, [str(self.manifest), '--', 'inert']), 0)
                self.assertEqual(prepare.call_args.kwargs['compilation_unit'],
                                 manifests.describe(self.g, self.manifest, self.repo, self.head))
                call.assert_called_once_with(['inert'], env={'fixture': 'inert'})
        finally:
            os.chdir(previous)

    def test_with_manifest_dispatch_keeps_git_for_windows_selection(self):
        # Exercise the real Windows Git branch without changing pathlib's host.
        windows_os = SimpleNamespace(**{**vars(os), 'name': 'nt'})
        host_which = shutil.which

        def path_git(command):
            self.assertEqual(command, 'git')
            return host_which(command, path=os.environ.get('PATH', ''))

        with patch.dict(self.g.git.__globals__, {'os': windows_os}), \
                patch.object(shutil, 'which', side_effect=path_git) as which:
            # Git installed outside the default Windows search path requires PATH.
            with patch.dict(os.environ, {}, clear=True):
                self.assertIsNone(shutil.which('git'))
            self.test_with_manifest_dispatch_passes_exact_authenticated_binding()
            self.assertGreater(which.call_count, 1)

    def test_with_manifest_dispatch_keeps_relative_git_for_windows_selection(self):
        git_program = shutil.which('git')
        self.assertIsNotNone(git_program, 'fixture requires Git on PATH')
        git_directory = Path(git_program).absolute().parent
        previous = Path.cwd()
        try:
            # Find the installed Git through a real relative PATH before dispatch.
            os.chdir(git_directory.parent)
            with patch.dict(os.environ, {'PATH': git_directory.name}, clear=True):
                relative_git = shutil.which('git')
                self.assertIsNotNone(relative_git)
                self.assertFalse(Path(relative_git).is_absolute())
                self.assertTrue(Path(relative_git).is_file())
                self.test_with_manifest_dispatch_keeps_git_for_windows_selection()
        finally:
            os.chdir(previous)

    def test_build_script_generated_inputs_keep_producer_lock_closure(self):
        dep = self.publish(self.unit(self.package / 'lib.rs'), 'producer.rlib')
        producer = self.verify(dep)
        generation = dict(receipt='unused-inert-receipt', producer=producer)
        manifests = self.g.support('candidate_identity_manifest')
        root = self.before(self.repo / 'main.rs')['graph']['input']
        parser = self.before(self.code)['graph']['input']
        with self.assertRaisesRegex(self.g.IdentityError, 'cross original workspace locks'):
            manifests.closure(self.g, root, [], generation, self.repo, self.head, self.target)
        contexts = manifests.closure(self.g, parser, [], generation, self.repo, self.head, self.target)
        self.assertEqual([c['manifest'] for c in contexts], [str(self.manifest)])

    def test_replay_rejects_changed_image_and_wrong_head(self):
        parser = self.publish(self.unit(self.code, kind='bin'), 'parser.inert')
        original = parser.read_bytes()
        parser.write_bytes(original + b'changed')
        with self.assertRaises(self.g.IdentityError):
            self.verify(parser)
        parser.write_bytes(original)
        with self.assertRaises(self.g.IdentityError):
            self.units.verify(self.g, parser, self.repo, 'f' * 40, self.target)
        self.verify(parser)

    def test_v1_root_receipt_remains_valid_but_cannot_import_parser_closure(self):
        root = self.unit(self.repo / 'main.rs', kind='bin')
        root['graph'].pop('lock_context_version')
        root['graph'].pop('lock_contexts')
        root['graph'].pop('manifest_scope')
        self.verify(self.publish(root, 'legacy.inert'))
        parser = self.publish(self.unit(self.package / 'lib.rs'), 'parser.rlib')
        root['graph']['dependencies'] = [self.verify(parser)]
        with self.assertRaisesRegex(self.g.IdentityError, 'legacy compiler dependencies cross'):
            self.verify(self.publish(root, 'legacy-with-parser.inert'))

    def test_windows_final_sink_witness_expects_the_marker_of_a_fresh_unit(self):
        # candidate_identity_windows builds the final-sink child through this
        # same before(); its Rust witness must search the marker it embeds.
        unit = self.unit(self.repo / 'main.rs', kind='bin')
        marker = self.units.graph_marker(unit['graph'], unit['graph_sha256'])
        self.assertTrue(marker.startswith(self.units.LOCKED_GRAPH), marker)
        witness = (Path(__file__).resolve().parents[1] / 'crates/rmux-server/src/test_shell'
                   / 'final_sink/windows_byte_child.rs').read_text(encoding='utf-8')
        body = witness.split('fn the_generated_final_sink_child_has_candidate_identity_before_use(',
                             1)[1].split('\n    }\n', 1)[0]
        self.assertIn('b"%s"' % self.units.LOCKED_GRAPH.decode(), body)
        self.assertNotIn('b"%s"' % self.units.GRAPH.decode(), body)


class LockParse(unittest.TestCase):
    """S2099: each parent unit re-verifies its dependencies' lock closures.

    Parsing the same Cargo.lock bytes again for every transitive node made each
    rustc invocation cost grow with its dependency count.
    """
    setUp = LockClosure.setUp
    git = LockClosure.git
    unit = LockClosure.unit
    publish = LockClosure.publish
    verify = LockClosure.verify
    manifests = LockClosure.manifests

    def parses(self):
        toml = self.g.support('toml_reader')
        counted = Counter()
        original = toml.loads

        def loads(text):
            counted[text] += 1
            return original(text)

        return counted, patch.object(toml, 'loads', loads)

    def test_transitive_closure_parses_each_lock_text_once(self):
        counted, patched = self.parses()
        with patched:
            dependency = self.publish(self.unit(self.package / 'lib.rs'), 'dep.rlib')
            for _ in range(8):
                # Each parent unit re-verifies its dependency's lock closure.
                parent = self.unit(self.code, [dependency], 'bin')
                self.assertEqual(self.manifests(parent), [str(self.manifest)])
                proof = self.verify(dependency)
                self.assertEqual([c['manifest'] for c in proof['lock_contexts']],
                                 [str(self.manifest)])
        self.assertIn(self.lock.read_text(), counted)
        self.assertEqual(max(counted.values()), 1, counted.values())

    def test_changed_lock_bytes_are_parsed_and_refused(self):
        dependency = self.publish(self.unit(self.package / 'lib.rs'), 'dep.rlib')
        self.unit(self.code, [dependency], 'bin')
        self.lock.write_text(self.lock.read_text().replace('checksum="', 'checksum="0'))
        with self.assertRaises(self.g.IdentityError):
            self.unit(self.code, [dependency], 'bin')
        with self.assertRaises(self.g.IdentityError):
            self.verify(dependency)


if __name__ == '__main__':
    unittest.main()
