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
        printf '[issue34-smoke] ERROR: unsupported RMUX_PROFILE: %s\n' "$PROFILE" >&2
        exit 1
        ;;
esac
RMUX="${RMUX_BIN:-$TARGET_DIR/$PROFILE/rmux}"
SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-issue34-smoke.XXXXXX")"
export RMUX_TMPDIR="$SMOKE_ROOT/socket"
mkdir -p "$RMUX_TMPDIR"
export TERM="${TERM:-xterm-256color}"

log() {
    printf '[issue34-smoke] %s\n' "$*"
}

fail() {
    printf '[issue34-smoke] ERROR: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [[ -x "$RMUX" ]]; then
        "$RMUX" kill-server >/dev/null 2>&1 || true
    fi
    rm -rf "$SMOKE_ROOT"
}
trap cleanup EXIT

command -v python3 >/dev/null 2>&1 || fail 'python3 is required for issue #34 smoke'

cd "$ROOT"

if [[ -z "${RMUX_BIN:-}" ]]; then
    log "cargo build --locked ${PROFILE_FLAG[*]}"
    cargo build --locked "${PROFILE_FLAG[@]}"
fi

[[ -x "$RMUX" ]] || fail "rmux binary not found or not executable: $RMUX"

log "using $RMUX"
python3 - "$RMUX" "$SMOKE_ROOT" "${RMUX_ISSUE34_MAX_MS:-750}" "${RMUX_ISSUE34_ITERATIONS:-18}" <<'PY'
import os
import pty
import select
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

rmux = sys.argv[1]
root = Path(sys.argv[2])
max_ms = int(sys.argv[3])
iterations = int(sys.argv[4])
command_timeout = max(1.0, (max_ms / 1000.0) + 0.5)
session = "issue34"
env = os.environ.copy()
env["HOME"] = str(root / "home")
(root / "home").mkdir(parents=True, exist_ok=True)


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


def time_cmd(label, args):
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            args,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=command_timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(f"{label} exceeded {int(command_timeout * 1000)} ms") from exc
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr.decode(errors="replace"))
        raise SystemExit(f"{label} exited with {completed.returncode}")
    print(f"{label} ms={elapsed_ms}")
    if elapsed_ms > max_ms:
        raise SystemExit(f"{label} exceeded threshold {max_ms} ms: {elapsed_ms} ms")
    return elapsed_ms


heavy_script = root / "issue34-heavy-output.py"
heavy_script.write_text(
    """
import sys
import time

frame = []
for index in range(8192):
    frame.append(
        f"\\x1b[38;5;{index % 256}m{index:04d} "
        "abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 "
        "\\x1b[0m\\r\\n"
    )
payload = "".join(frame)
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    sys.stdout.write(payload)
    sys.stdout.flush()
""".strip(),
    encoding="utf-8",
)


class AttachDrain:
    def __init__(self):
        self.pid = None
        self.fd = None
        self.thread = None
        self.stop = threading.Event()
        self.bytes_read = 0

    def start(self):
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(rmux, [rmux, "attach-session", "-t", session], env)
        self.pid = pid
        self.fd = fd
        self.thread = threading.Thread(target=self._drain, name="issue34-attach-drain")
        self.thread.start()

    def _drain(self):
        assert self.fd is not None
        while not self.stop.is_set():
            readable, _, _ = select.select([self.fd], [], [], 0.1)
            if self.fd not in readable:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.bytes_read += len(data)

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


attach = AttachDrain()
try:
    print(run([rmux, "-V"]).stdout.decode().strip())
    run(
        [
            rmux,
            "new-session",
            "-d",
            "-s",
            session,
            "/bin/sh",
            "-c",
            "while true; do printf 'quiet-%s\\n' \"$(date +%s)\"; sleep 0.2; done",
        ]
    )
    time_cmd("baseline-list", [rmux, "list-panes", "-t", session])
    time_cmd("baseline-capture", [rmux, "capture-pane", "-p", "-t", f"{session}:0.0"])
    run([rmux, "split-window", "-t", session, "python3", str(heavy_script)])
    run([rmux, "select-pane", "-t", f"{session}:0.1"])
    attach.start()
    time.sleep(0.5)
    observed = []
    for index in range(1, iterations + 1):
        observed.append(time_cmd(f"heavy-list-{index}", [rmux, "list-panes", "-t", session]))
        observed.append(
            time_cmd(
                f"heavy-capture-quiet-{index}",
                [rmux, "capture-pane", "-p", "-t", f"{session}:0.0"],
            )
        )
        time.sleep(0.15)
    if attach.bytes_read == 0:
        raise SystemExit("attach PTY did not receive heavy pane output")
    print(f"max_ms={max(observed)} attach_bytes={attach.bytes_read}")
finally:
    attach.close()
    subprocess.run([rmux, "kill-server"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
PY

log 'issue #34 responsiveness smoke passed'
