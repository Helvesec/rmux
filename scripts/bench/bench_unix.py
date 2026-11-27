#!/usr/bin/env python3
"""Collect local Unix RMUX/tmux benchmark measurements as JSON.

Contract for anything added here:

* Measure only on the machine that owns the tools; never across SSH.
* Emit a metric only for a documented strict equivalent or an explicitly
  approximate layout operation. Never estimate a missing adapter or a failed
  sample. Darwin failures retain attempts and leave an incomplete artifact.
* Record the commit, platform, tool versions and every p50/p95 sample so a
  reader can re-derive a published cell.
* Keep host identity out of the artifact: no hostname, address or absolute
  machine path is hard-coded, and binary paths are emitted repo-relative.
"""

from __future__ import annotations

import argparse
import atexit
import errno
import hashlib
import json
import os
import platform
import re
import secrets
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Callable
from contextlib import contextmanager
from pathlib import Path
from typing import Any


# Also support source loaders used by the repository's adapter specs.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import qualification
import tool_identity

if sys.platform == "darwin":
    import darwin_driver
    import screen_environment
    screen_environment.prepare()
else:
    darwin_driver = None


ROOT = Path.cwd()
SHELL = "/bin/sh"
TOOLS = ["rmux", "tmux", "zellij", "screen"]


def _remove_screen_directory(directory: Path, owner: int) -> None:
    if os.getpid() != owner:
        return
    try:
        directory.rmdir()
    except OSError as error:
        if error.errno not in (errno.ENOENT, errno.ENOTEMPTY, errno.EEXIST):
            raise


if sys.platform == "linux":
    # A 9-byte namespace fits the 79-byte session name, seven-digit PID,
    # '-queryX' reply suffix and NUL in Linux's 108-byte sun_path (105 total).
    # Allocate outside the timers and never reuse the caller's screen sessions.
    for _ in range(128):
        _screen_directory = Path("/tmp") / secrets.token_hex(2)
        try:
            _screen_directory.mkdir(mode=0o700)
        except FileExistsError:
            continue
        os.environ["SCREENDIR"] = str(_screen_directory)
        atexit.register(_remove_screen_directory, _screen_directory, os.getpid())
        break
    else:
        raise FileExistsError("cannot allocate a private short screen socket directory")


class FixtureCleanupError(RuntimeError):
    def __init__(self, details: dict[str, Any]) -> None:
        self.details = details
        super().__init__(f"fixture cleanup failed: {details}")


class Adapter:
    def __init__(self, tool: str, executable: str | Path, *, cleanup_timeout: float = 5.0) -> None:
        self.tool = tool
        self.executable = str(executable)
        self.cleanup_timeout = cleanup_timeout
        self._daemons: dict[str, Any] = {}

    def command(self, socket: str, args: list[str]) -> list[str]:
        return [self.executable, "-L", socket, *args]

    def observe_fixture(self, socket: str) -> None:
        if darwin_driver is not None and socket not in self._daemons:
            pid = int(captured(self.command(socket, ["display-message", "-p", "#{pid}"])))
            self._daemons[socket] = darwin_driver.FixtureProcess(pid)

    @staticmethod
    def _absent(result: Any, *, exited: bool = False) -> bool:
        if result.returncode != 1 or result.stdout:
            return False
        socket = result.args[result.args.index("-L") + 1]
        reasons = "No such file or directory|Connection refused" if exited else "No such file or directory"
        pattern = rf"error connecting to [^\r\n]+/{re.escape(socket)} \(({reasons})\)\n?"
        diagnostic = result.stderr.decode(errors="replace")
        if re.fullmatch(pattern, diagnostic) is not None:
            return True
        return exited and re.fullmatch(
            rf"no server running on [^\r\n]+/{re.escape(socket)}\n?", diagnostic
        ) is not None

    def cleanup(self, socket: str, *, absent_ok: bool = False) -> None:
        if darwin_driver is None:
            quiet(self.command(socket, ["kill-server"]), check=False, timeout=5.0)
            return
        process = self._daemons.pop(socket, None)
        details: dict[str, Any] = {"socket": socket, "pid": process.pid if process else None}
        try:
            if process is not None and process.poll_exit():
                return
            result = None
            try:
                result = darwin_driver.run(self.command(socket, ["kill-server"]),
                                           env=clean_env(), check=False, timeout=self.cleanup_timeout)
                if result.returncode:
                    error = subprocess.CalledProcessError(result.returncode, result.args,
                                                         result.stdout, result.stderr)
                    details["command_error"] = failure_details(error)
            except Exception as error:
                details["command_error"] = failure_details(error)
            if process is None:
                if absent_ok and result is not None and self._absent(result):
                    return
                details["retirement_error"] = "fixture process identity was not established"
            else:
                try:
                    process.wait(self.cleanup_timeout)
                except Exception as error:
                    details["retirement_error"] = failure_details(error)
                details["exit_observed"] = process.exited
                if process.exited and result is not None and self._absent(result, exited=True):
                    details.pop("command_error", None)
            if "command_error" in details or "retirement_error" in details:
                if process is not None and not process.exited:
                    details["forced_cleanup_attempted"] = True
                    try:
                        process.retire_after_failure(self.cleanup_timeout)
                        details["forced_cleanup_exit_observed"] = process.exited
                    except Exception as error:
                        details["forced_cleanup_error"] = failure_details(error)
                raise FixtureCleanupError(details)
        finally:
            if process is not None:
                process.close()

    def cleanup_after_error(self, socket: str, primary: BaseException) -> None:
        try:
            self.cleanup(socket)
        except BaseException as cleanup_error:
            primary.cleanup_error = cleanup_error
            primary.add_note(f"Additional fixture cleanup failure: {cleanup_error}")

    @contextmanager
    def fixture(self, socket: str):
        try:
            yield
        except BaseException as primary:
            self.cleanup_after_error(socket, primary)
            raise
        else:
            self.cleanup(socket)


AdapterOperation = Callable[[Adapter], float]
MeasuredOperation = Callable[[], float]


def clean_env() -> dict[str, str]:
    env = dict(os.environ)
    for name in ("RMUX", "TMUX", "TERM_PROGRAM", "ZELLIJ",
                 "RMUX_INTERNAL_BINARY_PATH", "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE"):
        env.pop(name, None)
    return env


def quiet(cmd: list[str], *, check: bool = True, timeout: float = 15.0) -> None:
    if darwin_driver is not None:
        darwin_driver.run(cmd, env=clean_env(), timeout=timeout, check=check)
        return
    subprocess.run(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=clean_env(),
        timeout=timeout,
        check=check,
    )


def captured(cmd: list[str], *, timeout: float = 15.0) -> str:
    if darwin_driver is not None:
        return darwin_driver.run(cmd, env=clean_env(), timeout=timeout).stdout.decode().strip()
    return subprocess.check_output(
        cmd,
        text=True,
        stderr=subprocess.DEVNULL,
        env=clean_env(),
        timeout=timeout,
    ).strip()


def timed(cmd: list[str], *, timeout: float = 15.0) -> float:
    if darwin_driver is not None:
        return darwin_driver.run(cmd, env=clean_env(), timeout=timeout).elapsed_ms
    start = time.perf_counter()
    quiet(cmd, timeout=timeout)
    return (time.perf_counter() - start) * 1000.0


def timed_unchecked(cmd: list[str], *, timeout: float = 15.0) -> float:
    """Time a command whose success is not encoded in its exit status.

    GNU `screen -ls` reports the session list on stdout and still exits
    nonzero, so the listing operation cannot be timed with `timed`.
    """
    if darwin_driver is not None:
        return darwin_driver.run(cmd, env=clean_env(), timeout=timeout, check=False).elapsed_ms
    start = time.perf_counter()
    quiet(cmd, check=False, timeout=timeout)
    return (time.perf_counter() - start) * 1000.0


def git(*args: str) -> str:
    try:
        return subprocess.check_output(
            ["git", *args], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except Exception:
        return "unknown"


def version(cmd: str, *args: str, accept_nonzero_exit: bool = False) -> str | None:
    """Return a tool's first version line, or None when the tool is absent.

    `accept_nonzero_exit` keeps the printed banner of tools that report their
    version through a failing exit status; GNU `screen -v` is one of them, and
    rejecting it would degrade a real version string to the bare literal
    "available".
    """
    path = shutil.which(cmd)
    if not path:
        return None
    try:
        completed = subprocess.run(
            [path, *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            env=clean_env(),
            timeout=5.0,
            check=False,
        )
    except Exception:
        return "available"
    if completed.returncode != 0 and not accept_nonzero_exit:
        return "available"
    lines = completed.stdout.strip().splitlines()
    if not lines:
        return "available"
    return lines[0]


def stats(samples: list[float]) -> dict[str, object]:
    ordered = sorted(samples)
    p95_index = min(len(ordered) - 1, max(0, int(len(ordered) * 0.95 + 0.999) - 1))
    return {
        "p50_ms": round(statistics.median(ordered), 3),
        "p95_ms": round(ordered[p95_index], 3),
        "samples_ms": [round(sample, 3) for sample in samples],
    }


def progress(enabled: bool, message: str) -> None:
    if enabled:
        print(f"[bench] {message}", file=sys.stderr, flush=True)


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    tmp = path.with_name(f"{path.name}.tmp")
    tmp.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    tmp.replace(path)


def comma_set(value: str | None) -> set[str] | None:
    if value is None or value.strip() == "":
        return None
    return {part.strip() for part in value.split(",") if part.strip()}


def socket_name(operation: str, tool: str) -> str:
    return f"rmux-bench-{tool}-{operation}-{os.getpid()}-{time.time_ns()}"


def session(
    adapter: Adapter,
    socket: str,
    *,
    name: str = "bench",
    command: str | None = None,
    width: int | None = None,
    height: int | None = None,
    ready_lines: int | None = None,
) -> None:
    args = ["new-session", "-d", "-s", name]
    if width is not None:
        args.extend(["-x", str(width)])
    if height is not None:
        args.extend(["-y", str(height)])
    if darwin_driver is not None and ready_lines is not None:
        if command != output_command(ready_lines):
            raise ValueError("readiness must match the declared output workload")
        with darwin_driver.Readiness() as ready:
            # Signal from the producer after its last write, before its existing
            # sleep. This is setup, not part of the timed capture command.
            script = output_script(ready_lines, ready.shell_signal)
            quiet(adapter.command(socket, [*args, f"{SHELL} -c {shlex.quote(script)}"]))
            adapter.observe_fixture(socket)
            ready.wait()
        return
    if command is not None:
        args.append(command)
    quiet(adapter.command(socket, args))
    adapter.observe_fixture(socket)
    if darwin_driver is not None and command is None:
        shell_ready(adapter, socket, name)


def shell_ready(adapter: Adapter, socket: str, target: str) -> None:
    """A fresh acknowledgement from the fixture's shell, outside timing.

    Sending the command can return before the shell starts reading. Only the
    private FIFO acknowledgement proves that this shell consumed the challenge.
    The setup command is visible in the pane's history on both products.
    """
    with darwin_driver.Readiness() as ready:
        quiet(adapter.command(socket, ["send-keys", "-t", target, ready.shell_signal, "Enter"]))
        ready.wait()


def output_command(lines: int) -> str:
    """The measured payload as a shell command line.

    tmux and zellij hand their trailing session argument to a shell, so they
    receive the workload already quoted for one.
    """
    return f"{SHELL} -c '{output_script(lines)}'"


def output_argv(lines: int) -> list[str]:
    """The same measured payload as an argv vector.

    GNU screen executes its program argument directly instead of through a
    shell, so it needs the elements `execvp` receives rather than a command
    line. Both forms wrap the same `output_script`, so the two adapters measure
    an identical workload.
    """
    return [SHELL, "-c", output_script(lines)]


def output_script(lines: int, ready_signal: str | None = None) -> str:
    return (
        "i=0; "
        f"while [ $i -lt {lines} ]; do printf \"rmux-bench-%05d\\n\" \"$i\"; "
        "i=$((i+1)); done; "
        + (f"{ready_signal}; " if ready_signal is not None else "")
        + "sleep 60"
    )


def output_sentinel(lines: int) -> str:
    """The last line `output_script` prints, so a reader can tell it finished."""
    return f"rmux-bench-{lines - 1:05d}"


def with_session(
    adapter: Adapter,
    operation: str,
    timed_args: list[str],
    *,
    command: str | None = None,
    setup: Callable[[str], None] | None = None,
    width: int | None = None,
    height: int | None = None,
    timeout: float = 15.0,
    ready_lines: int | None = None,
) -> float:
    socket = socket_name(operation, adapter.tool)
    with adapter.fixture(socket):
        session(adapter, socket, command=command, width=width, height=height,
                ready_lines=ready_lines)
        if setup is not None:
            setup(socket)
        return timed(adapter.command(socket, timed_args), timeout=timeout)


def wait_for_target(adapter: Adapter, socket: str, target: str) -> None:
    if darwin_driver is not None:
        quiet(adapter.command(socket, ["list-panes", "-t", target]), timeout=5.0)
        shell_ready(adapter, socket, target)
        return
    deadline = time.monotonic() + 5.0
    while True:
        try:
            quiet(adapter.command(socket, ["list-panes", "-t", target]), timeout=5.0)
            return
        except Exception:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.01)


def list_commands(adapter: Adapter) -> float:
    return timed([adapter.executable, "list-commands"])


def new_session_cold_sh(adapter: Adapter) -> float:
    socket = socket_name("new_session_cold_sh", adapter.tool)
    adapter.cleanup(socket, absent_ok=True)
    with adapter.fixture(socket):
        elapsed = timed(adapter.command(socket, ["new-session", "-d", "-s", "bench"]))
        adapter.observe_fixture(socket)
        if darwin_driver is not None:
            shell_ready(adapter, socket, "bench")
        return elapsed


def new_session_warm_sh(adapter: Adapter) -> float:
    socket = socket_name("new_session_warm_sh", adapter.tool)
    with adapter.fixture(socket):
        quiet(
            adapter.command(
                socket,
                ["start-server", ";", "set-option", "-g", "exit-empty", "off"],
            )
        )
        adapter.observe_fixture(socket)
        elapsed = timed(adapter.command(socket, ["new-session", "-d", "-s", "bench"]))
        adapter.observe_fixture(socket)
        if darwin_driver is not None:
            shell_ready(adapter, socket, "bench")
        return elapsed


def split_window_h_detached_sh(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "split_window_h_detached_sh",
        ["split-window", "-h", "-d", "-t", "bench"],
    )


def split_window_v_detached_sh(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "split_window_v_detached_sh",
        ["split-window", "-v", "-d", "-t", "bench"],
    )


def split_window_h_attached_sh(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "split_window_h_attached_sh",
        ["split-window", "-h", "-t", "bench"],
    )


def split_window_v_attached_sh(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "split_window_v_attached_sh",
        ["split-window", "-v", "-t", "bench"],
    )


def resize_pane_right_1(adapter: Adapter) -> float:
    return with_session(adapter, "resize_pane_right_1", ["resize-pane", "-t", "bench", "-R", "1"])


def resize_pane_right_10(adapter: Adapter) -> float:
    return with_session(adapter, "resize_pane_right_10", ["resize-pane", "-t", "bench", "-R", "10"])


def resize_pane_left_1(adapter: Adapter) -> float:
    return with_session(adapter, "resize_pane_left_1", ["resize-pane", "-t", "bench", "-L", "1"])


def resize_pane_absolute_100x30(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "resize_pane_absolute_100x30",
        ["resize-pane", "-x", "100", "-y", "30", "-t", "bench"],
    )


def resize_pane_absolute_200x50(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "resize_pane_absolute_200x50",
        ["resize-pane", "-x", "200", "-y", "50", "-t", "bench"],
    )


def resize_pane_storm_200(adapter: Adapter) -> float:
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as handle:
        for _ in range(100):
            handle.write("resize-pane -t bench:0.0 -R 1\n")
            handle.write("resize-pane -t bench:0.0 -L 1\n")
        path = handle.name

    def setup(socket: str) -> None:
        quiet(adapter.command(socket, ["split-window", "-h", "-d", "-t", "bench"]))

    try:
        return with_session(
            adapter,
            "resize_pane_storm_200",
            ["source-file", path],
            setup=setup,
            width=200,
            height=50,
            timeout=30.0,
        )
    finally:
        Path(path).unlink(missing_ok=True)


def list_sessions_default(adapter: Adapter) -> float:
    return with_session(adapter, "list_sessions_default", ["list-sessions"])


def capture_pane_5000_lines(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "capture_pane_5000_lines",
        ["capture-pane", "-p", "-t", "bench"],
        command=output_command(5000),
        timeout=30.0,
        ready_lines=5000,
    )


def capture_pane_80x24(adapter: Adapter) -> float:
    return with_session(adapter, "capture_pane_80x24", ["capture-pane", "-p", "-t", "bench"])


def capture_pane_200x50_scrollback_10k(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "capture_pane_200x50_scrollback_10k",
        ["capture-pane", "-p", "-S", "-10000", "-t", "bench"],
        command=output_command(10000),
        width=200,
        height=50,
        timeout=30.0,
        ready_lines=10000,
    )


def new_window_detached_sh(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "new_window_detached_sh",
        ["new-window", "-d", "-t", "bench"],
    )


def new_window_then_kill(adapter: Adapter) -> float:
    return with_session(adapter, "new_window_then_kill", ["new-window", "-d", "-t", "bench"])


def send_keys_detached_round_trip(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "send_keys_detached_round_trip",
        ["send-keys", "-t", "bench:0.0", "printf rmux-bench", "Enter"],
    )


def display_message_default(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "display_message_default",
        ["display-message", "-p", "-t", "bench", "#{session_name}"],
    )


def show_options_global(adapter: Adapter) -> float:
    return with_session(adapter, "show_options_global", ["show-options", "-g"])


def show_window_options(adapter: Adapter) -> float:
    return with_session(adapter, "show_window_options", ["show-window-options", "-g"])


def setup_list_windows_20(adapter: Adapter, socket: str) -> None:
    for index in range(1, 20):
        quiet(adapter.command(socket, ["new-window", "-d", "-n", f"w{index}", "-t", "bench"]))
        wait_for_target(adapter, socket, f"bench:{index}")


def list_windows_20(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "list_windows_20",
        ["list-windows", "-t", "bench"],
        setup=lambda socket: setup_list_windows_20(adapter, socket),
    )


def batched_list_windows_20(adapter: Adapter, iterations: int) -> MeasuredOperation:
    socket: str | None = None
    remaining = iterations

    def operation() -> float:
        nonlocal socket, remaining
        if socket is None:
            socket = socket_name("list_windows_20", adapter.tool)
            try:
                session(adapter, socket)
                setup_list_windows_20(adapter, socket)
            except BaseException as primary:
                try:
                    adapter.cleanup_after_error(socket, primary)
                finally:
                    socket = None
                raise
        primary = None
        try:
            return timed(adapter.command(socket, ["list-windows", "-t", "bench"]))
        except BaseException as error:
            primary = error
            remaining = 1
            raise
        finally:
            remaining -= 1
            if remaining <= 0 and socket is not None:
                try:
                    if primary is None:
                        adapter.cleanup(socket)
                    else:
                        adapter.cleanup_after_error(socket, primary)
                finally:
                    socket = None

    return qualification.cancellable(operation, lambda: adapter.cleanup(socket) if socket is not None else None)


def setup_list_panes_80(adapter: Adapter, socket: str) -> None:
    for index in range(1, 20):
        quiet(adapter.command(socket, ["new-window", "-d", "-n", f"w{index}", "-t", "bench"]))
        wait_for_target(adapter, socket, f"bench:{index}")
    for window in range(20):
        target = f"bench:{window}"
        wait_for_target(adapter, socket, target)
        for _ in range(3):
            quiet(adapter.command(socket, ["split-window", "-d", "-t", target]))


def list_panes_80(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "list_panes_80",
        ["list-panes", "-a"],
        setup=lambda socket: setup_list_panes_80(adapter, socket),
        timeout=30.0,
    )


def batched_list_panes_80(adapter: Adapter, iterations: int) -> MeasuredOperation:
    socket: str | None = None
    remaining = iterations

    def operation() -> float:
        nonlocal socket, remaining
        if socket is None:
            socket = socket_name("list_panes_80", adapter.tool)
            try:
                session(adapter, socket)
                setup_list_panes_80(adapter, socket)
            except BaseException as primary:
                try:
                    adapter.cleanup_after_error(socket, primary)
                finally:
                    socket = None
                raise
        primary = None
        try:
            return timed(adapter.command(socket, ["list-panes", "-a"]), timeout=30.0)
        except BaseException as error:
            primary = error
            remaining = 1
            raise
        finally:
            remaining -= 1
            if remaining <= 0 and socket is not None:
                try:
                    if primary is None:
                        adapter.cleanup(socket)
                    else:
                        adapter.cleanup_after_error(socket, primary)
                finally:
                    socket = None

    return qualification.cancellable(operation, lambda: adapter.cleanup(socket) if socket is not None else None)



def rename_window(adapter: Adapter) -> float:
    return with_session(adapter, "rename_window", ["rename-window", "-t", "bench:0", "renamed"])


def select_window_next(adapter: Adapter) -> float:
    def setup(socket: str) -> None:
        quiet(adapter.command(socket, ["new-window", "-d", "-n", "next", "-t", "bench"]))

    return with_session(adapter, "select_window_next", ["select-window", "-t", "bench:1"], setup=setup)


def break_pane_detached(adapter: Adapter) -> float:
    def setup(socket: str) -> None:
        quiet(adapter.command(socket, ["split-window", "-d", "-t", "bench"]))

    return with_session(adapter, "break_pane_detached", ["break-pane", "-d", "-t", "bench:0.1"], setup=setup)


def join_pane_detached(adapter: Adapter) -> float:
    def setup(socket: str) -> None:
        quiet(adapter.command(socket, ["new-window", "-d", "-n", "join", "-t", "bench"]))

    return with_session(
        adapter,
        "join_pane_detached",
        ["join-pane", "-d", "-s", "bench:1.0", "-t", "bench:0.0"],
        setup=setup,
    )


def source_file_minimal(adapter: Adapter) -> float:
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as handle:
        handle.write("set-option -g status on\n")
        path = handle.name
    try:
        return with_session(adapter, "source_file_minimal", ["source-file", path])
    finally:
        Path(path).unlink(missing_ok=True)


def set_option_quiet(adapter: Adapter) -> float:
    return with_session(adapter, "set_option_quiet", ["set-option", "-g", "status", "on"])


def set_window_option_quiet(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "set_window_option_quiet",
        ["set-window-option", "-g", "automatic-rename", "off"],
    )


def kill_pane(adapter: Adapter) -> float:
    def setup(socket: str) -> None:
        quiet(adapter.command(socket, ["split-window", "-d", "-t", "bench"]))

    return with_session(adapter, "kill_pane", ["kill-pane", "-t", "bench:0.1"], setup=setup)


def kill_session(adapter: Adapter) -> float:
    return with_session(adapter, "kill_session", ["kill-session", "-t", "bench"])


def kill_server(adapter: Adapter) -> float:
    return with_session(adapter, "kill_server", ["kill-server"])


def zellij_session_name(operation: str) -> str:
    seed = f"{operation}-{os.getpid()}-{time.time_ns()}".encode()
    return f"rmux-zj-{hashlib.sha1(seed).hexdigest()[:12]}"


def zellij_cleanup(executable: str, name: str) -> None:
    quiet([executable, "kill-session", name], check=False, timeout=10.0)
    quiet([executable, "delete-session", "--force", name], check=False, timeout=10.0)


def zellij_create_session(executable: str, name: str) -> None:
    quiet([executable, "attach", "--create-background", name], timeout=30.0)


def with_zellij_session(
    executable: str,
    operation: str,
    timed_args: list[str],
    *,
    setup: Callable[[str], None] | None = None,
    timeout: float = 15.0,
) -> float:
    name = zellij_session_name(operation)
    try:
        zellij_create_session(executable, name)
        if setup is not None:
            setup(name)
        return timed([executable, "--session", name, *timed_args], timeout=timeout)
    finally:
        zellij_cleanup(executable, name)


def zellij_new_session_cold(executable: str) -> float:
    name = zellij_session_name("new_session_cold_sh")
    zellij_cleanup(executable, name)
    try:
        return timed([executable, "attach", "--create-background", name], timeout=30.0)
    finally:
        zellij_cleanup(executable, name)


def zellij_list_sessions(executable: str) -> float:
    name = zellij_session_name("list_sessions_default")
    try:
        zellij_create_session(executable, name)
        return timed([executable, "list-sessions"], timeout=15.0)
    finally:
        zellij_cleanup(executable, name)


def zellij_new_window(executable: str) -> float:
    return with_zellij_session(
        executable,
        "new_window_detached_sh",
        ["action", "new-tab", "--name", "bench-tab"],
        timeout=20.0,
    )


def zellij_send_keys(executable: str) -> float:
    return with_zellij_session(
        executable,
        "send_keys_detached_round_trip",
        ["action", "write-chars", "printf rmux-bench\n"],
    )


def zellij_capture_visible(executable: str) -> float:
    return with_zellij_session(executable, "capture_pane_80x24", ["action", "dump-screen"])


def zellij_capture_scrollback(executable: str) -> float:
    def setup(name: str) -> None:
        captured(
            [executable, "--session", name, "action", "new-pane", "--", SHELL, "-c", output_script(10000)],
            timeout=30.0,
        )
        time.sleep(0.05)

    return with_zellij_session(
        executable,
        "capture_pane_200x50_scrollback_10k",
        ["action", "dump-screen", "--full"],
        setup=setup,
        timeout=30.0,
    )


def setup_zellij_list_windows(executable: str, name: str) -> None:
    for index in range(1, 20):
        quiet([executable, "--session", name, "action", "new-tab", "--name", f"w{index}"], timeout=10.0)


def zellij_list_windows(executable: str) -> float:
    return with_zellij_session(
        executable,
        "list_windows_20",
        ["action", "query-tab-names"],
        setup=lambda name: setup_zellij_list_windows(executable, name),
        timeout=30.0,
    )


def batched_zellij_list_windows(executable: str, iterations: int) -> MeasuredOperation:
    name: str | None = None
    remaining = iterations

    def operation() -> float:
        nonlocal name, remaining
        if name is None:
            name = zellij_session_name("list_windows_20")
            try:
                zellij_create_session(executable, name)
                setup_zellij_list_windows(executable, name)
            except Exception:
                zellij_cleanup(executable, name)
                name = None
                raise
        try:
            return timed([executable, "--session", name, "action", "query-tab-names"], timeout=30.0)
        finally:
            remaining -= 1
            if remaining <= 0 and name is not None:
                zellij_cleanup(executable, name)
                name = None

    return qualification.cancellable(operation, lambda: zellij_cleanup(executable, name) if name is not None else None)


def zellij_kill_session(executable: str) -> float:
    name = zellij_session_name("kill_session")
    try:
        zellij_create_session(executable, name)
        return timed([executable, "kill-session", name], timeout=15.0)
    finally:
        zellij_cleanup(executable, name)


def zellij_split_right(executable: str) -> float:
    return with_zellij_session(
        executable,
        "split_window_h_detached_sh",
        ["action", "new-pane", "--direction", "right", "--", SHELL],
    )


def zellij_split_down(executable: str) -> float:
    return with_zellij_session(
        executable,
        "split_window_v_detached_sh",
        ["action", "new-pane", "--direction", "down", "--", SHELL],
    )


def zellij_resize_right(executable: str) -> float:
    def setup(name: str) -> None:
        quiet([executable, "--session", name, "action", "new-pane", "--direction", "right", "--", SHELL])

    return with_zellij_session(
        executable,
        "resize_pane_right_1",
        ["action", "resize", "increase", "right"],
        setup=setup,
    )


def setup_zellij_list_panes(executable: str, name: str) -> None:
    for _ in range(79):
        quiet([executable, "--session", name, "action", "new-pane", "--", SHELL], timeout=10.0)


def zellij_list_panes(executable: str) -> float:
    return with_zellij_session(
        executable,
        "list_panes_80",
        ["action", "list-panes"],
        setup=lambda name: setup_zellij_list_panes(executable, name),
        timeout=60.0,
    )


def batched_zellij_list_panes(executable: str, iterations: int) -> MeasuredOperation:
    name: str | None = None
    remaining = iterations

    def operation() -> float:
        nonlocal name, remaining
        if name is None:
            name = zellij_session_name("list_panes_80")
            try:
                zellij_create_session(executable, name)
                setup_zellij_list_panes(executable, name)
            except Exception:
                zellij_cleanup(executable, name)
                name = None
                raise
        try:
            return timed([executable, "--session", name, "action", "list-panes"], timeout=60.0)
        finally:
            remaining -= 1
            if remaining <= 0 and name is not None:
                zellij_cleanup(executable, name)
                name = None

    return qualification.cancellable(operation, lambda: zellij_cleanup(executable, name) if name is not None else None)


# GNU screen keeps at most this many characters of a session name and silently
# truncates longer ones, after which every `-S <name>` lookup misses.
SCREEN_NAME_LIMIT = 79


def screen_session_name(operation: str) -> str:
    """A unique session name that survives screen's own length limit.

    `capture_pane_200x50_scrollback_10k` pushes the shared socket name one
    character past what screen keeps, and a truncated name makes the capture
    and the cleanup that follow it address a session that does not exist.
    Uniqueness lives in the trailing pid and timestamp, so the operation is the
    part that gets shortened.
    """
    name = socket_name(operation, "screen")
    if len(name) <= SCREEN_NAME_LIMIT:
        return name
    head, pid, nanos = name.rsplit("-", 2)
    unique = f"-{pid}-{nanos}"
    return head[: max(0, SCREEN_NAME_LIMIT - len(unique))] + unique


def screen_create_command(
    executable: str,
    name: str,
    *,
    program: list[str] | None = None,
    scrollback: int | None = None,
) -> list[str]:
    """Build a detached `screen` session command.

    Options precede the program, and omitting the program starts the login
    shell, which is the payload the tmux-like adapters also measure.

    `program` is an argv vector, never a shell command line. screen execs it
    directly, so a whole command passed as one element is looked up as a single
    executable name, fails, and takes the new session down with it.
    """
    args = [executable, "-dmS", name]
    if scrollback is not None:
        args.extend(["-h", str(scrollback)])
    if program is not None:
        args.extend(program)
    return args


def screen_control_command(executable: str, name: str, args: list[str]) -> list[str]:
    return [executable, "-S", name, *args]


def _screen_process_start(pid: int) -> str | None:
    if sys.platform != "linux":
        return None
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    except (FileNotFoundError, ProcessLookupError):
        # Exit after open but before read reports ESRCH, not ENOENT.
        return None
    if fields[0] in ("Z", "X"):
        # A child's leader can be Z before the whole process is waitable.
        # Leave its exit status for the subprocess owner to collect.
        try:
            status = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        except ChildProcessError:
            # Detached servers have no child status for this process to read.
            return None
        if status is not None:
            return None
    # Starttime distinguishes a subsequently reused PID.
    return fields[19]


def screen_cleanup(executable: str, name: str) -> None:
    deadline = time.monotonic() + 5.0
    directory = clean_env().get("SCREENDIR")
    sockets = []
    if directory is not None:
        try:
            sockets = [path for path in Path(directory).iterdir()
                       if path.name.partition(".")[0].isdigit()
                       and path.name.partition(".")[2] == name]
        except FileNotFoundError:
            pass
    processes = {int(path.name.partition(".")[0]): _screen_process_start(
        int(path.name.partition(".")[0])) for path in sockets}
    quiet(screen_control_command(executable, name, ["-X", "quit"]),
          check=False, timeout=max(0.0, deadline - time.monotonic()))
    # The quit client only sends a request. Observe the server's own unlink
    # and exit, outside the measured sample and within the original budget.
    while True:
        sockets = [path for path in sockets if path.exists()]
        processes = {pid: start for pid, start in processes.items()
                     if start is not None and _screen_process_start(pid) == start}
        if not sockets and not processes:
            return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise FixtureCleanupError({"tool": "screen", "session": name,
                                       "remaining_sockets": [path.name for path in sockets],
                                       "live_processes": list(processes)})
        time.sleep(min(0.01, remaining))


def screen_create_session(
    executable: str,
    name: str,
    *,
    program: list[str] | None = None,
    scrollback: int | None = None,
) -> None:
    quiet(
        screen_create_command(executable, name, program=program, scrollback=scrollback),
        timeout=10.0,
    )


def with_screen_session(
    executable: str,
    operation: str,
    timed_args: list[str],
    *,
    program: list[str] | None = None,
    setup: Callable[[str], None] | None = None,
    scrollback: int | None = None,
    timeout: float = 15.0,
) -> float:
    name = screen_session_name(operation)
    try:
        screen_create_session(executable, name, program=program, scrollback=scrollback)
        if setup is not None:
            setup(name)
        return timed(screen_control_command(executable, name, timed_args), timeout=timeout)
    finally:
        screen_cleanup(executable, name)


def screen_new_session_cold(executable: str) -> float:
    name = screen_session_name("new_session_cold_sh")
    screen_cleanup(executable, name)
    try:
        return timed(screen_create_command(executable, name), timeout=10.0)
    finally:
        screen_cleanup(executable, name)


def screen_list_sessions(executable: str) -> float:
    name = screen_session_name("list_sessions_default")
    try:
        screen_create_session(executable, name)
        return timed_unchecked([executable, "-ls"], timeout=10.0)
    finally:
        screen_cleanup(executable, name)


def screen_new_window(executable: str) -> float:
    return with_screen_session(executable, "new_window_detached_sh", ["-X", "screen"])


def screen_send_keys(executable: str) -> float:
    return with_screen_session(
        executable,
        "send_keys_detached_round_trip",
        ["-X", "stuff", "printf rmux-bench\n"],
    )


def screen_capture_visible(executable: str) -> float:
    with tempfile.NamedTemporaryFile(delete=False) as handle:
        path = handle.name
    try:
        return with_screen_session(
            executable,
            "capture_pane_80x24",
            ["-X", "hardcopy", path],
            timeout=10.0,
        )
    finally:
        Path(path).unlink(missing_ok=True)


def wait_for_screen_payload(
    executable: str,
    name: str,
    sentinel: str,
    *,
    timeout: float = 20.0,
) -> None:
    """Block until the session's measured payload has reached its buffer.

    `screen -dmS` returns as soon as it has forked, so a control command issued
    immediately can reach a session that is not serving yet, or one whose
    payload has not printed. Timing the capture then measures an empty buffer
    instead of the workload the row is named for.
    """
    with tempfile.NamedTemporaryFile(delete=False) as handle:
        probe = handle.name
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            quiet(
                screen_control_command(executable, name, ["-X", "hardcopy", probe]),
                check=False,
                timeout=5.0,
            )
            if sentinel in Path(probe).read_text(encoding="utf-8", errors="replace"):
                return
            time.sleep(0.05)
        raise TimeoutError(f"screen session did not print {sentinel} within {timeout:.0f}s")
    finally:
        Path(probe).unlink(missing_ok=True)


def screen_capture_scrollback(executable: str) -> float:
    with tempfile.NamedTemporaryFile(delete=False) as handle:
        path = handle.name
    try:
        return with_screen_session(
            executable,
            "capture_pane_200x50_scrollback_10k",
            ["-X", "hardcopy", "-h", path],
            program=output_argv(10000),
            scrollback=10000,
            setup=lambda name: wait_for_screen_payload(
                executable, name, output_sentinel(10000)
            ),
            timeout=30.0,
        )
    finally:
        Path(path).unlink(missing_ok=True)


def setup_screen_list_windows(executable: str, name: str) -> None:
    for index in range(1, 20):
        quiet(
            screen_control_command(executable, name, ["-X", "screen", "-t", f"w{index}"]),
            timeout=10.0,
        )


def screen_list_windows(executable: str) -> float:
    return with_screen_session(
        executable,
        "list_windows_20",
        ["-Q", "windows"],
        setup=lambda name: setup_screen_list_windows(executable, name),
        timeout=10.0,
    )


def batched_screen_list_windows(executable: str, iterations: int) -> MeasuredOperation:
    name: str | None = None
    remaining = iterations

    def operation() -> float:
        nonlocal name, remaining
        if name is None:
            name = screen_session_name("list_windows_20")
            try:
                screen_create_session(executable, name)
                setup_screen_list_windows(executable, name)
            except Exception:
                screen_cleanup(executable, name)
                name = None
                raise
        try:
            return timed(screen_control_command(executable, name, ["-Q", "windows"]), timeout=10.0)
        finally:
            remaining -= 1
            if remaining <= 0 and name is not None:
                screen_cleanup(executable, name)
                name = None

    return qualification.cancellable(operation, lambda: screen_cleanup(executable, name) if name is not None else None)


def screen_kill_session(executable: str) -> float:
    name = screen_session_name("kill_session")
    try:
        screen_create_session(executable, name)
        return timed(screen_control_command(executable, name, ["-X", "quit"]), timeout=10.0)
    finally:
        screen_cleanup(executable, name)


OPERATIONS: list[tuple[str, AdapterOperation]] = [
    ("new_session_warm_sh", new_session_warm_sh),
    ("list_sessions_default", list_sessions_default),
    ("split_window_h_detached_sh", split_window_h_detached_sh),
    ("split_window_v_detached_sh", split_window_v_detached_sh),
    ("send_keys_detached_round_trip", send_keys_detached_round_trip),
    ("capture_pane_80x24", capture_pane_80x24),
    ("capture_pane_200x50_scrollback_10k", capture_pane_200x50_scrollback_10k),
    ("list_windows_20", list_windows_20),
    ("resize_pane_right_1", resize_pane_right_1),
    ("kill_session", kill_session),
    ("list_commands", list_commands),
    ("capture_pane_5000_lines", capture_pane_5000_lines),
    ("new_window_detached_sh", new_window_detached_sh),
    ("resize_pane_absolute_100x30", resize_pane_absolute_100x30),
    ("new_session_cold_sh", new_session_cold_sh),
    ("split_window_h_attached_sh", split_window_h_attached_sh),
    ("split_window_v_attached_sh", split_window_v_attached_sh),
    ("resize_pane_left_1", resize_pane_left_1),
    ("resize_pane_right_10", resize_pane_right_10),
    ("resize_pane_absolute_200x50", resize_pane_absolute_200x50),
    ("resize_pane_storm_200", resize_pane_storm_200),
    ("display_message_default", display_message_default),
    ("show_options_global", show_options_global),
    ("show_window_options", show_window_options),
    ("list_panes_80", list_panes_80),
    ("rename_window", rename_window),
    ("select_window_next", select_window_next),
    ("join_pane_detached", join_pane_detached),
    ("source_file_minimal", source_file_minimal),
    ("set_option_quiet", set_option_quiet),
    ("set_window_option_quiet", set_window_option_quiet),
    ("kill_pane", kill_pane),
    ("kill_server", kill_server),
]


def tmux_like_operations(adapter: Adapter, iterations: int) -> dict[str, MeasuredOperation]:
    operations = {
        label: (lambda operation=operation: operation(adapter)) for label, operation in OPERATIONS
    }
    operations["list_windows_20"] = batched_list_windows_20(adapter, iterations)
    operations["list_panes_80"] = batched_list_panes_80(adapter, iterations)
    return operations


def zellij_operations(executable: str, iterations: int) -> dict[str, MeasuredOperation]:
    return {
        "new_session_cold_sh": lambda: zellij_new_session_cold(executable),
        "list_sessions_default": lambda: zellij_list_sessions(executable),
        "new_window_detached_sh": lambda: zellij_new_window(executable),
        "send_keys_detached_round_trip": lambda: zellij_send_keys(executable),
        "capture_pane_80x24": lambda: zellij_capture_visible(executable),
        "list_windows_20": batched_zellij_list_windows(executable, iterations),
        "kill_session": lambda: zellij_kill_session(executable),
        "split_window_h_detached_sh": lambda: zellij_split_right(executable),
        "split_window_v_detached_sh": lambda: zellij_split_down(executable),
        "list_panes_80": batched_zellij_list_panes(executable, iterations),
    }


def screen_operations(executable: str, iterations: int) -> dict[str, MeasuredOperation]:
    """GNU screen equivalents for the eight operations it can express.

    Splits, resizes and per-pane listing have no strict screen equivalent, so
    those rows stay absent rather than being approximated.
    """
    return {
        "new_session_cold_sh": lambda: screen_new_session_cold(executable),
        "list_sessions_default": lambda: screen_list_sessions(executable),
        "new_window_detached_sh": lambda: screen_new_window(executable),
        "send_keys_detached_round_trip": lambda: screen_send_keys(executable),
        "capture_pane_80x24": lambda: screen_capture_visible(executable),
        "capture_pane_200x50_scrollback_10k": lambda: screen_capture_scrollback(executable),
        "list_windows_20": batched_screen_list_windows(executable, iterations),
        "kill_session": lambda: screen_kill_session(executable),
    }


def temp_path_pattern() -> re.Pattern[str]:
    r"""Match this host's temporary directory, with or without a path under it.

    Both the configured and the fully resolved root are matched, longest first,
    so a resolved prefix such as macOS's `/private` cannot be left behind.

    A backslash in the root is matched written plainly or written escaped. The
    note a failure records quotes the command through `repr`, which doubles
    every backslash, so a root spelled with them reaches the note as
    `C:\\Users\\...`; matching only the plain spelling would publish the very
    location this redaction exists to remove. A root spelled without them, so
    every Unix root, is escaped exactly as before and matches exactly as
    before.

    The root counts only on a path-component boundary. A neighbour that merely
    starts with it, `/tmp-not-a-child` for `/tmp`, and a directory that merely
    ends with it, `/var/tmp`, are neither the root nor under it; reducing them
    would delete provenance the reader needs and announce a redaction that
    never happened.
    """
    character = r"[^\s'\",;)\]]"
    escapable = r"\\{1,2}"
    root = tempfile.gettempdir()
    roots = sorted({root, str(Path(root).resolve())}, key=len, reverse=True)
    alternatives = "|".join(
        escapable.join(re.escape(part) for part in candidate.split("\\"))
        for candidate in roots
    )
    return re.compile(
        rf"(?<![\w.~/\\-])(?:{alternatives})(?:[/\\]{character}*)?(?!{character})"
    )


TEMP_PATH = temp_path_pattern()


def sanitize(text: str) -> str:
    """Replace absolute temporary paths with a placeholder.

    A failure note quotes the command that failed, and the capture operations
    hand that command a private temporary file. The published artifact keeps
    which tool failed on which operation, never the host location that held
    the capture.
    """
    return TEMP_PATH.sub("<temp>", text)


def failure_details(error: BaseException) -> dict[str, Any]:
    """Keep the operation's cause and any additional cleanup cause separately."""
    details: dict[str, Any] = {"error_type": type(error).__name__,
                               "error": sanitize(str(error))}
    if isinstance(error, subprocess.CalledProcessError):
        details["returncode"] = error.returncode
    if isinstance(error, subprocess.TimeoutExpired):
        details["timeout_s"] = error.timeout
    for field in ("stdout", "stderr"):
        value = getattr(error, field, None)
        if value is not None:
            details[field] = sanitize(value.decode(errors="replace")
                                      if isinstance(value, bytes) else value)
    if isinstance(error, FixtureCleanupError):
        details["cleanup"] = error.details
    cleanup_error = getattr(error, "cleanup_error", None)
    if cleanup_error is not None:
        details["cleanup_error"] = failure_details(cleanup_error)
    return details


def measure(
    operation: MeasuredOperation,
    iterations: int,
    *,
    label: str,
    tool: str,
    progress_enabled: bool,
    sample_progress: bool,
    attempts: list[dict[str, Any]] | None = None,
    identity: dict[str, Any] | None = None,
    checkpoint: Callable[[], None] | None = None,
) -> tuple[dict[str, object] | None, str | None]:
    samples: list[float] = []
    attempts = attempts if attempts is not None else []
    run_id = uuid.uuid4().hex
    if identity is not None and iterations != 1:
        raise qualification.QualificationError("one planned identity permits exactly one attempt")
    for index in range(iterations):
        attempt = dict(identity) if identity is not None else {
            "run_id": run_id, "tool": tool, "operation": label, "pair": index + 1,
            "order": 0, "sequence": index + 1, "attempt_number": 1,
            "attempt_id": f"{run_id}/{label}/{index + 1}/0/{tool}"}
        attempt.update(sample=attempt["pair"], status="running", started_ns=time.monotonic_ns())
        if any(row.get("attempt_id") == attempt["attempt_id"] for row in attempts):
            raise qualification.QualificationError("retry/duplicate attempt refused before launch")
        attempts.append(attempt)
        try:
            if checkpoint is not None:
                checkpoint()  # Durable intent precedes any operation/setup launch.
            if sample_progress:
                progress(progress_enabled, f"  {tool} {label} sample {index + 1}/{iterations}")
            samples.append(qualification.finish_attempt(attempt, operation()))
            attempt["ended_ns"] = time.monotonic_ns()
            if checkpoint is not None:
                checkpoint()
        except BaseException as error:
            attempt.update(status="failed" if isinstance(error, Exception) else "interrupted",
                           ended_ns=time.monotonic_ns(), **failure_details(error))
            if checkpoint is not None:
                checkpoint()
            if not isinstance(error, Exception):
                raise
            return None, f"{tool}/{label}: {type(error).__name__}: {error}"
    return stats(samples), None


def relative_or_string(path: Path | None) -> str | None:
    if path is None:
        return None
    resolved = path.resolve()
    # A temporary root inside the checkout still takes privacy precedence.
    public_path = sanitize(str(resolved))
    if public_path != str(resolved):
        return public_path
    try:
        return str(resolved.relative_to(ROOT))
    except ValueError:
        return public_path


def collect(
    out: Path,
    iterations: int,
    rmux: Path,
    *,
    rmux_layout: str,
    rmux_public_binary: Path | None,
    rmux_helper_binary: Path | None,
    rmux_daemon_binary: Path | None,
    progress_enabled: bool,
    sample_progress: bool,
    only_operations: set[str] | None,
    selected_tools: set[str],
    tool_provenance: Path | None = None,
) -> None:
    qualification.verify_source(Path(__file__).read_text(), OPERATIONS)
    unknown_tools = selected_tools.difference(TOOLS)
    if unknown_tools:
        raise SystemExit(f"unknown --only-tools value(s): {', '.join(sorted(unknown_tools))}")

    operations_by_tool: dict[str, dict[str, MeasuredOperation]] = {}
    rmux_executable = Path(os.path.abspath(rmux))
    if "rmux" in selected_tools:
        operations_by_tool["rmux"] = tmux_like_operations(Adapter("rmux", rmux_executable), iterations)
    tmux = shutil.which("tmux")
    if darwin_driver is not None and "tmux" in selected_tools and tmux is None:
        raise FileNotFoundError("selected native tmux executable is unavailable")
    if tmux and "tmux" in selected_tools:
        operations_by_tool["tmux"] = tmux_like_operations(Adapter("tmux", tmux), iterations)
    zellij = shutil.which("zellij")
    if zellij and "zellij" in selected_tools:
        operations_by_tool["zellij"] = zellij_operations(zellij, iterations)
    screen = shutil.which("screen")
    if screen and "screen" in selected_tools:
        operations_by_tool["screen"] = screen_operations(screen, iterations)
    if not operations_by_tool:
        raise SystemExit("no selected benchmark tools are available")

    known_operations = {label for label, _ in OPERATIONS}
    unknown_operations = (only_operations or set()).difference(known_operations)
    if unknown_operations:
        raise SystemExit(f"unknown operation(s): {', '.join(sorted(unknown_operations))}")
    operation_specs = [
        (label, operation)
        for label, operation in OPERATIONS
        if only_operations is None or label in only_operations
    ]
    if not operation_specs:
        raise SystemExit("no benchmark operations selected")

    platform_id = "macos" if platform.system().lower() == "darwin" else "linux"
    rows = [{"id": label, "label": label, "metrics": {}} for label, _ in operation_specs]
    rows_by_label = {row["id"]: row for row in rows}
    errors: list[str] = []
    attempts: list[dict[str, Any]] = []
    generated_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    measured_tools = list(operations_by_tool)
    population = qualification.population()
    for tool, operations in operations_by_tool.items():
        if set(operations) != set(population["tools"][tool]):
            raise qualification.QualificationError(f"{tool} operation population changed")
    run_id = uuid.uuid4().hex
    expected = qualification.schedule(run_id, [label for label, _ in operation_specs],
                                      measured_tools, iterations)
    plan = {"run_id": run_id, "iterations": iterations,
            "operations": [label for label, _ in operation_specs], "tools": measured_tools,
            "expected": expected, "expected_sha256": qualification.digest(expected)}
    declarations = {
        tool: {"declared": str(rmux) if tool == "rmux" else tool,
               "selected": {"rmux": str(rmux_executable), "tmux": tmux, "zellij": zellij, "screen": screen}[tool],
               "lookup": "path" if tool == "rmux" else "PATH"}
        for tool in TOOLS if tool in selected_tools}
    if "rmux" in selected_tools:
        for role, path in (("rmux_public", rmux_public_binary), ("rmux_helper", rmux_helper_binary),
                           ("rmux_daemon", rmux_daemon_binary)):
            if path is not None or role == "rmux_daemon":
                declarations[role] = {"declared": str(path) if path is not None else None,
                                      "selected": str(path) if path is not None else None, "lookup": "path"}
    identities = tool_identity.ToolIdentity(run_id, out, declarations,
                                            [tool for tool in TOOLS if tool in selected_tools], tool_provenance)
    versions = {}
    git_identity = {"branch": git("rev-parse", "--abbrev-ref", "HEAD"), "commit": git("rev-parse", "HEAD")}
    cancellation = []
    sampler_errors = []
    accounting_errors = []
    sampling_finished = False
    cleanup_finished = False
    population_digest = hashlib.sha256(qualification.population_bytes()).hexdigest()

    def payload(complete: bool) -> dict[str, Any]:
        attempted_ids = {attempt.get("attempt_id") for attempt in attempts}
        return {
            "schema": 1,
            "kind": "rmux-public-benchmark",
            "complete": complete,
            "driver": "darwin-posix-spawn-kqueue-waitpid" if darwin_driver else "subprocess",
            "generated_at": generated_at,
            "platform": {"id": platform_id, "name": "macOS" if platform_id == "macos" else "Linux"},
            "git": git_identity,
            "tools": measured_tools,
            "tool_versions": versions,
            "tool_environments": {tool: "native" for tool in measured_tools},
            "baseline": "tmux" if "tmux" in measured_tools else measured_tools[0],
            "units": "ms",
            "lower_is_better": True,
            "rmux_layout": rmux_layout,
            "rmux_binaries": {
                "client": str(rmux),
                "public": str(rmux_public_binary if rmux_public_binary is not None else rmux),
                "helper": str(rmux_helper_binary) if rmux_helper_binary is not None else None,
                "daemon": str(rmux_daemon_binary) if rmux_daemon_binary is not None else None,
            },
            "notes": errors,
            "attempts": attempts,
            "operations": rows,
            "qualification_scope": "R5-B accounting and optional R6 declared disk identities only; no transient helper, functional, thermal or statistical qualification",
            "tool_identity": identities.state(),
            "population_sha256": population_digest,
            "sampling_plan": plan,
            "requested_tools": [tool for tool in TOOLS if tool in selected_tools],
            "unavailable_tools": [tool for tool in TOOLS if tool in selected_tools and tool not in measured_tools],
            "unattempted": [item["attempt_id"] for item in expected
                             if item["attempt_id"] not in attempted_ids],
            "sampler_cleanup": cancellation,
            "sampler_errors": sampler_errors,
            "accounting_errors": accounting_errors,
            "sampling_finished": sampling_finished,
            "cleanup_finished": cleanup_finished,
        }

    progress(progress_enabled, f"writing incremental results to {out}")
    progress(
        progress_enabled,
        f"selected tools={','.join(operations_by_tool)} operations={len(operation_specs)} iterations={iterations}",
    )
    write_json_atomic(out, payload(complete=False))

    primary = None
    samples_by_operation: dict[tuple[str, str], list[float]] = {}
    try:
        identities.prepare(version)
        versions = {tool: record["version"].get("value")
                    for tool, record in identities.manifest["tools"].items() if tool in TOOLS}
        write_json_atomic(out, payload(complete=False))
        for identity in expected:
            tool, label = identity["tool"], identity["operation"]
            operation = operations_by_tool[tool][label]
            if identities.required:
                identities.check("before_attempt:" + identity["attempt_id"])
            progress(progress_enabled, f"  {label} pair {identity['pair']}/{iterations} {tool}")
            result, error = measure(
                operation,
                1,
                label=label,
                tool=tool,
                progress_enabled=progress_enabled,
                sample_progress=sample_progress,
                attempts=attempts,
                identity=identity,
                checkpoint=lambda: write_json_atomic(out, payload(complete=False)),
            )
            if result is not None:
                samples = samples_by_operation.setdefault((label, tool), [])
                samples.append(attempts[-1]["elapsed_ms"])
                if len(samples) == iterations:
                    rows_by_label[label]["metrics"][tool] = stats(samples)
            else:
                message = sanitize(error or f"{tool}/{label}: failed")
                raise qualification.QualificationError(message)
            write_json_atomic(out, payload(complete=False))
        sampling_finished = True
    except BaseException as error:
        primary = error
        sampler_errors.append(failure_details(error))
        errors.append(sanitize(f"{type(error).__name__}: {error}"))
    finally:
        # A paired batch can be open in the other tool when sampling stops.
        # Delegate retirement to its existing cleanup, never call it for another sample.
        for tool, operations in operations_by_tool.items():
            for label, operation in operations.items():
                cancel = getattr(operation, "cancel", None)
                if cancel is None:
                    continue
                event = {"tool": tool, "operation": label, "phase": "sampler_cleanup"}
                cancellation.append(event)
                try:
                    cancel()
                    event["status"] = "pass"
                except BaseException as error:
                    event.update(status="failed", **failure_details(error))
                    errors.append(sanitize(f"{tool}/{label} sampler cleanup: {error}"))
        cleanup_finished = True
        try:
            identities.finish()
        except Exception as error:
            sampler_errors.append(failure_details(error))
            errors.append(sanitize(f"tool identity final check: {error}"))
        accounting_errors = qualification.validate_artifact(payload(complete=False))
        write_json_atomic(out, payload(complete=primary is None and not accounting_errors))
    if primary is not None:
        raise primary
    if accounting_errors:
        raise qualification.QualificationError("; ".join(accounting_errors))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=15)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--rmux-layout", default="custom-binary")
    parser.add_argument("--rmux-public-binary", type=Path)
    parser.add_argument("--rmux-helper-binary", type=Path)
    parser.add_argument("--rmux-daemon-binary", type=Path)
    parser.add_argument("--tool-provenance", type=Path, help="require declared tool identity using pinned producer receipts")
    parser.add_argument("--quiet", action="store_true", help="suppress progress output")
    parser.add_argument("--sample-progress", action="store_true", help="print every measured sample")
    parser.add_argument("--only-operations", help="comma-separated operation IDs to run")
    parser.add_argument(
        "--only-tools",
        default="rmux,tmux,zellij,screen",
        help="comma-separated tools to run",
    )
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be a positive integer")
    if not args.binary.exists():
        parser.error(f"rmux binary not found: {args.binary}")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    collect(
        args.out,
        args.iterations,
        args.binary,
        rmux_layout=args.rmux_layout,
        rmux_public_binary=args.rmux_public_binary,
        rmux_helper_binary=args.rmux_helper_binary,
        rmux_daemon_binary=args.rmux_daemon_binary,
        progress_enabled=not args.quiet,
        sample_progress=args.sample_progress,
        only_operations=comma_set(args.only_operations),
        selected_tools=comma_set(args.only_tools) or set(TOOLS),
        tool_provenance=args.tool_provenance,
    )
    print(args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
