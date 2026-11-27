"""Native regression: real Windows file sharing during probe message exchange."""
import ctypes
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest

from file_exchange import publish_json, read_json


@unittest.skipUnless(os.name == 'nt', 'requires Windows file sharing')
class FileExchange(unittest.TestCase):
    def test_publish_waits_for_reader_to_release_delete_sharing(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'request.json'
            publish_json(path, {'id': 1})
            held = path.open()
            def release():
                time.sleep(.1)
                held.close()
            thread = threading.Thread(target=release)
            thread.start()
            try:
                publish_json(path, {'id': 2})
            finally:
                thread.join()
            self.assertEqual(read_json(path), {'id': 2})

    def test_read_waits_for_exclusive_native_handle(self):
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint, ctypes.c_uint,
                                      ctypes.c_void_p, ctypes.c_uint, ctypes.c_uint, ctypes.c_void_p]
        kernel.CreateFileW.restype = ctypes.c_void_p
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        kernel.CloseHandle.restype = ctypes.c_int
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'reply.json'
            publish_json(path, {'id': 1})
            handle = kernel.CreateFileW(str(path), 0x80000000, 0, None, 3, 0, None)
            self.assertNotEqual(handle, ctypes.c_void_p(-1).value)
            def release():
                time.sleep(.1)
                kernel.CloseHandle(handle)
            thread = threading.Thread(target=release)
            thread.start()
            try:
                self.assertEqual(read_json(path), {'id': 1})
            finally:
                thread.join()


if __name__ == '__main__':
    unittest.main()
