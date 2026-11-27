"""Parse the real Windows runner with the native argv parser, not Unix shlex."""
import ctypes
from ctypes import wintypes
import os
import subprocess
from types import SimpleNamespace
import unittest

import test_native_input_collector_consumer as fixtures


def windows_argv(command):
    api = ctypes.WinDLL('shell32', use_last_error=True)
    api.CommandLineToArgvW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_int)]
    api.CommandLineToArgvW.restype = ctypes.POINTER(wintypes.LPWSTR)
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    size = ctypes.c_int()
    result = api.CommandLineToArgvW(command, ctypes.byref(size))
    if not result:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return [result[i] for i in range(size.value)]
    finally:
        kernel.LocalFree(result)


if os.name == 'nt':
    # Change only how the fixture decodes the command string. Its frozen
    # expected argv, population and production subprocess module stay intact.
    fixtures.shlex = SimpleNamespace(split=windows_argv)


class WindowsRunnerParsing(unittest.TestCase):
    @unittest.skipUnless(os.name == 'nt', 'Windows native argv authority required')
    def test_native_parser_preserves_isolated_runner_paths_and_arguments(self):
        argv = [r'C:\qualified python\python.exe', '-I', '-S', '-B',
                r'C:\qualified candidate\candidate_identity.py', 'run']
        self.assertEqual(windows_argv(subprocess.list2cmdline(argv)), argv)


if __name__ == '__main__':
    unittest.main()
