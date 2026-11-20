"""Recompilation must preserve the receipt actually consumed by an older parent."""
import hashlib
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class ProducerHistory(unittest.TestCase):
    @unittest.skipIf(os.name == 'nt', 'Unix nested compiler receipt publication')
    def test_nested_compilers_keep_their_receipts_during_sibling_publication(self):
        import candidate_identity as identity

        units = identity.support('candidate_identity_units')
        cache = identity.support('candidate_identity_cache')
        for cached in (False, True):
            with self.subTest(cached=cached), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                (root / 'Cargo.lock').write_bytes(b'locked')
                compiler = root / 'compiler'
                compiler.write_bytes(b'inert compiler fixture')
                guard = SimpleNamespace(**vars(identity))
                outputs = [root / 'tmp' / f'xtask-completion-surface-{pid}' /
                           'target/debug/deps/libmemchr-975157c7154e4dab.rmeta'
                           for pid in (1, 2)]

                def produce(path):
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b'identical metadata from independent nested compilers')
                    graph = dict(head='head', source=str(root), input={'input': 'memchr'},
                                 compiler=str(compiler), compiler_sha256=identity.file_hash(compiler),
                                 lock_sha256=identity.file_hash(root / 'Cargo.lock'),
                                 arguments=['--out-dir', str(path.parent)],
                                 dependencies=[], generation=None)
                    unit = dict(outputs=[path], stamps={}, kind='library',
                                graph=graph, graph_sha256=units.digest(graph))
                    with patch.object(units, 'check_depinfo'):
                        units.after(guard, unit, root, 'head', root)
                    return units.digest(graph)

                first_graph = produce(outputs[0])
                first = units.verify(guard, outputs[0], root, 'head', root)
                sibling_graph = []

                def interleave(path):
                    # The first receipt is already witnessed when its compiler
                    # is checked. Complete the other nested compilation here.
                    if Path(path) == compiler and not sibling_graph:
                        sibling_graph.append(produce(outputs[1]))
                    return identity.file_hash(path)

                checking = SimpleNamespace(**vars(guard))
                checking.file_hash = interleave
                def verify_first():
                    return units.verify(checking, outputs[0], root, 'head', root)
                result = (cache.cached(checking, 'unit-test', outputs[0], root, 'head', root,
                                       verify_first) if cached else verify_first())
                self.assertEqual(len(sibling_graph), 1)
                self.assertNotEqual(first_graph, sibling_graph[0])
                self.assertEqual(result, first)
                self.assertEqual(units.verify(guard, outputs[0], root, 'head', root), first)
                second = units.verify(guard, outputs[1], root, 'head', root)
                self.assertEqual(second['graph_sha256'], sibling_graph[0])
                self.assertNotEqual(first['receipt'], second['receipt'])
                self.assertEqual(first['sha256'], second['sha256'])

    def check_retained_parent(self, changed=False):
        spec = importlib.util.spec_from_file_location(
            'units', Path(__file__).with_name('candidate_identity_units.py'))
        units = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(units)
        retention_spec = importlib.util.spec_from_file_location(
            'retention', Path(__file__).with_name('candidate_identity_retention.py'))
        retention = importlib.util.module_from_spec(retention_spec)
        retention_spec.loader.exec_module(retention)
        manifest_spec = importlib.util.spec_from_file_location(
            'manifests', Path(__file__).with_name('candidate_identity_manifest.py'))
        manifests = importlib.util.module_from_spec(manifest_spec)
        manifest_spec.loader.exec_module(manifests)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'Cargo.lock').write_bytes(b'locked')
            compiler = Path(sys.executable).resolve()
            guard = SimpleNamespace(
                file_hash=lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest(),
                stamp=lambda st: (st.st_size, st.st_mtime_ns), IdentityError=RuntimeError,
                support=lambda name: {'candidate_identity_retention': retention,
                                      'candidate_identity_manifest': manifests}[name])

            def produce(path, data, input_name, dependencies):
                path.write_bytes(data)
                graph = dict(head='head', source=str(root), input={'input': input_name},
                             compiler=str(compiler), compiler_sha256=guard.file_hash(compiler),
                             lock_sha256=guard.file_hash(root / 'Cargo.lock'),
                             dependencies=dependencies, generation=None)
                unit = dict(outputs=[path], stamps={}, kind='library',
                            graph=graph, graph_sha256=units.digest(graph))
                with patch.object(units, 'check_depinfo'):
                    units.after(guard, unit, root, 'head', root)
                return units.verify(guard, path, root, 'head', root)

            dependency = root / 'dependency.rlib'
            first = produce(dependency, b'same compiled library', 'producer-one', [])
            parent = root / 'parent.rlib'
            produce(parent, b'older retained parent', 'parent', [first])
            second = produce(dependency, b'changed bytes' if changed else b'same compiled library',
                             'producer-two', [])
            self.assertNotEqual(first['graph_sha256'], second['graph_sha256'])
            if changed:
                # Mutate the actual bytes bound by the older parent, preserving
                # the same negative assertion now that aliases may be rebuilt.
                witnessed = Path(first['path'])
                witnessed.chmod(0o700)
                witnessed.write_bytes(b'changed witnessed dependency')
                with self.assertRaisesRegex(RuntimeError, 'linked unit bytes changed'):
                    units.verify(guard, parent, root, 'head', root)
            else:
                units.verify(guard, parent, root, 'head', root)
                self.assertNotEqual(first['receipt'], second['receipt'])
                self.assertEqual(guard.file_hash(first['receipt']), first['receipt_sha256'])

    def test_shared_dependency_mutated_between_parent_visits_is_rejected(self):
        spec = importlib.util.spec_from_file_location(
            'units', Path(__file__).with_name('candidate_identity_units.py'))
        units = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(units)
        retention_spec = importlib.util.spec_from_file_location(
            'retention', Path(__file__).with_name('candidate_identity_retention.py'))
        retention = importlib.util.module_from_spec(retention_spec)
        retention_spec.loader.exec_module(retention)
        manifest_spec = importlib.util.spec_from_file_location(
            'manifests', Path(__file__).with_name('candidate_identity_manifest.py'))
        manifests = importlib.util.module_from_spec(manifest_spec)
        manifest_spec.loader.exec_module(manifests)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'Cargo.lock').write_bytes(b'locked')
            compiler = Path(sys.executable).resolve()
            guard = SimpleNamespace(
                file_hash=lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest(),
                stamp=lambda st: (st.st_size, st.st_mtime_ns), IdentityError=RuntimeError,
                support=lambda name: {'candidate_identity_retention': retention,
                                      'candidate_identity_manifest': manifests}[name])

            def produce(name, dependencies):
                path = root / (name + '.rlib')
                path.write_bytes(name.encode())
                graph = dict(head='head', source=str(root), input={'input': name},
                             compiler=str(compiler), compiler_sha256=guard.file_hash(compiler),
                             lock_sha256=guard.file_hash(root / 'Cargo.lock'),
                             dependencies=dependencies, generation=None)
                unit = dict(outputs=[path], stamps={}, kind='library',
                            graph=graph, graph_sha256=units.digest(graph))
                with patch.object(units, 'check_depinfo'):
                    units.after(guard, unit, root, 'head', root)
                return units.verify(guard, path, root, 'head', root)

            leaf = produce('leaf', [])
            left = produce('left', [leaf])
            right = produce('right', [leaf])
            parent = produce('parent', [left, right])
            original_hash = guard.file_hash
            mutated = False

            def mutate_between_visits(path):
                nonlocal mutated
                if Path(path).resolve() == Path(right['path']) and not mutated:
                    actual_leaf = Path(leaf['path'])
                    actual_leaf.chmod(0o700)
                    actual_leaf.write_bytes(b'changed after left parent checked it')
                    mutated = True
                return original_hash(path)

            guard.file_hash = mutate_between_visits
            with self.assertRaisesRegex(RuntimeError, 'linked unit bytes changed'):
                units.verify(guard, parent['path'], root, 'head', root)
            self.assertTrue(mutated)

    def test_older_parent_keeps_exact_observed_receipt_for_identical_bytes(self):
        self.check_retained_parent()

    def test_history_does_not_authorize_changed_dependency_bytes(self):
        self.check_retained_parent(changed=True)


if __name__ == '__main__':
    unittest.main()
