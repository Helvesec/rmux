"""One qualification reads each image once, without sharing trust with consumers."""
import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace


@unittest.skipIf(os.name == 'nt', 'Unix qualification byte witnesses')
class QualificationReads(unittest.TestCase):
    def test_nested_compiler_lookup_rejects_a_sibling_receipt(self):
        import candidate_identity as identity

        units = identity.support('candidate_identity_units')
        cache = identity.support('candidate_identity_cache')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'Cargo.lock').write_bytes(b'locked')
            compiler = root / 'compiler'
            compiler.write_bytes(b'inert compiler fixture')
            guard = SimpleNamespace(**vars(identity))
            paths = [root / name / 'libsame.rmeta' for name in ('first', 'sibling')]
            for path in paths:
                path.parent.mkdir()
                path.write_bytes(b'identical metadata')
                graph = dict(head='head', source=str(root), input={'input': 'same'},
                             compiler=str(compiler), compiler_sha256=identity.file_hash(compiler),
                             lock_sha256=identity.file_hash(root / 'Cargo.lock'),
                             arguments=['--out-dir', str(path.parent)],
                             dependencies=[], generation=None)
                unit = dict(outputs=[path], stamps={}, kind='library', graph=graph,
                            graph_sha256=units.digest(graph))
                with patch.object(units, 'check_depinfo'):
                    units.after(guard, unit, root, 'head', root)

            def verify():
                return units.verify(guard, paths[0], root, 'head', root)
            def cached():
                return cache.cached(guard, 'unit-test', paths[0], root, 'head', root, verify)
            self.assertEqual(verify(), cached())
            self.assertEqual(verify(), cached())
            sha = identity.file_hash(paths[0])
            aliases = [root / 'identity-units' / (units.compiler_receipt_key(p, sha) + '.json')
                       for p in paths]
            original = aliases[0].read_bytes()
            aliases[0].write_bytes(aliases[1].read_bytes())
            for check in (verify, cached):
                with self.assertRaisesRegex(identity.IdentityError, 'another output'):
                    check()
            # A missing output index cannot fall back to the sibling's valid
            # equal-byte observation through the legacy SHA index either.
            aliases[0].unlink()
            for check in (verify, cached):
                with self.assertRaisesRegex(identity.IdentityError, 'another output'):
                    check()
            aliases[0].write_bytes(original)
            self.assertEqual(verify(), cached())

    def test_one_qualification_reuses_bytes_and_rejects_mutation(self):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = script.parent.parent / '.rmux-audit' / 's3-044' / 'qualification-tests'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        code = source / 'main.rs'
        code.write_text('fn main() {}\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        compiler = source / 'compiler'
        compiler.write_bytes(b'qualified fixture compiler')
        git = ['git', '-C', str(source)]
        for args in (['init', '-q'], ['add', '.'],
                     ['-c', 'user.name=Identity Test', '-c',
                      'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
            subprocess.run(git + args, check=True)
        head = guard.git(source, 'rev-parse', 'HEAD')
        target = guard.target_directory(source, head, '0' * 32)
        target.mkdir(parents=True)
        units = guard.support('candidate_identity_units')
        g = SimpleNamespace(**vars(guard))
        dependency = target / 'dependency.rlib'
        dependency.write_bytes(b'transitive fixture bytes')
        generated = target / 'generated-observation.json'
        generated.write_bytes(b'{"files": {}}\n')
        def unit(path, kind, graph):
            graph_sha = units.digest(graph)
            if kind == 'bin':
                source_id = hashlib.sha256(str(source).encode()).hexdigest()
                path.write_bytes((f'RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:{guard.host_os()}:END\n'
                                  f'RMUX_BUILD_GRAPH_V1:{graph_sha}:END\n').encode())
            record = dict(path=str(path), sha256=guard.file_hash(path), kind=kind,
                          graph=graph, graph_sha256=graph_sha)
            receipt = units.preserve(g, target / 'identity-units', record)
            (receipt.parent / (record['sha256'] + '.json')).write_bytes(receipt.read_bytes())
            return dict(path=str(path), sha256=record['sha256'], receipt=str(receipt),
                        receipt_sha256=guard.file_hash(receipt))
        graph = dict(head=head, source=str(source), compiler=str(compiler),
                     compiler_sha256=guard.file_hash(compiler),
                     lock_sha256=guard.file_hash(source / 'Cargo.lock'), dependencies=[])
        dep = unit(dependency, 'library', graph)
        binary = target / 'binary'
        unit(binary, 'bin', dict(graph, dependencies=[dep, dep],
             generation=dict(receipt=str(generated), receipt_sha256=guard.file_hash(generated))))
        built = guard.verify(binary, source, head, target)
        built.update(input=str(code), input_sha256=guard.file_hash(code),
                     compiler=str(compiler), compiler_sha256=guard.file_hash(compiler),
                     exit=0, kind='bin')
        guard.support('candidate_identity_retention').publish(g, target, built)
        def qualify_built():
            return guard.support('candidate_identity_retention').verify_built(
                g, binary, source, head, target)
        original = Path.read_bytes
        observed = (binary, compiler, dependency, generated)
        def qualify(action):
            reads = {}
            def read(path):
                reads[path] = reads.get(path, 0) + 1
                return original(path)
            with patch.object(Path, 'read_bytes', read):
                result = action()
            return result, reads
        for consumer in range(2):
            result, reads = qualify(qualify_built)
            self.assertEqual(result['sha256'], built['sha256'])
            for path in observed:
                self.assertEqual(reads.get(path), 1,
                                 f'{path.name}: redundant reads in consumer {consumer}')
        for action in (lambda: guard.verify(binary, source, head, target),
                       lambda: units.verify(g, binary, source, head, target)):
            _, reads = qualify(action)
            self.assertEqual(reads[binary], 1, 'sibling qualification rereads image bytes')
        image = original(binary)
        for defect in ('bytes', 'stamp'):
            armed = [True]
            def changed(path):
                data = original(path)
                if path == compiler and armed[0]:
                    armed[0] = False
                    if defect == 'bytes':
                        info = binary.stat()
                        binary.write_bytes(image.replace(b'BUILD_IDENTITY', b'BUILD_IDENTITZ'))
                        import os
                        os.utime(binary, ns=(info.st_atime_ns, info.st_mtime_ns))
                    else:
                        binary.chmod(0o700)
                return data
            binary.chmod(0o600)
            with patch.object(Path, 'read_bytes', changed):
                with self.assertRaises(guard.IdentityError, msg=defect):
                    qualify_built()
            binary.write_bytes(image)
        for path in (binary, code, compiler, dependency, generated):
            data = original(path)
            path.write_bytes(data + b'changed')
            with self.assertRaises(guard.IdentityError, msg=path.name):
                qualify_built()
            path.write_bytes(data)
            qualify_built()
        dep_bytes = original(dependency)
        def changed_dependency(path):
            data = original(path)
            if path == dependency:
                dependency.write_bytes(dep_bytes + b'changed')
            return data
        with patch.object(Path, 'read_bytes', changed_dependency):
            with self.assertRaises(guard.IdentityError):
                qualify_built()


if __name__ == '__main__':
    unittest.main()
