"""Publication must retain the actual compiler output and immutable observation."""
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
import candidate_identity as identity
import candidate_identity_units as units
import candidate_identity_windows as windows


class PublicationHistory(unittest.TestCase):
    def test_published_fixture_preserves_the_original_completed_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            target = source / 'target'
            target.mkdir()
            (source / 'Cargo.lock').write_text('version=4\n')
            compiler = source / 'compiler'
            compiler.write_bytes(b'compiler observation fixture')
            partial = target / 'observed.partial'
            program = target / 'published'
            graph = dict(head='head', source=str(source), compiler=str(compiler),
                         compiler_sha256=identity.file_hash(compiler),
                         lock_sha256=identity.file_hash(source / 'Cargo.lock'),
                         dependencies=[], generation=None)
            graph_sha = units.digest(graph)
            partial.write_bytes(b'publication fixture:' + units.GRAPH + graph_sha.encode() + b':END')
            sha = identity.file_hash(partial)
            raw = dict(path=str(partial), compiler_output=str(partial), sha256=sha,
                       kind='bin', graph=graph, graph_sha256=graph_sha)
            unit_dir = target / 'identity-units'
            guard = SimpleNamespace(IdentityError=identity.IdentityError,
                                    file_hash=identity.file_hash, stamp=identity.stamp)
            original = units.preserve(guard, unit_dir, raw)
            original_bytes = original.read_bytes()
            (unit_dir / (sha + '.json')).write_bytes(original_bytes)
            build_dir = target / 'identity-builds'
            build_dir.mkdir()
            build = build_dir / (sha + '.json')
            build.write_text(json.dumps(dict(path=str(partial), sha256=sha)) + '\n')
            def verify(path, *_):
                unit = units.verify(guard, path, source, 'head', target)
                return dict(path=str(path), sha256=sha, build=str(build), unit=unit)
            guard.verify_built = verify
            guard.support = identity.support
            record = verify(partial)
            windows.publish(guard, partial, program, record, source, 'head', target)
            self.assertEqual(partial.read_bytes(), program.read_bytes())
            self.assertEqual(original.read_bytes(), original_bytes)
            units.verify(guard, partial, source, 'head', target, receipt=original)
            verify(program)


if __name__ == '__main__':
    unittest.main()
