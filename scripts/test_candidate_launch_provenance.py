"""Regression for R1 F1: an embedded tag alone never authorizes a launch."""
import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


class LaunchProvenance(unittest.TestCase):
    def test_unbuilt_or_mutated_payload_never_reaches_process(self):
        spec = importlib.util.spec_from_file_location('identity', Path(__file__).with_name('candidate_identity.py'))
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve()
            subprocess.run(['git', '-C', str(source), 'init', '-q'], check=True)
            subprocess.run(['git', '-C', str(source), '-c', 'user.name=Identity Test',
                            '-c', 'user.email=identity@example.invalid', 'commit',
                            '--allow-empty', '-qm', 'candidate'], check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            target = guard.target_directory(source, head, '0' * 32)
            target.mkdir(parents=True)
            source_id = hashlib.sha256(os.fsencode(str(source))).hexdigest()
            binary = target / 'unbuilt'
            marker = f'RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:{guard.host_os()}:END'.encode()
            env = dict(PATH=os.defpath, RMUX_IDENTITY_SOURCE=str(source),
                       RMUX_IDENTITY_HEAD=head, CARGO_TARGET_DIR=str(target),
                       RMUX_IDENTITY_TARGET=str(target))
            for payload in (b'arbitrary\0', b'modified\0'):
                binary.write_bytes(payload + marker)
                with patch.dict(os.environ, env, clear=True), patch.object(guard.subprocess, 'call') as process:
                    with self.assertRaises(guard.IdentityError):
                        guard.launch([str(binary)])
                    process.assert_not_called()


if __name__ == '__main__':
    unittest.main()
