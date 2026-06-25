#!/usr/bin/env python3
"""Collect local Unix RMUX/tmux benchmark measurements as JSON."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any


ROOT = Path.cwd()
SHELL = "/bin/sh"
TOOLS = ["rmux", "tmux", "zellij"]


class Adapter:
    def __init__(self, tool: str, executable: str | Path) -> None:
        self.tool = tool
        self.executable = str(executable)

    def command(self, socket: str, args: list[str]) -> list[str]:
        return [self.executable, "-L", socket, *args]

    def cleanup(self, socket: str) -> None:
        quiet(self.command(socket, ["kill-server"]), check=False, timeout=5.0)


AdapterOperation = Callable[[Adapter], float]
MeasuredOperation = Callable[[], float]


def clean_env() -> dict[str, str]:
    env = dict(os.environ)
    for name in ("RMUX", "TMUX", "TERM_PROGRAM", "ZELLIJ"):
        env.pop(name, None)
    return env


def quiet(cmd: list[str], *, check: bool = True, timeout: float = 15.0) -> None:
    subprocess.run(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=clean_env(),
        timeout=timeout,
        check=check,
    )


def captured(cmd: list[str], *, timeout: float = 15.0) -> str:
    return subprocess.check_output(
        cmd,
        text=True,
        stderr=subprocess.DEVNULL,
        env=clean_env(),
        timeout=timeout,
    ).strip()


def timed(cmd: list[str], *, timeout: float = 15.0) -> float:
    start = time.perf_counter()
    quiet(cmd, timeout=timeout)
    return (time.perf_counter() - start) * 1000.0


def git(*args: str) -> str:
    try:
        return subprocess.check_output(
            ["git", *args], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except Exception:
        return "unknown"


def version(cmd: str, *args: str) -> str | None:
    path = shutil.which(cmd)
    if not path:
        return None
    try:
        return subprocess.check_output(
            [path, *args],
            text=True,
            stderr=subprocess.STDOUT,
            env=clean_env(),
            timeout=5.0,
        ).strip().splitlines()[0]
    except Exception:
        return "available"


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
) -> None:
    args = ["new-session", "-d", "-s", name]
    if width is not None:
        args.extend(["-x", str(width)])
    if height is not None:
        args.extend(["-y", str(height)])
    if command is not None:
        args.append(command)
    quiet(adapter.command(socket, args))


def output_command(lines: int) -> str:
    return f"{SHELL} -c '{output_script(lines)}'"


def output_script(lines: int) -> str:
    return (
        "i=0; "
        f"while [ $i -lt {lines} ]; do printf \"rmux-bench-%05d\\n\" \"$i\"; "
        "i=$((i+1)); done; sleep 60"
    )


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
) -> float:
    socket = socket_name(operation, adapter.tool)
    try:
        session(adapter, socket, command=command, width=width, height=height)
        if setup is not None:
            setup(socket)
        return timed(adapter.command(socket, timed_args), timeout=timeout)
    finally:
        adapter.cleanup(socket)


def wait_for_target(adapter: Adapter, socket: str, target: str) -> None:
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
    adapter.cleanup(socket)
    try:
        return timed(adapter.command(socket, ["new-session", "-d", "-s", "bench"]))
    finally:
        adapter.cleanup(socket)


def new_session_warm_sh(adapter: Adapter) -> float:
    socket = socket_name("new_session_warm_sh", adapter.tool)
    try:
        quiet(adapter.command(socket, ["start-server"]))
        return timed(adapter.command(socket, ["new-session", "-d", "-s", "bench"]))
    finally:
        adapter.cleanup(socket)


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


def list_sessions_default(adapter: Adapter) -> float:
    return with_session(adapter, "list_sessions_default", ["list-sessions"])


def capture_pane_5000_lines(adapter: Adapter) -> float:
    return with_session(
        adapter,
        "capture_pane_5000_lines",
        ["capture-pane", "-p", "-t", "bench"],
        command=output_command(5000),
        timeout=30.0,
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
            except Exception:
                adapter.cleanup(socket)
                socket = None
                raise
        try:
            return timed(adapter.command(socket, ["list-windows", "-t", "bench"]))
        finally:
            remaining -= 1
            if remaining <= 0 and socket is not None:
                adapter.cleanup(socket)
                socket = None

    return operation


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
            except Exception:
                adapter.cleanup(socket)
                socket = None
                raise
        try:
            return timed(adapter.command(socket, ["list-panes", "-a"]), timeout=30.0)
        finally:
            remaining -= 1
            if remaining <= 0 and socket is not None:
                adapter.cleanup(socket)
                socket = None

    return operation



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

    return operation


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

    return operation


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


def measure(
    operation: MeasuredOperation,
    iterations: int,
    *,
    label: str,
    tool: str,
    progress_enabled: bool,
    sample_progress: bool,
) -> tuple[dict[str, object] | None, str | None]:
    samples: list[float] = []
    for index in range(iterations):
        try:
            if sample_progress:
                progress(progress_enabled, f"  {tool} {label} sample {index + 1}/{iterations}")
            samples.append(operation())
        except Exception as error:
            return None, f"{tool}/{label}: {type(error).__name__}: {error}"
    return stats(samples), None


def relative_or_string(path: Path | None) -> str | None:
    if path is None:
        return None
    resolved = path.resolve()
    try:
        return str(resolved.relative_to(ROOT))
    except ValueError:
        return str(resolved)


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
) -> None:
    unknown_tools = selected_tools.difference(TOOLS)
    if unknown_tools:
        raise SystemExit(f"unknown --only-tools value(s): {', '.join(sorted(unknown_tools))}")

    operations_by_tool: dict[str, dict[str, MeasuredOperation]] = {}
    if "rmux" in selected_tools:
        operations_by_tool["rmux"] = tmux_like_operations(Adapter("rmux", rmux), iterations)
    tmux = shutil.which("tmux")
    if tmux and "tmux" in selected_tools:
        operations_by_tool["tmux"] = tmux_like_operations(Adapter("tmux", tmux), iterations)
    zellij = shutil.which("zellij")
    if zellij and "zellij" in selected_tools:
        operations_by_tool["zellij"] = zellij_operations(zellij, iterations)
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
    generated_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    measured_tools = list(operations_by_tool)

    def payload(complete: bool) -> dict[str, Any]:
        return {
            "schema": 1,
            "kind": "rmux-public-benchmark",
            "complete": complete,
            "generated_at": generated_at,
            "platform": {"id": platform_id, "name": "macOS" if platform_id == "macos" else "Linux"},
            "git": {"branch": git("rev-parse", "--abbrev-ref", "HEAD"), "commit": git("rev-parse", "HEAD")},
            "tools": measured_tools,
            "tool_versions": {
                "rmux": version(str(rmux), "-V"),
                "tmux": version("tmux", "-V"),
                "zellij": version("zellij", "--version"),
            },
            "baseline": "tmux" if "tmux" in measured_tools else measured_tools[0],
            "units": "ms",
            "lower_is_better": True,
            "rmux_layout": rmux_layout,
            "rmux_binaries": {
                "public": relative_or_string(rmux_public_binary or rmux),
                "helper": relative_or_string(rmux_helper_binary),
                "daemon": relative_or_string(rmux_daemon_binary),
            },
            "notes": errors,
            "operations": rows,
        }

    progress(progress_enabled, f"writing incremental results to {out}")
    progress(
        progress_enabled,
        f"selected tools={','.join(operations_by_tool)} operations={len(operation_specs)} iterations={iterations}",
    )
    write_json_atomic(out, payload(complete=False))

    for tool_index, (tool, operations) in enumerate(operations_by_tool.items(), start=1):
        progress(progress_enabled, f"tool {tool_index}/{len(operations_by_tool)} {tool}")
        for operation_index, (label, _) in enumerate(operation_specs, start=1):
            operation = operations.get(label)
            if operation is None:
                continue
            progress(
                progress_enabled,
                f"  {operation_index}/{len(operation_specs)} {label}",
            )
            result, error = measure(
                operation,
                iterations,
                label=label,
                tool=tool,
                progress_enabled=progress_enabled,
                sample_progress=sample_progress,
            )
            if result is not None:
                rows_by_label[label]["metrics"][tool] = result
                progress(
                    progress_enabled,
                    f"    done p50={result['p50_ms']:.3f}ms p95={result['p95_ms']:.3f}ms",
                )
            else:
                message = error or f"{tool}/{label}: failed"
                errors.append(message)
                progress(progress_enabled, f"    {message}")
            write_json_atomic(out, payload(complete=False))

    write_json_atomic(out, payload(complete=True))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=15)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--rmux-layout", default="custom-binary")
    parser.add_argument("--rmux-public-binary", type=Path)
    parser.add_argument("--rmux-helper-binary", type=Path)
    parser.add_argument("--rmux-daemon-binary", type=Path)
    parser.add_argument("--quiet", action="store_true", help="suppress progress output")
    parser.add_argument("--sample-progress", action="store_true", help="print every measured sample")
    parser.add_argument("--only-operations", help="comma-separated operation IDs to run")
    parser.add_argument("--only-tools", default="rmux,tmux,zellij", help="comma-separated tools to run")
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be a positive integer")
    if not args.binary.exists():
        parser.error(f"rmux binary not found: {args.binary}")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    collect(
        args.out,
        args.iterations,
        args.binary.resolve(),
        rmux_layout=args.rmux_layout,
        rmux_public_binary=args.rmux_public_binary,
        rmux_helper_binary=args.rmux_helper_binary,
        rmux_daemon_binary=args.rmux_daemon_binary,
        progress_enabled=not args.quiet,
        sample_progress=args.sample_progress,
        only_operations=comma_set(args.only_operations),
        selected_tools=comma_set(args.only_tools) or set(TOOLS),
    )
    print(args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
