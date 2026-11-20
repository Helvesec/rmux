"""Bounded kernel observations; a copied DLL is never reported as a loaded DLL."""
import ctypes
from ctypes import wintypes as w
from pathlib import Path
import time


class ProcessEntry(ctypes.Structure):
    _fields_ = [('size', w.DWORD), ('usage', w.DWORD), ('pid', w.DWORD),
                ('heap', ctypes.c_size_t), ('module', w.DWORD), ('threads', w.DWORD),
                ('parent', w.DWORD), ('priority', w.LONG), ('flags', w.DWORD),
                ('name', w.WCHAR * 260)]


class MemoryCounters(ctypes.Structure):
    _fields_ = [('size', w.DWORD), ('faults', w.DWORD),
                *[(name, ctypes.c_size_t) for name in (
                    'peak_working', 'working', 'peak_paged', 'paged',
                    'peak_nonpaged', 'nonpaged', 'pagefile', 'peak_pagefile', 'private')]]


def api():
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    kernel.OpenProcess.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    kernel.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = w.HANDLE
    kernel.Process32FirstW.argtypes = [w.HANDLE, ctypes.POINTER(ProcessEntry)]
    kernel.Process32NextW.argtypes = [w.HANDLE, ctypes.POINTER(ProcessEntry)]
    kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
    kernel.WaitForSingleObject.restype = w.DWORD
    kernel.GetCurrentProcess.restype = w.HANDLE
    psapi = ctypes.WinDLL('psapi', use_last_error=True)
    psapi.EnumProcessModulesEx.argtypes = [w.HANDLE, ctypes.POINTER(w.HMODULE),
                                         w.DWORD, ctypes.POINTER(w.DWORD), w.DWORD]
    psapi.GetModuleFileNameExW.argtypes = [w.HANDLE, w.HMODULE, w.LPWSTR, w.DWORD]
    psapi.GetProcessMemoryInfo.argtypes = [w.HANDLE, ctypes.POINTER(MemoryCounters), w.DWORD]
    return kernel, psapi


def processes(kernel):
    snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
    if snapshot == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    entry = ProcessEntry()
    entry.size = ctypes.sizeof(entry)
    try:
        more = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
        while more:
            yield entry.pid, entry.parent
            more = kernel.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)


def modules(psapi, handle):
    # A changing loader list may race a sample. Later samples can prove a load;
    # a missed load is never upgraded to a positive observation.
    size = 256
    while size <= 8192:
        items, needed = (w.HMODULE * size)(), w.DWORD()
        if not psapi.EnumProcessModulesEx(handle, items, ctypes.sizeof(items),
                                          ctypes.byref(needed), 3):
            return
        if needed.value > ctypes.sizeof(items):
            size *= 2
            continue
        for module in items[:needed.value // ctypes.sizeof(w.HMODULE)]:
            buffer = ctypes.create_unicode_buffer(32768)
            if psapi.GetModuleFileNameExW(handle, module, buffer, len(buffer)):
                if Path(buffer.value).name.casefold() == 'conpty.dll':
                    yield Path(buffer.value).resolve(strict=True)
        return
    raise RuntimeError('ConPTY observation module bound exceeded')


def observe(g, process, child, target, prepared):
    kernel, psapi = api()
    native = g.support('native_input_windows_process')
    runtime = g.support('candidate_conpty_runtime')
    manifest, _ = runtime.manifest(g)
    hashes = {item['name'].casefold(): item['sha256'] for item in manifest['files']}
    owned = {child['pid']: (process._handle, child)}
    seen, loads, hosts, errors = set(), [], [], []
    try:
        while True:
            memory = MemoryCounters()
            memory.size = ctypes.sizeof(memory)
            if not psapi.GetProcessMemoryInfo(kernel.GetCurrentProcess(), ctypes.byref(memory), memory.size):
                raise ctypes.WinError(ctypes.get_last_error())
            if memory.private > 4 * 1024**3:
                raise RuntimeError('ConPTY observer memory limit exceeded')
            # Stream the process table. Open only live descendants of our exact
            # handles, then use births to exclude recycled parent/child PIDs.
            for pid, parent in processes(kernel):
                if pid in owned or parent not in owned:
                    continue
                parent_handle, parent_identity = owned[parent]
                if kernel.WaitForSingleObject(parent_handle, 0) != 0x102:
                    continue
                # Every handle tested with WaitForSingleObject needs
                # SYNCHRONIZE. Query/VM-read rights alone produce WAIT_FAILED
                # and would silently exclude live OpenConsole descendants.
                handle = kernel.OpenProcess(0x101410, False, pid)
                if not handle:
                    # Console hosts may deny VM reads while still allowing
                    # image/birth queries and a lifetime wait on this handle.
                    handle = kernel.OpenProcess(0x101000, False, pid)
                if not handle:
                    continue
                try:
                    identity = native.identity(handle)
                    if identity['birth_ns'] < parent_identity['birth_ns']:
                        continue
                    identity['parent_pid'] = parent
                    identity['parent_birth_ns'] = parent_identity['birth_ns']
                    if len(owned) >= 4096:
                        raise RuntimeError('ConPTY observation process bound exceeded')
                    owned[pid] = (handle, identity)
                    handle = None
                except OSError:
                    pass  # Process disappeared before it could be qualified.
                finally:
                    if handle:
                        kernel.CloseHandle(handle)
            for pid, (handle, identity) in tuple(owned.items()):
                if kernel.WaitForSingleObject(handle, 0) != 0x102:
                    continue
                image = Path(identity['image'])
                if image.name.casefold() == 'openconsole.exe':
                    key = (pid, identity['birth_ns'], str(image))
                    if key not in seen:
                        runtime.checked(g, image, hashes['openconsole.exe'])
                        if not image.is_relative_to(target):
                            raise RuntimeError('ConPTY host outside private target')
                        seen.add(key)
                        hosts.append(dict(process=identity, path=str(image),
                                          sha256=hashes['openconsole.exe'], at_ns=time.time_ns()))
                    continue
                for dll in modules(psapi, handle):
                    key = (pid, identity['birth_ns'], str(dll))
                    if key in seen:
                        continue
                    if prepared['configuration'] == 'system':
                        raise RuntimeError('system configuration loaded companion ConPTY')
                    if dll != image.with_name('conpty.dll') or not dll.is_relative_to(target):
                        raise RuntimeError('loaded ConPTY is not adjacent to candidate image')
                    runtime.checked(g, dll, hashes['conpty.dll'])
                    seen.add(key)
                    loads.append(dict(process=identity, path=str(dll),
                                      sha256=hashes['conpty.dll'], at_ns=time.time_ns()))
            for pid, (handle, identity) in tuple(owned.items()):
                if pid != child['pid'] and kernel.WaitForSingleObject(handle, 0) == 0:
                    kernel.CloseHandle(handle)
                    del owned[pid]
            if process.poll() is not None:
                break
            time.sleep(0.01)
    except (OSError, RuntimeError) as error:
        errors.append(str(error))
        # Always complete the parental wait; observation failure is not a
        # license to abandon an active test or to report its result as PASS.
    finally:
        for handle, identity in owned.values():
            if handle != process._handle:
                kernel.CloseHandle(handle)
    paired = bool(loads) and all(any(
        Path(host['path']).parent == Path(load['path']).parent
        and host['process']['parent_pid'] == load['process']['pid']
        and host['process']['parent_birth_ns'] == load['process']['birth_ns']
        for host in hosts) for load in loads)
    return dict(method='kernel handles and live module enumeration',
                pair_loaded=paired, dll_loads=loads, host_processes=hosts,
                status='observed' if paired else 'not-observed', errors=errors)
