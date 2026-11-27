"""R2: snapshots have no buffer size ceiling; buffers retain their ceiling.

Run after cargo build --locked --bins. All files remain in target-audit.
RMUX_TEST_BINARY selects the binary before correction.
"""
import unittest

import test_snapshot_file_reads as fixture

BUFFER_LIMIT = 64 * 1024 * 1024
CHUNK = 1024 * 1024


class LargeSnapshotReads(fixture.SnapshotFileReads):
    def large_snapshot(self, suffix):
        payload = self.success(self.run_cli(
            'export-window-snapshot' + suffix, '-t', 'probe:0'))
        path = self.directory / ('large' + suffix + '.json')
        with path.open('wb') as output:
            output.write(payload.encode())
            for _ in range(BUFFER_LIMIT // CHUNK):
                output.write(b' ' * CHUNK)
        self.assertGreater(path.stat().st_size, BUFFER_LIMIT)
        return path

    def check_large_regular(self, suffix):
        path = self.large_snapshot(suffix)
        command = 'restore-window-snapshot' + suffix
        config = self.config(f'{command} -t probe:0 {path}\n')
        for route, argv in (
                ('source', ('source-file', config)),
                ('queue', ('display-message', '-p', 'ready', ';', command,
                           '-t', 'probe:0', str(path))),
                ('direct', (command, '-t', 'probe:0', str(path)))):
            with self.subTest(route=route):
                self.success(self.run_cli(*argv))

    def test_large_regular_v1(self):
        self.check_large_regular('')

    def test_large_regular_v2(self):
        self.check_large_regular('-v2')

    def test_buffers_still_reject_oversized_regular_and_device_sources(self):
        path = self.large_snapshot('')
        for kind, source in (('regular', str(path)), ('device', '/dev/zero')):
            with self.subTest(kind=kind):
                result = self.run_cli('load-buffer', source)
                self.assertEqual(result[0], 1, result)
                self.assertIn(f'buffer file exceeds the {BUFFER_LIMIT}-byte limit',
                              result[1] + result[2])
                self.success(self.run_cli('list-sessions'))


if __name__ == '__main__':
    unittest.main(verbosity=2)
