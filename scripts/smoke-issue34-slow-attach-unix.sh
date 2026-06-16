#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
PROFILE="${RMUX_PROFILE:-release}"
case "$PROFILE" in
    debug)
        PROFILE_FLAG=()
        ;;
    release)
        PROFILE_FLAG=(--release)
        ;;
    *)
        printf '[issue34-slow-attach] ERROR: unsupported RMUX_PROFILE: %s\n' "$PROFILE" >&2
        exit 1
        ;;
esac
RMUX="${RMUX_BIN:-$TARGET_DIR/$PROFILE/rmux}"
SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-issue34-slow-attach.XXXXXX")"
export RMUX_TMPDIR="$SMOKE_ROOT/socket"
mkdir -p "$RMUX_TMPDIR"
export TERM="${TERM:-xterm-256color}"

log() {
    printf '[issue34-slow-attach] %s\n' "$*"
}

fail() {
    printf '[issue34-slow-attach] ERROR: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [[ -x "$RMUX" ]]; then
        "$RMUX" kill-server >/dev/null 2>&1 || true
    fi
    rm -rf "$SMOKE_ROOT"
}
trap cleanup EXIT

command -v python3 >/dev/null 2>&1 || fail 'python3 is required for issue #34 slow attach smoke'

cd "$ROOT"

if [[ -z "${RMUX_BIN:-}" ]]; then
    log "cargo build --locked ${PROFILE_FLAG[*]}"
    cargo build --locked "${PROFILE_FLAG[@]}"
fi

[[ -x "$RMUX" ]] || fail "rmux binary not found or not executable: $RMUX"

log "using $RMUX"
python3 - \
    "$RMUX" \
    "$SMOKE_ROOT" \
    "${RMUX_ISSUE34_SLOW_ATTACH_MAX_DELAY_MS:-${RMUX_ISSUE34_MAX_MS:-10000}}" \
    "${RMUX_ISSUE34_SLOW_IPC_MAX_MS:-750}" \
    "${RMUX_ISSUE34_SLOW_FRAMES:-5000}" \
    "${RMUX_ISSUE34_SLOW_READ_BYTES:-256}" \
    "${RMUX_ISSUE34_SLOW_READ_DELAY_MS:-50}" \
    "${RMUX_ISSUE34_SLOW_MAX_ATTACH_BYTES:-262144}" <<'PY'
import os
import pty
import select
import shlex
import signal
import fcntl
import struct
import subprocess
import sys
import threading
import time
import termios
from pathlib import Path

rmux = sys.argv[1]
root = Path(sys.argv[2])
max_attach_delay_ms = int(sys.argv[3])
max_ipc_ms = int(sys.argv[4])
ipc_timeout = max(1.0, (max_ipc_ms / 1000.0) + 0.5)
frames = int(sys.argv[5])
read_size = int(sys.argv[6])
read_delay = int(sys.argv[7]) / 1000.0
max_attach_bytes = int(sys.argv[8])
session = "issue34slow"
marker = "RMUX_ISSUE34_SLOW_ATTACH_DONE"
marker_bytes = marker.encode()
attach_cols = int(os.environ.get("RMUX_ISSUE34_SLOW_ATTACH_COLS", "80"))
attach_rows = int(os.environ.get("RMUX_ISSUE34_SLOW_ATTACH_ROWS", "24"))
env = os.environ.copy()
env["HOME"] = str(root / "home")
env["COLUMNS"] = str(attach_cols)
env["LINES"] = str(attach_rows)
(root / "home").mkdir(parents=True, exist_ok=True)


def set_pty_size(fd):
    winsize = struct.pack("HHHH", attach_rows, attach_cols, 0, 0)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)


def run(args, timeout=5.0):
    completed = subprocess.run(
        args,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr.decode(errors="replace"))
        raise SystemExit(f"{args!r} exited with {completed.returncode}")
    return completed


def time_cmd(label, args, timeout=None):
    if timeout is None:
        timeout = ipc_timeout
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            args,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(f"{label} exceeded {int(timeout * 1000)} ms") from exc
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr.decode(errors="replace"))
        raise SystemExit(f"{label} exited with {completed.returncode}")
    if elapsed_ms > max_ipc_ms:
        raise SystemExit(f"{label} exceeded threshold {max_ipc_ms} ms: {elapsed_ms} ms")
    return elapsed_ms, completed.stdout


def wait_until(label, predicate, timeout):
    deadline = time.perf_counter() + timeout
    while time.perf_counter() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise SystemExit(f"timed out waiting for {label}")


heavy_script = root / "issue34-slow-attach-output.py"
done_file = root / "producer.done"
heavy_script.write_text(
    f"""
import pathlib
import sys
import time

done_file = pathlib.Path(sys.argv[1])
frames = int(sys.argv[2])
marker = sys.argv[3]

for frame in range(frames):
    sys.stdout.write("\\x1b[H")
    for row in range(24):
        color = (frame + row) % 256
        sys.stdout.write(
            f"\\x1b[38;5;{{color}}mframe={{frame:05d}} row={{row:02d}} "
            "abcdefghijklmnopqrstuvwxyz 0123456789 !@#$%^&*()"
            "\\x1b[0m\\r\\n"
        )
sys.stdout.write("\\x1b[H")
sys.stdout.write(marker + "\\r\\n")
sys.stdout.write("final frame visible after finite noisy output\\r\\n")
sys.stdout.flush()
done_file.write_text(str(time.monotonic()), encoding="utf-8")
time.sleep(0.5)
""".strip(),
    encoding="utf-8",
)


class SlowAttachDrain:
    def __init__(self):
        self.pid = None
        self.fd = None
        self.thread = None
        self.stop = threading.Event()
        self.bytes_read = 0
        self.marker_seen_at = None
        self.marker_byte_offset = None
        self.tail = bytearray()

    def start(self):
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(rmux, [rmux, "attach-session", "-t", session], env)
        self.pid = pid
        self.fd = fd
        set_pty_size(fd)
        self.thread = threading.Thread(target=self._drain, name="issue34-slow-attach-drain")
        self.thread.start()

    def _drain(self):
        assert self.fd is not None
        while not self.stop.is_set():
            readable, _, _ = select.select([self.fd], [], [], 0.1)
            if self.fd not in readable:
                continue
            try:
                data = os.read(self.fd, read_size)
            except OSError:
                return
            if not data:
                return
            self.bytes_read += len(data)
            self.tail.extend(data)
            if len(self.tail) > 4096:
                del self.tail[:-4096]
            if self.marker_seen_at is None and marker_bytes in self.tail:
                self.marker_seen_at = time.perf_counter()
                self.marker_byte_offset = self.bytes_read
            time.sleep(read_delay)

    def wait_for_initial_bytes(self):
        wait_until("initial attach output", lambda: self.bytes_read > 0, 5.0)

    def close(self):
        self.stop.set()
        if self.fd is not None:
            try:
                os.write(self.fd, b"\x02d")
            except OSError:
                pass
        if self.thread is not None:
            self.thread.join(timeout=2)
        if self.fd is not None:
            try:
                os.close(self.fd)
            except OSError:
                pass
        if self.pid is not None:
            try:
                os.kill(self.pid, signal.SIGTERM)
            except OSError:
                pass
            try:
                os.waitpid(self.pid, os.WNOHANG)
            except (ChildProcessError, OSError):
                pass


def capture_has_marker():
    elapsed, stdout = time_cmd(
        "capture-final-marker",
        [rmux, "capture-pane", "-p", "-t", f"{session}:0.0"],
    )
    observed_ipc.append(elapsed)
    return marker in stdout.decode(errors="replace")


attach = SlowAttachDrain()
observed_ipc = []
try:
    print(run([rmux, "-V"]).stdout.decode().strip())
    run([rmux, "new-session", "-d", "-s", session, "/bin/sh"])
    elapsed, _ = time_cmd("baseline-list", [rmux, "list-panes", "-t", session])
    observed_ipc.append(elapsed)
    attach.start()
    attach.wait_for_initial_bytes()

    command = " ".join(
        [
            "python3",
            shlex.quote(str(heavy_script)),
            shlex.quote(str(done_file)),
            str(frames),
            shlex.quote(marker),
        ]
    )
    run([rmux, "send-keys", "-t", f"{session}:0.0", command, "Enter"])

    while not done_file.exists():
        elapsed, _ = time_cmd("load-list", [rmux, "list-panes", "-t", session])
        observed_ipc.append(elapsed)
        time.sleep(0.1)

    done_at = time.perf_counter()
    wait_until("final marker in capture-pane", capture_has_marker, 2.0)
    wait_until("final marker in slow attach output", lambda: attach.marker_seen_at, 15.0)

    attach_delay_ms = int((attach.marker_seen_at - done_at) * 1000)
    if attach_delay_ms > max_attach_delay_ms:
        raise SystemExit(
            f"slow attach marker exceeded threshold {max_attach_delay_ms} ms: "
            f"{attach_delay_ms} ms"
        )
    if attach.marker_byte_offset and attach.marker_byte_offset > max_attach_bytes:
        raise SystemExit(
            f"slow attach marker arrived after {attach.marker_byte_offset} bytes, "
            f"threshold {max_attach_bytes}"
        )
    print(
        "slow_attach_delay_ms="
        f"{attach_delay_ms} max_ipc_ms={max(observed_ipc)} "
        f"attach_marker_bytes={attach.marker_byte_offset} total_attach_bytes={attach.bytes_read}"
    )
finally:
    attach.close()
    subprocess.run([rmux, "kill-server"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
PY

log 'issue #34 slow attach smoke passed'
