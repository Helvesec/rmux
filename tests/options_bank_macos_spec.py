"""Native acceptance for the existing options bank's macOS process boundary."""
import errno
import os
from pathlib import Path
import sys
import subprocess
import unittest
from unittest import mock

import options_bank_linux as adapter


@unittest.skipUnless(sys.platform == 'darwin', 'macOS native authority required')
class MacProcessAcceptance(unittest.TestCase):
    def test_native_process_birth_image_and_descriptors(self):
        birth = adapter.identity(os.getpid())
        self.assertIsNotNone(birth)
        self.assertEqual(birth['ppid'], os.getppid())
        self.assertTrue(adapter.same(birth, adapter.identity(os.getpid())))
        process = adapter.describe(os.getpid())
        self.assertEqual(Path(process['exe']).resolve(), Path(sys.executable).resolve())
        self.assertEqual(process['exe_sha256'], adapter.sha(sys.executable))
        self.assertEqual(process['cwd'], str(Path.cwd()))
        self.assertIn('0', adapter.fds())

    def test_incomplete_argv_is_retried_on_the_same_live_birth(self):
        import options_bank_macos as native
        child = subprocess.Popen(['/bin/sleep', '30'], stdin=subprocess.DEVNULL)
        try:
            valid = native.darwin.procargs(child.pid)
            with mock.patch.object(native.darwin, 'procargs', side_effect=[
                    OSError(errno.EIO, 'native argv not ready'), valid]):
                observed = native.describe(child.pid)
            self.assertEqual(observed['argv'], ['/bin/sleep', '30'])
            self.assertEqual(observed['identity']['pid'], child.pid)
        finally:
            child.terminate()
            child.wait()

    def test_native_shell_consumer_accepts_both_argv0_spellings(self):
        from options_bank_linux_spec import fixture
        registry, contract = fixture()
        registry['target_os'] = 'macos'
        contract.update(native_os='macos', python={'path': sys.executable})
        plan, _ = adapter.per_option_plan(registry, contract, ['default-shell'])
        for executable, role in [('/bin/bash', 'create-consumer'),
                                 ('/bin/sh', 'local-effect-create')]:
            row = next(item for item in plan if item['role'] == 'default-shell:' + role)
            payload = row['argv'][-1].replace('exec /bin/sleep', 'printf %s')
            for spelling in [executable, Path(executable).name]:
                with self.subTest(executable=executable, argv0=spelling):
                    actual = subprocess.check_output([spelling, '-c', payload],
                                                     executable=executable, text=True, timeout=2)
                    self.assertEqual(actual, row['consumer_argv'][-1])

    def test_invalid_procargs_during_exec_keeps_the_live_birth(self):
        import options_bank_macos as native
        child = subprocess.Popen(['/bin/sleep', '30'], stdin=subprocess.DEVNULL)
        try:
            valid = native.darwin.procargs(child.pid)
            with mock.patch.object(native.darwin, 'procargs', side_effect=[
                    OSError(errno.EINVAL, 'native exec argv incomplete'), valid]):
                observed = native.describe(child.pid)
            self.assertEqual(observed['argv'], ['/bin/sleep', '30'])
            self.assertEqual(observed['identity']['pid'], child.pid)
        finally:
            child.terminate()
            child.wait()


if __name__ == '__main__':
    unittest.main()
