"""Native Windows handles for the installed Job Object consumer."""
import ctypes
from ctypes import wintypes as w
import subprocess
import time

CREATE_SUSPENDED = 0x00000004


class IO_COUNTERS(ctypes.Structure):
    _fields_ = [(name, ctypes.c_ulonglong) for name in (
        'ReadOperationCount', 'WriteOperationCount', 'OtherOperationCount',
        'ReadTransferCount', 'WriteTransferCount', 'OtherTransferCount')]


class BASIC_LIMIT(ctypes.Structure):
    _fields_ = [('PerProcessUserTimeLimit', ctypes.c_longlong),
        ('PerJobUserTimeLimit', ctypes.c_longlong), ('LimitFlags', w.DWORD),
        ('MinimumWorkingSetSize', ctypes.c_size_t), ('MaximumWorkingSetSize', ctypes.c_size_t),
        ('ActiveProcessLimit', w.DWORD), ('Affinity', ctypes.c_size_t),
        ('PriorityClass', w.DWORD), ('SchedulingClass', w.DWORD)]


class EXTENDED_LIMIT(ctypes.Structure):
    _fields_ = [('BasicLimitInformation', BASIC_LIMIT), ('IoInfo', IO_COUNTERS),
        ('ProcessMemoryLimit', ctypes.c_size_t), ('JobMemoryLimit', ctypes.c_size_t),
        ('PeakProcessMemoryUsed', ctypes.c_size_t), ('PeakJobMemoryUsed', ctypes.c_size_t)]


class THREAD_ENTRY(ctypes.Structure):
    _fields_ = [('dwSize', w.DWORD), ('cntUsage', w.DWORD), ('th32ThreadID', w.DWORD),
        ('th32OwnerProcessID', w.DWORD), ('tpBasePri', w.LONG), ('tpDeltaPri', w.LONG),
        ('dwFlags', w.DWORD)]


class Native:
    def __init__(self):
        self.dll = ctypes.WinDLL('kernel32', use_last_error=True)
        signatures = {
            'CreateJobObjectW': ([ctypes.c_void_p, w.LPCWSTR], w.HANDLE),
            'SetInformationJobObject': ([w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD], w.BOOL),
            'AssignProcessToJobObject': ([w.HANDLE, w.HANDLE], w.BOOL),
            'CreateToolhelp32Snapshot': ([w.DWORD, w.DWORD], w.HANDLE),
            'Thread32First': ([w.HANDLE, ctypes.POINTER(THREAD_ENTRY)], w.BOOL),
            'Thread32Next': ([w.HANDLE, ctypes.POINTER(THREAD_ENTRY)], w.BOOL),
            'OpenThread': ([w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            'ResumeThread': ([w.HANDLE], w.DWORD),
            'OpenProcess': ([w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            'WaitForSingleObject': ([w.HANDLE, w.DWORD], w.DWORD),
            'QueryFullProcessImageNameW': ([w.HANDLE, w.DWORD, w.LPWSTR, ctypes.POINTER(w.DWORD)], w.BOOL),
            'CloseHandle': ([w.HANDLE], w.BOOL),
            'TerminateProcess': ([w.HANDLE, w.UINT], w.BOOL),
        }
        for name, (args, result) in signatures.items():
            function = getattr(self.dll, name)
            function.argtypes, function.restype = args, result

    @staticmethod
    def require(result):
        if not result:
            raise ctypes.WinError(ctypes.get_last_error())
        return result

    def resume(self, pid):
        snapshot = self.dll.CreateToolhelp32Snapshot(4, 0)
        if snapshot == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            entry = THREAD_ENTRY()
            entry.dwSize = ctypes.sizeof(entry)
            more = self.dll.Thread32First(snapshot, ctypes.byref(entry))
            while more:
                if entry.th32OwnerProcessID == pid:
                    thread = self.require(self.dll.OpenThread(2, False, entry.th32ThreadID))
                    try:
                        if self.dll.ResumeThread(thread) == 0xffffffff:
                            raise ctypes.WinError(ctypes.get_last_error())
                        return
                    finally:
                        self.dll.CloseHandle(thread)
                more = self.dll.Thread32Next(snapshot, ctypes.byref(entry))
            raise ValueError('suspended launcher primary thread not found')
        finally:
            self.dll.CloseHandle(snapshot)

    def process(self, pid):
        return Process(self, self.require(self.dll.OpenProcess(0x100000 | 0x1000 | 1, False, pid)))


class Process:
    def __init__(self, native, handle):
        self.native, self.handle = native, handle

    def image(self):
        text = ctypes.create_unicode_buffer(32768)
        size = w.DWORD(len(text))
        self.native.require(self.native.dll.QueryFullProcessImageNameW(
            self.handle, 0, text, ctypes.byref(size)))
        return text.value

    def exited(self, timeout=0):
        result = self.native.dll.WaitForSingleObject(self.handle, int(timeout * 1000))
        if result not in (0, 258):
            raise ctypes.WinError(ctypes.get_last_error())
        return result == 0

    def terminate(self):
        if not self.exited():
            self.native.require(self.native.dll.TerminateProcess(self.handle, 1))
            if not self.exited(5):
                raise ValueError('owned witness process did not terminate')

    def close(self):
        if self.handle:
            self.native.dll.CloseHandle(self.handle)
            self.handle = None


class Job:
    def __init__(self, native, allow_breakaway):
        self.native = native
        self.handle = native.require(native.dll.CreateJobObjectW(None, None))
        limits = EXTENDED_LIMIT()
        limits.BasicLimitInformation.LimitFlags = 0x2000 | (0x800 if allow_breakaway else 0)
        try:
            native.require(native.dll.SetInformationJobObject(
                self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)))
        except BaseException:
            self.close()
            raise

    def launch(self, argv, env, directory, log, timeout=20):
        # CPython retains the process HANDLE but closes the primary thread.
        # Resume through Toolhelp only after assignment to the containing job.
        child = subprocess.Popen(argv, env=env, cwd=directory, stdin=subprocess.DEVNULL,
            stdout=log, stderr=log, creationflags=CREATE_SUSPENDED)
        try:
            self.native.require(self.native.dll.AssignProcessToJobObject(self.handle, int(child._handle)))
            self.native.resume(child.pid)
            return child.wait(timeout=timeout)
        except BaseException:
            child.kill()
            child.wait(timeout=5)
            raise

    def close(self):
        if self.handle:
            self.native.dll.CloseHandle(self.handle)
            self.handle = None
