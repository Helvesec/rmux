"""Native Windows process identity; no WSL/Unix process authority."""
import ctypes
from ctypes import wintypes as w
import os

kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
kernel.OpenProcess.restype = w.HANDLE
kernel.QueryFullProcessImageNameW.argtypes = [w.HANDLE, w.DWORD, w.LPWSTR, ctypes.POINTER(w.DWORD)]
kernel.GetProcessTimes.argtypes = [w.HANDLE, ctypes.POINTER(w.FILETIME), ctypes.POINTER(w.FILETIME), ctypes.POINTER(w.FILETIME), ctypes.POINTER(w.FILETIME)]
kernel.CloseHandle.argtypes = [w.HANDLE]


def windows_identity(pid):
    process = kernel.OpenProcess(0x1000, False, pid)
    if not process:
        return None
    try:
        image = ctypes.create_unicode_buffer(32768)
        length = w.DWORD(len(image))
        if not kernel.QueryFullProcessImageNameW(process, 0, image, ctypes.byref(length)):
            return None
        times = [w.FILETIME() for _ in range(4)]
        if not kernel.GetProcessTimes(process, *(ctypes.byref(t) for t in times)):
            return None
        if times[1].dwLowDateTime or times[1].dwHighDateTime:
            return None
        birth = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        return {'pid': pid, 'path': os.path.realpath(image.value), 'ps_lstart_argv': str(birth)}
    finally:
        kernel.CloseHandle(process)
