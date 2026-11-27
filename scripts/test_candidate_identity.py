"""Frozen acceptance: python3 -m unittest discover -s scripts -p test_candidate_identity.py."""
import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('candidate_identity.py')


class CandidateIdentityAcceptance(unittest.TestCase):
    def test_reject_before_scenario_for_every_executable_role(self):
        self.assertTrue(SCRIPT.is_file(), 'candidate executable identity guard is absent')
        spec = importlib.util.spec_from_file_location('candidate_identity', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / 'candidate'
            source.mkdir()
            git = ['git', '-C', str(source)]
            subprocess.run(git + ['init', '-q'], check=True)
            subprocess.run(git + ['-c', 'user.name=Identity Test', '-c',
                           'user.email=identity@example.invalid', 'commit', '--allow-empty',
                           '-qm', 'candidate'], check=True)
            head = subprocess.check_output(git + ['rev-parse', 'HEAD'], text=True).strip()
            target = guard.target_directory(source, head)
            target.mkdir(parents=True)
            os_id = guard.host_os()
            source_id = hashlib.sha256(os.fsencode(str(source))).hexdigest()
            marker = (f'RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:{os_id}:END').encode()
            called = []
            for role in ('cli', 'daemon', 'helper', 'sdk', 'rust-test'):
                binary = target / role
                binary.write_bytes(b'fixture\0' + marker + b'\0')
                binary.chmod(0o755)
                result = guard.verify(binary, source, head, target)
                self.assertEqual(result['commit'], head)
                self.assertEqual(result['path'], str(binary.resolve()))
                self.assertEqual(result['sha256'], hashlib.sha256(binary.read_bytes()).hexdigest())
                for defect in ('missing', 'base', 'other-source', 'other-os', 'ambiguous', 'shared-target', 'outside-target'):
                    with self.subTest(role=role, defect=defect):
                        binary.write_bytes(b'fixture\0' + marker + b'\0')
                        chosen_target, chosen_binary = target, binary
                        if defect == 'missing':
                            binary.write_bytes(b'no embedded identity')
                        elif defect == 'base':
                            binary.write_bytes(marker.replace(head.encode(), b'0' * 40))
                        elif defect == 'other-source':
                            binary.write_bytes(marker.replace(source_id.encode(), b'0' * 64))
                        elif defect == 'other-os':
                            binary.write_bytes(marker.replace(os_id.encode(), b'foreign'))
                        elif defect == 'ambiguous':
                            binary.write_bytes(marker + marker.replace(head.encode(), b'0' * 40))
                        elif defect == 'shared-target':
                            chosen_target = root / 'shared'
                            chosen_target.mkdir(exist_ok=True)
                            chosen_binary = chosen_target / role
                            chosen_binary.write_bytes(marker)
                        elif defect == 'outside-target':
                            chosen_binary = root / role
                            chosen_binary.write_bytes(marker)
                        with self.assertRaises(guard.IdentityError):
                            guard.before_scenario([chosen_binary], source, head,
                                                  chosen_target, lambda: called.append(role))
            self.assertEqual(called, [], 'a rejected identity must never reach the scenario')


if __name__ == '__main__':
    unittest.main()
