"""First-use siblings and Windows adapters for the unchanged frozen assertions."""
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

import candidate_identity as guard
import test_native_input_collector_consumer as fixtures


# The frozen runner assertion uses shlex to inspect a command generated with
# Windows quoting. An absolute forward-slash spelling is the same Python image
# and is understood by both parsers, without changing its arguments or bytes.
# Its deliberately corruptible snapshot stand-in also needs writable objects:
# Windows rejects unlink of a readonly file before the refusal under test.
# These adapters affect only that module's fixture; production uses new_snapshot.
if os.name == 'nt':
    sys.executable = Path(sys.executable).as_posix()
    class CorruptibleSnapshot(fixtures.Snapshot):
        def retain(self, path):
            super().retain(path)
            Path(self.files[str(Path(path).absolute())]['object']).chmod(0o600)
    fixtures.Snapshot = CorruptibleSnapshot


class ConsumerFirstUseSiblings(unittest.TestCase):
    setUp = fixtures.ConsumerRetention.setUp
    assert_receipt = fixtures.ConsumerRetention.assert_receipt
    run_action = fixtures.ConsumerRetention.run_action

    def test_replacement_during_metadata_is_refused(self):
        # The consumer action receives its retention module from guard.support(),
        # a private per-process copy, not the module that `import` would bind.
        retention = guard.support('native_input_windows_consumer')
        real_retain = retention.retain
        phases = []
        def retain(g, ticket, paths, phase):
            phases.append(phase)
            if phase == 'resolved':
                other = self.directory / 'replacement.rs'
                other.write_bytes(self.code.read_bytes())
                other.replace(self.code)
            return real_retain(g, ticket, paths, phase)
        # The replacement happens after the initial check but before the new
        # lock version is retained. It must not become a new source authority.
        with patch.object(retention, 'retain', side_effect=retain), \
                self.assertRaises((guard.IdentityError, RuntimeError)):
            self.run_action()
        self.assertEqual(phases, ['initial', 'resolved'], 'replacement was never injected')

    def test_dropped_native_session_is_refused(self):
        def drop(ticket):
            os.environ.pop('RMUX_NATIVE_INPUT_MANIFEST')
        with self.assertRaises(guard.IdentityError):
            self.run_action(drop)

    def test_final_sink_python_entry_is_isolated_before_script(self):
        text = (Path(guard.__file__).parent.parent /
                'crates/rmux-server/src/test_shell/final_sink/windows_byte_child.rs').read_text()
        body = text.split('fn identity_action(', 1)[1].split('fn verify_program(', 1)[0]
        self.assertIn('RMUX_NATIVE_INPUT_MANIFEST', body)
        self.assertIn('["-I", "-S", "-B"]', body)
        self.assertLess(body.index('["-I", "-S", "-B"]'), body.index('.arg(script)'))


if __name__ == '__main__':
    unittest.main()
