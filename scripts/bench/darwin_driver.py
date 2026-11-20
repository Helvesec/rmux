"""Darwin process wait and private readiness channel for the Unix collector.

This is a single-threaded harness driver, not a performance qualification or an
attestation of RMUX's helpers. It never waits for, or signals, an unrelated PID.
"""

from __future__ import annotations

import errno
import ctypes
import math
import os
import secrets
import select
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
from dataclasses import dataclass
from contextlib import closing
from pathlib import Path


@dataclass(frozen=True)
class Completed:
    args: list[str]
    pid: int
    returncode: int
    stdout: bytes
    stderr: bytes
    elapsed_ms: float
    observation: dict | None = None


def _deadline(timeout: float) -> float:
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout must be finite and positive")
    return time.monotonic() + timeout


def _event(queue: select.kqueue, deadline: float) -> select.kevent:
    """Block in the kernel; EINTR is not a sample retry or a polling interval."""
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("native event deadline expired")
        try:
            events = queue.control(None, 1, remaining)
        except InterruptedError:
            continue
        if not events:
            raise TimeoutError("native event deadline expired")
        event = events[0]
        if event.flags & select.KQ_EV_ERROR:
            raise OSError(event.data, os.strerror(event.data))
        return event


def _kill_group(pid: int, *, exited: bool = False) -> None:
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except PermissionError:
        if not exited:
            raise
        # Darwin returns EPERM for a group containing only its zombie leader.
        # Ignore it only after WNOWAIT proved that leader exited and a native
        # group query proves there is no other member. A two-PID buffer suffices
        # to disprove singleton status; unknown/truncated/error results fail.
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        members = library.proc_listpgrppids
        members.argtypes = (ctypes.c_int, ctypes.c_void_p, ctypes.c_int)
        members.restype = ctypes.c_int
        buffer = (ctypes.c_int * 2)()
        if members(pid, buffer, ctypes.sizeof(buffer)) != 1 or buffer[0] != pid:
            raise


def run(args: list[str], *, env: dict[str, str], timeout: float = 15.0,
        check: bool = True, observer=None) -> Completed:
    """Prepare outside timing, posix_spawn, kqueue NOTE_EXIT, waitpid(own PID).

    Output goes to private files so a child cannot deadlock on a full pipe, nor
    can a descendant holding stdout open postpone collection of the CLI child.
    On timeout/interruption, kill the newly created process group and reap the
    direct child. Detached runtime daemons remain the adapter's fixture's duty.
    """
    _deadline(timeout)
    if not args:
        raise ValueError("empty command")
    if signal.getsignal(signal.SIGCHLD) != signal.SIG_DFL:
        raise RuntimeError("driver requires exclusive collection of its children (default SIGCHLD)")
    argv = [str(arg) for arg in args]
    executable = (os.path.abspath(argv[0]) if os.path.dirname(argv[0]) else
                  shutil.which(argv[0], path=env.get("PATH", os.defpath)))
    if executable is None:
        raise FileNotFoundError(errno.ENOENT, os.strerror(errno.ENOENT), argv[0])
    with closing(select.kqueue()) as queue, open(os.devnull, "rb") as stdin, \
            tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        actions = [(os.POSIX_SPAWN_DUP2, stream.fileno(), target)
                   for target, stream in enumerate((stdin, stdout, stderr))]
        # /dev/fd is native and finite; do not scan to RLIMIT_NOFILE. The listing
        # can contain its own already-closed directory descriptor.
        for name in os.listdir("/dev/fd"):
            fd = int(name)
            if fd <= 2:
                continue
            try:
                os.fstat(fd)
            except OSError as error:
                if error.errno == errno.EBADF:
                    continue
                raise
            # CLOEXEC descriptors need no action. In particular Darwin does
            # not inherit kqueues at all: trying to close one in the spawned
            # child would fail the launch with EBADF.
            if os.get_inheritable(fd):
                actions.append((os.POSIX_SPAWN_CLOSE, fd))
        pid: int | None = None
        reaped = False
        exited = None
        reset_signals = tuple(getattr(signal, name) for name in ("SIGPIPE", "SIGXFZ", "SIGXFSZ")
                              if hasattr(signal, name))
        observation = {"prepared_ns": time.perf_counter_ns(), "events": []} if observer else None
        start = time.perf_counter_ns()
        deadline = _deadline(timeout)
        try:
            pid = os.posix_spawn(executable, argv, env, file_actions=actions,
                                 setpgroup=0, setsigmask=(), setsigdef=reset_signals)
            if observer:
                observation["events"].append(observer("spawn", pid))
            try:
                queue.control([select.kevent(
                    pid, filter=select.KQ_FILTER_PROC,
                    flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                    fflags=select.KQ_NOTE_EXIT,
                )], 0, 0)
            except ProcessLookupError:
                # An immediate child can exit before registration. It is still
                # our unreaped child: blocking waitpid cannot poll or steal it.
                pass
            else:
                event = _event(queue, deadline)
                if event.ident != pid or not event.fflags & select.KQ_NOTE_EXIT:
                    raise RuntimeError("unexpected child event")
            # Inspect without reaping: on failure, terminate descendants in our
            # group while the unreaped child still reserves this PID. Do not
            # signal a possibly recycled group after waitpid has released it.
            exited = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOWAIT)
            if observer:
                observation["events"].append(observer("exit_before_reap", pid))
            if exited.si_code != os.CLD_EXITED or exited.si_status != 0:
                _kill_group(pid, exited=True)
            reaped_pid, status = os.waitpid(pid, 0)
            reaped = True
            if reaped_pid != pid:
                raise RuntimeError("wrong child collected")
            end = time.perf_counter_ns()
            elapsed_ms = (end - start) / 1_000_000
            if observation is not None:
                observation.update(start_ns=start, end_ns=end,
                                   raw_wait_status=status, reaped_pid=reaped_pid,
                                   driver="darwin-posix-spawn-kqueue-waitpid",
                                   polling=False, observer_inside_primary_timer=True)
        except BaseException as error:
            if pid is not None and not reaped:
                try:
                    _kill_group(pid, exited=exited is not None)
                finally:
                    os.waitpid(pid, 0)
            if isinstance(error, TimeoutError):
                stdout.seek(0)
                stderr.seek(0)
                timeout_error = subprocess.TimeoutExpired(
                    argv, timeout, output=stdout.read(), stderr=stderr.read())
                timeout_error.pid = pid
                raise timeout_error from error
            raise
        stdout.seek(0)
        stderr.seek(0)
        result = Completed(argv, pid, os.waitstatus_to_exitcode(status),
                           stdout.read(), stderr.read(), elapsed_ms, observation)
    if check and result.returncode:
        error = subprocess.CalledProcessError(result.returncode, argv,
                                             result.stdout, result.stderr)
        error.pid = pid
        raise error
    return result


class Readiness:
    """One fresh, single-use challenge on a mode-0600 FIFO in a private dir.

    The producer writes from the process whose readiness is required. The
    consumer never infers readiness from cached screen output or elapsed time.
    A bad frame is terminal; it cannot be replaced by a later successful retry.
    """

    def __init__(self) -> None:
        self._directory = tempfile.TemporaryDirectory(prefix="rmux-ready-")
        self.path = Path(self._directory.name) / "ready"
        self.challenge = secrets.token_hex(32)
        self._expected = (self.challenge + "\n").encode("ascii")
        self._used = False
        self._fd: int | None = None
        self._queue: select.kqueue | None = None
        try:
            os.mkfifo(self.path, 0o600)
            # Our own write end prevents EOF spin before the producer opens.
            self._fd = os.open(self.path, os.O_RDWR | os.O_NONBLOCK)
            self._queue = select.kqueue()
            self._queue.control([select.kevent(
                self._fd, filter=select.KQ_FILTER_READ,
                flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
            )], 0, 0)
        except BaseException:
            self.close()
            raise

    @property
    def shell_signal(self) -> str:
        return f"printf '%s\\n' {shlex.quote(self.challenge)} > {shlex.quote(str(self.path))}"

    def wait(self, timeout: float = 10.0) -> None:
        if self._used:
            raise RuntimeError("readiness challenge already consumed")
        self._used = True
        deadline = _deadline(timeout)
        frame = bytearray()
        while True:
            event = _event(self._queue, deadline)
            if event.ident != self._fd or event.filter != select.KQ_FILTER_READ:
                raise RuntimeError("unexpected readiness event")
            try:
                frame.extend(os.read(self._fd, 4096))
            except BlockingIOError:
                continue
            if not self._expected.startswith(frame):
                raise ValueError("wrong or stale readiness challenge")
            if frame == self._expected:
                return

    def close(self) -> None:
        if self._queue is not None:
            self._queue.close()
            self._queue = None
        if self._fd is not None:
            os.close(self._fd)
            self._fd = None
        self._directory.cleanup()

    def __enter__(self) -> Readiness:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class FixtureProcess:
    """Exit observation for one daemon obtained from an owned fixture endpoint.

    No process discovery or helper attestation. The adapter registers the
    process outside timing, before it can return a successful sample.
    """

    def __init__(self, pid: int) -> None:
        if pid <= 1 or pid in (os.getpid(), os.getppid()):
            raise ValueError("invalid fixture daemon PID")
        self.pid = pid
        self.exited = False
        self._queue = select.kqueue()
        try:
            self._queue.control([select.kevent(
                pid, filter=select.KQ_FILTER_PROC,
                flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                fflags=select.KQ_NOTE_EXIT,
            )], 0, 0)
        except ProcessLookupError:
            self.exited = True
        except BaseException:
            self.close()
            raise

    def poll_exit(self) -> bool:
        if not self.exited:
            events = self._queue.control(None, 1, 0)
            if events:
                self._accept(events[0])
        return self.exited

    def _accept(self, event: select.kevent) -> None:
        if event.flags & select.KQ_EV_ERROR:
            raise OSError(event.data, os.strerror(event.data))
        if event.ident != self.pid or not event.fflags & select.KQ_NOTE_EXIT:
            raise RuntimeError("unexpected fixture process event")
        self.exited = True

    def wait(self, timeout: float) -> None:
        if not self.poll_exit():
            self._accept(_event(self._queue, _deadline(timeout)))

    def retire_after_failure(self, timeout: float) -> None:
        """Emergency fixture disposal; it cannot turn a failed sample into PASS."""
        if not self.poll_exit():
            try:
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.wait(timeout)

    def close(self) -> None:
        self._queue.close()
