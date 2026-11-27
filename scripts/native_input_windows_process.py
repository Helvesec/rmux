"""Windows process handles, kernel births and actual parental wait returns."""
import ctypes
from ctypes import wintypes
from functools import cache
import os
from pathlib import Path
import subprocess
import time
from types import FunctionType


def kernel():
    if os.name != 'nt':
        raise RuntimeError('native Windows process authority unavailable')
    api = ctypes.WinDLL('kernel32', use_last_error=True)
    api.GetCurrentProcess.restype = wintypes.HANDLE
    api.GetProcessId.argtypes = [wintypes.HANDLE]
    api.GetProcessId.restype = wintypes.DWORD
    api.GetProcessTimes.argtypes = [wintypes.HANDLE, *([ctypes.POINTER(wintypes.FILETIME)] * 4)]
    api.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                             wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    api.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    api.WaitForSingleObject.restype = wintypes.DWORD
    api.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    api.ResumeThread.argtypes = [wintypes.HANDLE]
    api.ResumeThread.restype = wintypes.DWORD
    api.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    return api


def filetime(value):
    return (value.dwHighDateTime << 32) | value.dwLowDateTime


def epoch_ns(ticks):
    return (ticks - 116444736000000000) * 100


def identity(handle, image=None):
    api = kernel()
    values = [wintypes.FILETIME() for _ in range(4)]
    if not api.GetProcessTimes(handle, *(ctypes.byref(v) for v in values)):
        raise ctypes.WinError(ctypes.get_last_error())
    pid = api.GetProcessId(handle)
    if not pid or not filetime(values[0]):
        raise RuntimeError('Windows handle has no kernel process birth')
    if image is None:
        if filetime(values[1]):
            # Preserve the observer's existing vanished-descendant OSError path.
            raise ProcessLookupError('Windows image identity requested after process exit')
        buffer = ctypes.create_unicode_buffer(32768)
        length = wintypes.DWORD(len(buffer))
        if not api.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(length)):
            raise ctypes.WinError(ctypes.get_last_error())
        image = str(Path(buffer.value).resolve(strict=True))
    return dict(pid=pid, birth_100ns=filetime(values[0]),
                birth_ns=epoch_ns(filetime(values[0])), end_ns=epoch_ns(filetime(values[1]))
                if filetime(values[1]) else None, image=image)


def current():
    return identity(kernel().GetCurrentProcess())


@cache
def creation_popen(original):
    """Capture on CreateProcess's suspended handle, before any child code runs.

    Keep subprocess's Windows argv, pipes and handle ownership. Only this call's
    _execute_child gets a private Win32 delegate; never patch the shared API.
    """
    if getattr(original, '_rmux_creation_identity', False):
        return original

    class CreationPopen(original):
        _rmux_creation_identity = True

        def __init__(self, *args, creation_image=None, capture_creation=True, **kwargs):
            self.creation_identity = None
            self._creation_image = creation_image
            self._capture_creation = capture_creation
            super().__init__(*args, **kwargs)

        def _execute_child(self, *args, **kwargs):
            execute = original._execute_child
            if not self._capture_creation:
                return execute(self, *args, **kwargs)
            if not isinstance(execute, FunctionType) or '_winapi' not in execute.__globals__:
                raise RuntimeError('Windows subprocess creation boundary unavailable')
            api = execute.__globals__['_winapi']
            owner = self

            class CreationAPI:
                def __getattr__(self, name):
                    return getattr(api, name)

                def CreateProcess(self, *arguments):
                    values = list(arguments)
                    flags = values[5]
                    values[5] = flags | 4  # CREATE_SUSPENDED, including no-manifest launches.
                    hp, ht, pid, tid = api.CreateProcess(*values)
                    try:
                        child = identity(hp)
                        if (not child or child['pid'] != pid or not child['birth_100ns']
                                or not child['image'] or child['end_ns'] is not None):
                            raise RuntimeError('Windows creation handle has no live identity')
                        if owner._creation_image is not None:
                            try:
                                expected = Path(owner._creation_image).resolve(strict=True)
                            except OSError as error:
                                raise RuntimeError('Windows qualified launch image unavailable') from error
                            if Path(child['image']) != expected:
                                raise RuntimeError('Windows handle image differs from qualified launch')
                        owner.creation_identity = child
                        if not flags & 4 and kernel().ResumeThread(ht) != 1:
                            raise RuntimeError('Windows primary thread resume failed')
                    except BaseException:
                        try:
                            native = kernel()
                            terminated = native.TerminateProcess(hp, 1)
                            # A failed resume may already have ended the child.
                            # Even then collect its wait before releasing handles.
                            if native.WaitForSingleObject(hp, 0xFFFFFFFF if terminated else 0) != 0:
                                raise RuntimeError('Windows creation cleanup wait failed')
                        finally:
                            api.CloseHandle(ht)
                            api.CloseHandle(hp)
                        raise
                    # subprocess now owns hp and closes ht in its original path.
                    return hp, ht, pid, tid

            local = dict(execute.__globals__, _winapi=CreationAPI())
            isolated = FunctionType(execute.__code__, local, execute.__name__,
                                    execute.__defaults__, execute.__closure__)
            isolated.__kwdefaults__ = execute.__kwdefaults__
            return isolated(self, *args, **kwargs)

    return CreationPopen


def popen(*args, **kwargs):
    return creation_popen(subprocess.Popen)(*args, **kwargs)


def wait_receipt(process, before, parent, child, started_ns):
    """Use the original child handle, even if a previous poll observed its exit."""
    api = kernel()
    # Windows may no longer answer QueryFullProcessImageName after termination.
    # Keep the image queried from this exact handle before the wait; GetProcessId
    # and GetProcessTimes still bind the handle's same PID/birth and native end.
    actual = identity(process._handle, image=child['image'])
    if (actual['pid'], actual['birth_100ns']) != (child['pid'], child['birth_100ns']):
        raise RuntimeError('Windows child handle identity changed before wait')
    result = api.WaitForSingleObject(process._handle, 0xFFFFFFFF)
    finished_ns = time.time_ns()
    exit_code = wintypes.DWORD()
    if result != 0 or not api.GetExitCodeProcess(process._handle, ctypes.byref(exit_code)):
        raise RuntimeError('Windows parental wait did not return WAIT_OBJECT_0')
    actual = identity(process._handle, image=child['image'])
    if (actual['pid'], actual['birth_100ns']) != (child['pid'], child['birth_100ns']):
        raise RuntimeError('Windows child handle identity changed during wait')
    actual_parent = current()
    if (actual['end_ns'] is None
            or (actual_parent['pid'], actual_parent['birth_100ns'])
            != (parent['pid'], parent['birth_100ns'])):
        raise RuntimeError('Windows wait lacks child end or matching parent birth')
    return dict(os='windows', case=before['case'], head=before['head'],
                parent=parent, child=actual, handle=int(process._handle),
                executable_sha256=before['executable_sha256'], before_ns=before['at_ns'],
                wait_started_ns=started_ns, wait_finished_ns=finished_ns,
                wait_api='WaitForSingleObject', wait_return=result,
                exit=exit_code.value)


def generated_inputs(args, env, cwd=None):
    """Preserve the existing compiler/generator paths, without discovering an OS."""
    paths = []
    for arg in args[1:]:
        arg = os.fspath(arg)
        value = arg.removeprefix('-Clink-arg=')
        if value.startswith('link-arg='):
            value = value[len('link-arg='):]
        if value.startswith('--extern='):
            value = value.split('=', 2)[-1]
        elif '=' in value and not value.startswith('-'):
            value = value.split('=', 1)[-1]
        path = Path(value)
        if path.suffix.lower() in ('.rs', '.rlib', '.rmeta', '.o', '.lib', '.dll') and path.is_file():
            paths.append(path.absolute())
    # Build output must exist and be qualified by the identity generator before
    # reaching compile_observed. Retain its actual current bytes at that boundary.
    if env.get('OUT_DIR'):
        root = Path(env['OUT_DIR']).absolute()
        if root.is_dir():
            paths.extend(p for p in root.rglob('*') if p.is_file())
    for key in ('RUSTC_WRAPPER', 'RUSTDOC'):
        if env.get(key) and Path(env[key]).is_file():
            paths.append(Path(env[key]).absolute())
    if os.name == 'nt' or env.get('RMUX_NATIVE_INPUT_MANIFEST'):
        from native_input_windows_consumer import launch_inputs
        paths.extend(launch_inputs(args, env, cwd))
    return tuple(dict.fromkeys(paths))


def install():
    """Opt in per Windows command; every inherited Python guard keeps its waits."""
    if not os.environ.get('RMUX_NATIVE_INPUT_MANIFEST'):
        return
    if os.name != 'nt':
        raise RuntimeError('Windows native session requires Windows')
    if getattr(subprocess.Popen, '_rmux_native_waits', False):
        return
    from native_input_windows import before_launch, write_once
    original = subprocess.Popen

    class ObservedPopen(creation_popen(original)):
        _rmux_native_waits = True

        def __init__(self, args, *positional, **kwargs):
            env = kwargs.get('env')
            env = os.environ if env is None else env
            if kwargs.get('shell') or not isinstance(args, (list, tuple)):
                raise RuntimeError('Windows native session requires literal argv')
            if env.get('RMUX_NATIVE_INPUT_MANIFEST'):
                import sys
                if Path(args[0]).resolve() == Path(sys.executable).resolve():
                    args = [args[0], '-I', '-S', '-B', *args[1:]]
            cwd = kwargs.get('cwd')
            self._native_before = before_launch(args, env, generated_inputs(args, env, cwd), cwd=cwd)
            self._native_wait_saved = False
            self._native_parent = current() if self._native_before else None
            if self._native_before:
                expected = Path(self._native_before['executable']).resolve(strict=True)
                if kwargs.get('creation_image') is not None and Path(kwargs['creation_image']) != expected:
                    raise RuntimeError('Windows launch image qualifications disagree')
                kwargs['creation_image'] = expected
                kwargs['capture_creation'] = True
            else:
                kwargs.setdefault('capture_creation', False)
            super().__init__(args, *positional, **kwargs)
            self._native_child = self.creation_identity if self._native_before else None

        def wait(self, timeout=None):
            started = time.time_ns()
            result = super().wait(timeout=timeout)
            if self._native_before and not self._native_wait_saved:
                receipt = wait_receipt(self, self._native_before, self._native_parent,
                                       self._native_child, started)
                if receipt['exit'] != (result & 0xFFFFFFFF):
                    raise RuntimeError('Windows wait return disagrees with subprocess exit')
                before = self._native_before
                # Each boundary is exclusive, so no PID-based filename can mix lives.
                obj = next(iter(before['files'].values()))['object']
                write_once(Path(obj).parent.parent / 'wait.json', receipt)
                self._native_wait_saved = True
            return result

    subprocess.Popen = ObservedPopen
