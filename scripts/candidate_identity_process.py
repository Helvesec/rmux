"""Kernel process birth used to distinguish each Unix exec consumer."""
import ctypes
import os
from pathlib import Path
import sys


def current(g):
    pid = os.getpid()
    parent = os.getppid()
    if sys.platform == 'darwin':
        class BsdInfo(ctypes.Structure):
            _fields_ = [('prefix', ctypes.c_uint32 * 12), ('comm', ctypes.c_char * 16),
                        ('name', ctypes.c_char * 32), ('misc', ctypes.c_uint32 * 6),
                        ('seconds', ctypes.c_uint64), ('microseconds', ctypes.c_uint64)]
        info = BsdInfo()
        library = ctypes.CDLL('/usr/lib/libproc.dylib')
        library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                        ctypes.c_void_p, ctypes.c_int]
        size = ctypes.sizeof(info)
        if (library.proc_pidinfo(pid, 3, 0, ctypes.byref(info), size) != size
                or info.prefix[3] != pid):
            raise g.IdentityError('consumer process birth unavailable')
        parent = info.prefix[4]
        birth = dict(kind='darwin-proc-bsdinfo', seconds=info.seconds,
                     microseconds=info.microseconds)
    elif sys.platform == 'linux':
        fields = Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()
        parent = int(fields[1])
        birth = dict(kind='linux-proc-stat', ticks=int(fields[19]),
                     boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip())
    else:
        raise g.IdentityError('Unix consumer process identity unavailable')
    return dict(pid=pid, parent_pid=parent, birth=birth)
