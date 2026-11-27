"""The consumer fixture must emit the same ticket keys on Windows and Unix."""
import unittest
from pathlib import Path, PureWindowsPath
from unittest.mock import patch
import test_candidate_consumer as fixture


class WindowsRelativePath(type(Path())):
    def relative_to(self, *args, **kwargs):
        return PureWindowsPath(super().relative_to(*args, **kwargs))


class ConsumerFixturePaths(unittest.TestCase):
    def test_windows_relative_path_preserves_observed_consumer(self):
        with patch.object(fixture, 'Path', WindowsRelativePath):
            fixture.GeneratedConsumer(
                'test_observed_consumer_input_is_accepted_in_its_private_output'\
            ).test_observed_consumer_input_is_accepted_in_its_private_output()


if __name__ == '__main__':
    unittest.main()
